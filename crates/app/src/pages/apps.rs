//! Uninstall: installed programs, their uninstallers and what they leave behind.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Check, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, Layout, ScrollArea, Sense, Ui};
use egui_phosphor::regular as icon;
use zest_engine::core::delete::DeleteOptions;
use zest_engine::core::fmt::{format_bytes, plural};
use zest_engine::core::reg::RegBackup;
use zest_engine::core::{Error, Progress};
use zest_engine::system::apps::{
    self, AppSource, InstalledApp, Leftover, LeftoverReport, ListOptions, UninstallOutcome,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Uninstall,
    RemoveEntry,
}

type UninstallResult = Result<(InstalledApp, UninstallOutcome, Vec<Leftover>), String>;

/// State of the Uninstall page.
pub struct AppsPage {
    loading: Job<Vec<InstalledApp>>,
    apps: Option<Vec<InstalledApp>>,
    search: String,
    sort: (usize, bool),
    selected: Option<String>,
    working: Job<UninstallResult>,
    working_on: String,
    leftovers: Option<(InstalledApp, Vec<Leftover>, Vec<bool>)>,
    removing: Job<(LeftoverReport, Option<String>)>,
    confirm: Option<(Pending, InstalledApp)>,
}

impl Default for AppsPage {
    fn default() -> Self {
        AppsPage {
            loading: Job::default(),
            apps: None,
            search: String::new(),
            sort: (0, true),
            selected: None,
            working: Job::default(),
            working_on: String::new(),
            leftovers: None,
            removing: Job::default(),
            confirm: None,
        }
    }
}

