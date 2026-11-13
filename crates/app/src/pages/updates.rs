//! Software updates through the Windows Package Manager.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Check, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, Layout, ScrollArea, Sense, Ui};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use zest_engine::Settings;
use zest_engine::core::Progress;
use zest_engine::core::fmt::plural;
use zest_engine::updates::software::{
    self, CheckOptions, SoftwareUpdate, UpgradeOptions, UpgradeOutcome, UpgradeStatus,
};

/// State of the Software updates page.
#[derive(Default)]
pub struct UpdatesPage {
    available: Option<bool>,
    checking: Job<Result<Vec<SoftwareUpdate>, String>>,
    updates: Option<Vec<SoftwareUpdate>>,
    selected: HashSet<String>,
    upgrading: Job<Result<Vec<UpgradeOutcome>, String>>,
    outcomes: Vec<UpgradeOutcome>,
    confirm: bool,
}

impl UpdatesPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.checking.is_running() || self.upgrading.is_running()
    }

    /// Starts looking for updates (read-only; contacts the package sources).
    pub fn start_check(&mut self, ctx: &egui::Context, settings: &Settings) {
        if self.is_busy() {
            return;
        }
        self.outcomes.clear();
        let options = CheckOptions {
            ignore_ids: settings.general.ignored_updates.clone(),
            ..Default::default()
        };
        self.checking
            .start(ctx, "updates-check", move |progress: Progress| {
                software::check_updates(&options, &progress).map_err(|e| e.to_string())
            });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        let available = *self.available.get_or_insert_with(software::is_available);

        if let Some(result) = self.checking.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(updates) => {
                    self.selected = updates
                        .iter()
                        .filter(|u| u.can_upgrade() && !u.requires_explicit_upgrade)
                        .map(|u| u.id.clone())
                        .collect();
                    self.updates = Some(updates);
                }
                Err(e) => cx.toasts.error(format!("Could not check for updates: {e}")),
            }
        }
        if let Some(result) = self.upgrading.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(outcomes) => {
                    let done: HashSet<&str> = outcomes
                        .iter()
                        .filter(|o| o.is_success())
                        .map(|o| o.id.as_str())
                        .collect();
                    if let Some(updates) = &mut self.updates {
                        updates.retain(|u| !done.contains(u.id.as_str()));
                    }
                    self.selected.retain(|id| !done.contains(id.as_str()));
                    cx.toasts
                        .good(format!("{} updated", plural(done.len() as u64, "program")));
                    self.outcomes = outcomes;
                }
                Err(e) => cx.toasts.error(format!("Updating failed: {e}")),
            }
        }

        let selected = self.selected.len();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Software updates",
                    "Out-of-date programs are a common way in for malware. Updates come from each publisher's verified source.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(available && selected > 0 && !self.is_busy(), |ui| {
                    if widgets::primary_button(ui, &pal, "Update selected").clicked() {
                        self.confirm = true;
                    }
                });
                ui.add_enabled_ui(available && !self.is_busy(), |ui| {
                    let clicked = if selected > 0 {
                        widgets::button(ui, &pal, "Check for updates").clicked()
                    } else {
                        widgets::primary_button(ui, &pal, "Check for updates").clicked()
                    };
                    if clicked {
                        self.start_check(&ctx, cx.settings);
                    }
                });
            });
        });

        let mut ignore: Option<String> = None;
        let mut toggled: Option<String> = None;
        let mut toggle_all = false;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - egui::vec2(2.0, 2.0));
            if !available {
                if widgets::empty_state(
                    ui,
                    &pal,
                    icon::PACKAGE,
                    "Windows Package Manager is not installed",
                    "Software updates use the package manager that ships with Windows 10 and 11. Install “App Installer” from the Microsoft Store, then come back.",
                    Some("Open the Store page"),
                ) {
                    ctx.open_url(egui::OpenUrl::new_tab(software::INSTALL_URL));
                }
                return;
            }
            if self.is_busy() {
                let (verb, progress) = if self.upgrading.is_running() {
                    ("Updating", self.upgrading.progress())
                } else {
                    ("Looking for updates", self.checking.progress())
                };
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.3).clamp(40.0, 200.0));
                    ui.label(text::figure(format!("{verb}…"), 28.0));
                    ui.add_space(4.0);
                    ui.scope(|ui| {
                        ui.set_max_width(420.0);
                        widgets::progress_bar(ui, &pal, progress.and_then(|p| p.fraction()), 8.0);
                    });
                    let message = progress.map(|p| p.message()).unwrap_or_default();
                    ui.label(text::muted(if message.is_empty() { " ".to_owned() } else { message }, &pal));
                    if self.upgrading.is_running() {
                        ui.add_space(6.0);
                        if widgets::button(ui, &pal, "Stop after this one").clicked() {
                            self.upgrading.cancel();
                        }
                    }
                });
                return;
            }

            if !self.outcomes.is_empty() {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(text::section("Last run"));
                        for outcome in &self.outcomes {
                            ui.horizontal(|ui| {
                                let tone = match outcome.status {
                                    UpgradeStatus::Upgraded | UpgradeStatus::RebootRequired => Tone::Good,
                                    UpgradeStatus::Skipped | UpgradeStatus::Cancelled | UpgradeStatus::NoUpdateAvailable => Tone::Neutral,
                                    _ => Tone::Danger,
                                };
                                widgets::pill(ui, &pal, outcome.status.label(), tone);
                                ui.label(text::strong(&outcome.name));
                                ui.add(egui::Label::new(text::muted(&outcome.message, &pal)).truncate());
                            });
                        }
                    });
                widgets::hairline(ui, &pal);
            }

            let Some(updates) = &self.updates else {
                if widgets::empty_state(
                    ui,
                    &pal,
                    icon::ARROWS_CLOCKWISE,
                    "Check which programs have updates",
                    "The check compares your installed programs with the versions their publishers offer.",
                    Some("Check for updates"),
                ) {
                    self.start_check(&ctx, cx.settings);
                }
                return;
            };
            if updates.is_empty() {
                widgets::empty_state(
                    ui,
                    &pal,
                    icon::CHECK_CIRCLE,
                    "Everything is up to date",
                    "No newer versions were found for your installed programs.",
                    None,
                );
                return;
            }

            let selectable: Vec<&SoftwareUpdate> = updates.iter().filter(|u| u.can_upgrade()).collect();
            let state = Check::of(selectable.iter().map(|u| self.selected.contains(&u.id)));
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if widgets::check(ui, &pal, state, "Select all").clicked() {
                            toggle_all = true;
                        }
                        ui.label(text::strong(format!(
                            "{} of {} selected",
                            self.selected.len(),
                            plural(updates.len() as u64, "update")
                        )));
                    });
                });
            let columns = Columns(vec![
                Col::Fixed(22.0),
                Col::Flex(1.6),
                Col::Flex(0.8),
                Col::Flex(0.8),
                Col::Fixed(80.0),
                Col::Fixed(70.0),
            ]);
            let mut sort = (usize::MAX, true);
            widgets::list_header(
                ui,
                &pal,
                &columns,
                &[
                    ("", Align::Min, false),
                    ("Program", Align::Min, false),
                    ("Installed", Align::Min, false),
                    ("Available", Align::Min, false),
                    ("Source", Align::Min, false),
                    ("", Align::Min, false),
                ],
                &mut sort,
            );
            let row_height = 38.0;
            ScrollArea::vertical()
                .id_salt("updates")
                .auto_shrink([false, false])
                .show_rows(ui, row_height, updates.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for index in range {
                        let update = &updates[index];
                        let is_selected = self.selected.contains(&update.id);
                        let (row, response) = widgets::list_row(ui, &pal, row_height, is_selected, Sense::click());
                        let cells = columns.cells(row, 12.0);
                        if update.can_upgrade() {
                            let state = if is_selected { Check::On } else { Check::Off };
                            let clicked = widgets::in_cell(ui, cells[0], |ui| widgets::check(ui, &pal, state, &update.name).clicked());
                            if clicked || response.clicked() {
                                toggled = Some(update.id.clone());
                            }
                        }
                        widgets::paint_text(ui, cells[1], &update.name, text::body_font(14.0), pal.text, Align::Min);
                        let installed = if update.installed_version_known() {
                            update.installed_version.as_str()
                        } else {
                            "Unknown"
                        };
                        widgets::paint_text(ui, cells[2], installed, text::body_font(13.0), pal.text_muted, Align::Min);
                        widgets::paint_text(ui, cells[3], &update.available_version, text::semibold_font(13.0), pal.fresh, Align::Min);
                        widgets::paint_text(ui, cells[4], &update.source, text::body_font(13.0), pal.text_muted, Align::Min);
                        widgets::in_cell_right(ui, cells[5], |ui| {
                            if widgets::quiet_button(ui, &pal, "Ignore")
                                .on_hover_text("Never offer updates for this program")
                                .clicked()
                            {
                                ignore = Some(update.id.clone());
                            }
                        });
                        response.on_hover_text(&update.id);
                    }
                });
        });

        if toggle_all && let Some(updates) = &self.updates {
            let selectable: Vec<&SoftwareUpdate> =
                updates.iter().filter(|u| u.can_upgrade()).collect();
            if selectable.iter().all(|u| self.selected.contains(&u.id)) {
                self.selected.clear();
            } else {
                self.selected = selectable.iter().map(|u| u.id.clone()).collect();
            }
        }
        if let Some(id) = toggled
            && !self.selected.remove(&id)
        {
            self.selected.insert(id);
        }
        if let Some(id) = ignore {
            if let Some(updates) = &mut self.updates {
                updates.retain(|u| u.id != id);
            }
            self.selected.remove(&id);
            if !cx.settings.general.ignored_updates.contains(&id) {
                cx.settings.general.ignored_updates.push(id);
                cx.settings_changed = true;
            }
            cx.toasts
                .info("Ignored. Manage ignored programs in Options.");
        }

        if self.confirm {
            let body = format!(
                "{} will be downloaded from the publisher's source and installed. Windows may ask for permission, and programs that are open may need to close.",
                plural(selected as u64, "update")
            );
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "updates",
                "Install updates?",
                &body,
                "Update",
                false,
            ) {
                Confirm::Yes => {
                    self.confirm = false;
                    let chosen: Vec<SoftwareUpdate> = self
                        .updates
                        .iter()
                        .flatten()
                        .filter(|u| self.selected.contains(&u.id))
                        .cloned()
                        .collect();
                    self.upgrading
                        .start(&ctx, "updates-install", move |progress: Progress| {
                            software::upgrade_all(&chosen, &UpgradeOptions::default(), &progress)
                                .map_err(|e| e.to_string())
                        });
                }
                Confirm::No => self.confirm = false,
                Confirm::Pending => {}
            }
        }
    }
}