impl AppsPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.loading.is_running() || self.working.is_running() || self.removing.is_running()
    }

    /// Loads the list of installed programs (read-only).
    pub fn reload(&mut self, ctx: &egui::Context) {
        self.loading.start(ctx, "apps-list", |_| {
            apps::list_installed(&ListOptions::default())
        });
    }

    fn start_uninstall(&mut self, ctx: &egui::Context, app: InstalledApp) {
        self.working_on = app.name.clone();
        self.working
            .start(ctx, "uninstall", move |progress: Progress| {
                progress.set_message("Waiting for the uninstaller to finish");
                let outcome = apps::uninstall(&app, false, &progress).map_err(|e| match e {
                    Error::Cancelled => "The uninstall was cancelled.".to_owned(),
                    other => other.to_string(),
                })?;
                let leftovers = if outcome.entry_removed {
                    progress.set_message("Looking for leftovers");
                    apps::find_leftovers(&app, &progress)
                } else {
                    Vec::new()
                };
                Ok((app, outcome, leftovers))
            });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.loading.poll(&ctx) {
            match result {
                Ok(list) => self.apps = Some(list),
                Err(e) => {
                    self.apps = Some(Vec::new());
                    cx.toasts
                        .error(format!("Could not list installed programs: {e}"));
                }
            }
        }
        if self.apps.is_none() && !self.loading.is_running() {
            self.reload(&ctx);
        }
        if let Some(result) = self.working.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok((app, outcome, leftovers)) => {
                    if outcome.entry_removed {
                        if let Some(list) = &mut self.apps {
                            list.retain(|a| a.id != app.id);
                        }
                        self.selected = None;
                        let mut message = format!("{} was uninstalled.", app.name);
                        if outcome.reboot_required {
                            message.push_str(" Restart Windows to finish.");
                        }
                        cx.toasts.good(message);
                        if !leftovers.is_empty() {
                            let ticks = vec![true; leftovers.len()];
                            self.leftovers = Some((app, leftovers, ticks));
                        }
                    } else if outcome.wait_cancelled {
                        cx.toasts
                            .info("Stopped waiting. The uninstaller may still be open.");
                    } else {
                        cx.toasts.info(format!("{} is still installed.", app.name));
                    }
                }
                Err(e) => cx.toasts.error(e),
            }
        }
        if let Some(result) = self.removing.poll(&ctx) {
            match result {
                Ok((report, _)) => {
                    let mut message = format!(
                        "{} removed, {} freed.",
                        plural(report.removed as u64, "leftover"),
                        format_bytes(report.bytes_freed)
                    );
                    if !report.failures.is_empty() {
                        message
                            .push_str(&format!(" {} could not be removed.", report.failures.len()));
                    }
                    cx.toasts.good(message);
                }
                Err(e) => cx.toasts.error(format!("Removing leftovers failed: {e}")),
            }
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Uninstall",
                    "Remove programs with their own uninstaller, then clear out what they leave behind.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(!self.is_busy(), |ui| {
                    if widgets::icon_text_button(ui, &pal, icon::ARROWS_CLOCKWISE, "Refresh").clicked() {
                        self.reload(&ctx);
                    }
                });
            });
        });

        if self.working.is_running() {
            let message = self
                .working
                .progress()
                .map(|p| p.message())
                .unwrap_or_default();
            if widgets::banner(
                ui,
                &pal,
                Tone::Accent,
                icon::HOURGLASS,
                &format!(
                    "{}: {message}. Follow the uninstaller's own window.",
                    self.working_on
                ),
                Some("Stop waiting"),
            ) {
                self.working.cancel();
            }
            ui.add_space(6.0);
        }

        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - egui::vec2(2.0, 2.0));
            let Some(list) = &self.apps else {
                ui.vertical_centered(|ui| {
                    ui.add_space(90.0);
                    widgets::spinner(ui, &pal, 28.0);
                    ui.label(text::muted("Reading installed programs", &pal));
                });
                return;
            };
            let needle = self.search.trim().to_lowercase();
            let mut visible: Vec<&InstalledApp> = list
                .iter()
                .filter(|a| {
                    needle.is_empty()
                        || a.name.to_lowercase().contains(&needle)
                        || a.publisher.as_deref().unwrap_or("").to_lowercase().contains(&needle)
                })
                .collect();
            let (column, ascending) = self.sort;
            visible.sort_by(|a, b| {
                let order = match column {
                    1 => a.publisher.as_deref().unwrap_or("").to_lowercase().cmp(&b.publisher.as_deref().unwrap_or("").to_lowercase()),
                    3 => a.install_date.cmp(&b.install_date),
                    4 => a.size_bytes.unwrap_or(0).cmp(&b.size_bytes.unwrap_or(0)),
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                };
                if ascending { order } else { order.reverse() }
            });

            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        widgets::search_field(ui, &pal, &mut self.search, "Search programs", 280.0);
                        ui.add_space(6.0);
                        let total: u64 = visible.iter().filter_map(|a| a.size_bytes).sum();
                        ui.label(text::muted(
                            format!("{}, {} known size", plural(visible.len() as u64, "program"), format_bytes(total)),
                            &pal,
                        ));
                    });
                });

            let columns = Columns(vec![
                Col::Flex(1.6),
                Col::Flex(1.0),
                Col::Fixed(120.0),
                Col::Fixed(96.0),
                Col::Fixed(84.0),
            ]);
            widgets::list_header(
                ui,
                &pal,
                &columns,
                &[
                    ("Name", Align::Min, true),
                    ("Publisher", Align::Min, true),
                    ("Version", Align::Min, false),
                    ("Installed", Align::Min, true),
                    ("Size", Align::Max, true),
                ],
                &mut self.sort,
            );

            let selected_app = self
                .selected
                .as_ref()
                .and_then(|id| list.iter().find(|a| &a.id == id))
                .cloned();
            let footer = if selected_app.is_some() { 64.0 } else { 0.0 };
            let row_height = 38.0;
            let mut clicked: Option<String> = None;
            ScrollArea::vertical()
                .id_salt("apps")
                .auto_shrink([false, false])
                .max_height(ui.available_height() - footer)
                .show_rows(ui, row_height, visible.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for index in range {
                        let app = visible[index];
                        let is_selected = self.selected.as_deref() == Some(app.id.as_str());
                        let (row, response) = widgets::list_row(ui, &pal, row_height, is_selected, Sense::click());
                        let cells = columns.cells(row, 12.0);
                        widgets::paint_text(ui, cells[0], &app.name, text::body_font(14.0), pal.text, Align::Min);
                        widgets::paint_text(ui, cells[1], app.publisher.as_deref().unwrap_or(""), text::body_font(13.0), pal.text_muted, Align::Min);
                        widgets::paint_text(ui, cells[2], app.version.as_deref().unwrap_or(""), text::body_font(13.0), pal.text_muted, Align::Min);
                        let date = app.install_date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
                        widgets::paint_text(ui, cells[3], &date, text::body_font(13.0), pal.text_muted, Align::Min);
                        let size = app.size_bytes.map(format_bytes).unwrap_or_default();
                        widgets::paint_text(ui, cells[4], &size, text::body_font(13.0), pal.text_muted, Align::Max);
                        if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            clicked = Some(app.id.clone());
                        }
                    }
                });
            if let Some(id) = clicked {
                self.selected = if self.selected.as_ref() == Some(&id) { None } else { Some(id) };
            }

            if let Some(app) = selected_app {
                widgets::hairline(ui, &pal);
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(text::strong(&app.name));
                                ui.add_space(-6.0);
                                let source = match app.source {
                                    AppSource::Store => "Microsoft Store app".to_owned(),
                                    AppSource::Registry => app
                                        .install_location
                                        .as_ref()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|| "Desktop program".to_owned()),
                                };
                                ui.label(text::small(source, &pal));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_enabled_ui(!self.working.is_running(), |ui| {
                                    ui.add_enabled_ui(app.can_uninstall, |ui| {
                                        if widgets::primary_button(ui, &pal, "Uninstall").clicked() {
                                            self.confirm = Some((Pending::Uninstall, app.clone()));
                                        }
                                    });
                                    if app.can_repair && widgets::button(ui, &pal, "Repair").clicked() {
                                        let target = app.clone();
                                        self.working_on = app.name.clone();
                                        self.working.start(&ctx, "repair", move |progress: Progress| {
                                            progress.set_message("Waiting for the repair to finish");
                                            let outcome = apps::repair(&target, &progress).map_err(|e| e.to_string())?;
                                            Ok((target, outcome, Vec::new()))
                                        });
                                    }
                                    if app.key.is_some()
                                        && widgets::quiet_button(ui, &pal, "Remove from list")
                                            .on_hover_text("Deletes only the list entry, for programs that are already gone.")
                                            .clicked()
                                    {
                                        self.confirm = Some((Pending::RemoveEntry, app.clone()));
                                    }
                                });
                            });
                        });
                    });
            }
        });

        self.dialogs(&ctx, cx);
    }

    fn dialogs(&mut self, ctx: &egui::Context, cx: &mut Cx) {
        let pal = *cx.pal;
        if let Some((pending, app)) = self.confirm.clone() {
            let (title, body, label, danger) = match pending {
                Pending::Uninstall => (
                    format!("Uninstall {}?", app.name),
                    "Its own uninstaller will open. Zest waits for it and then looks for leftovers.".to_owned(),
                    "Uninstall",
                    false,
                ),
                Pending::RemoveEntry => (
                    format!("Remove {} from the list?", app.name),
                    "Only the entry in the installed-programs list is deleted, with a backup. Use this for programs that are already gone; it does not uninstall anything.".to_owned(),
                    "Remove entry",
                    true,
                ),
            };
            match widgets::confirm_dialog(ctx, &pal, "apps-confirm", &title, &body, label, danger) {
                Confirm::Yes => {
                    self.confirm = None;
                    match pending {
                        Pending::Uninstall => self.start_uninstall(ctx, app),
                        Pending::RemoveEntry => {
                            let mut backup = RegBackup::new("uninstall-entry");
                            match apps::remove_entry(&app, Some(&mut backup)) {
                                Ok(()) => {
                                    let _ = backup.save();
                                    if let Some(list) = &mut self.apps {
                                        list.retain(|a| a.id != app.id);
                                    }
                                    self.selected = None;
                                    cx.toasts.good("Entry removed. A backup was saved.");
                                }
                                Err(Error::NeedsElevation(_)) => cx
                                    .toasts
                                    .error("This entry applies to all users. Restart as administrator to remove it."),
                                Err(e) => cx.toasts.error(format!("Could not remove the entry: {e}")),
                            }
                        }
                    }
                }
                Confirm::No => self.confirm = None,
                Confirm::Pending => {}
            }
        }

        let mut close = false;
        let mut remove = false;
        if let Some((app, leftovers, ticks)) = &mut self.leftovers {
            let modal = egui::Modal::new(egui::Id::new("leftovers")).show(ctx, |ui| {
                ui.set_width(560.0);
                ui.label(text::section(format!("{} left these behind", app.name)));
                ui.label(text::muted(
                    "Only exact matches for this program are listed. Untick anything you want to keep.",
                    &pal,
                ));
                ui.add_space(6.0);
                ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for (leftover, tick) in leftovers.iter().zip(ticks.iter_mut()) {
                        ui.horizontal(|ui| {
                            let state = if *tick { Check::On } else { Check::Off };
                            if widgets::check(ui, &pal, state, &leftover.location()).clicked() {
                                *tick = !*tick;
                            }
                            ui.vertical(|ui| {
                                ui.add(egui::Label::new(text::body(leftover.location())).truncate());
                                ui.add_space(-6.0);
                                ui.label(text::small(
                                    format!("{}, {}", leftover.kind.label(), format_bytes(leftover.size_bytes)),
                                    &pal,
                                ));
                            });
                        });
                    }
                });
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let any = ticks.iter().any(|t| *t);
                    ui.add_enabled_ui(any, |ui| {
                        if widgets::primary_button(ui, &pal, "Remove selected").clicked() {
                            remove = true;
                        }
                    });
                    if widgets::button(ui, &pal, "Keep everything").clicked() {
                        close = true;
                    }
                });
            });
            if modal.should_close() {
                close = true;
            }
        }
        if remove && let Some((_, leftovers, ticks)) = self.leftovers.take() {
            let chosen: Vec<Leftover> = leftovers
                .into_iter()
                .zip(ticks)
                .filter(|(_, tick)| *tick)
                .map(|(leftover, _)| leftover)
                .collect();
            let options = if cx.settings.general.tools_use_recycle_bin {
                DeleteOptions::recycle()
            } else {
                DeleteOptions::permanent()
            };
            self.removing
                .start(ctx, "leftovers", move |progress: Progress| {
                    let mut backup = RegBackup::new("uninstall-leftovers");
                    let report =
                        apps::remove_leftovers(&chosen, &options, Some(&mut backup), &progress);
                    let saved = backup
                        .save()
                        .ok()
                        .flatten()
                        .map(|p| p.display().to_string());
                    (report, saved)
                });
        } else if close {
            self.leftovers = None;
        }
    }
}
