//! Software updates through the Windows Package Manager, with the public
//! software catalog as a stand-in on PCs where the package manager cannot
//! be used.

use super::Cx;
use crate::job::Job;
use crate::theme::{Palette, text};
use crate::widgets::{self, Check, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, Layout, ScrollArea, Sense, Ui};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use zest_engine::Settings;
use zest_engine::core::Progress;
use zest_engine::core::fmt::{format_count, plural};
use zest_engine::updates::software::{
    self, CatalogUpdate, CheckOptions, Listing, Problem, Remedy, SoftwareUpdate, UpgradeOptions,
    UpgradeOutcome, UpgradeStatus,
};

/// What is known when the package manager could not be used.
struct Fallback {
    /// Why the package manager was not used.
    problem: Problem,
    /// What the public catalog says instead, with the catalog's size.
    catalog: Option<(Vec<CatalogUpdate>, usize)>,
    /// Why the catalog could not be used either.
    catalog_error: Option<String>,
}

/// State of the Software updates page.
#[derive(Default)]
pub struct UpdatesPage {
    checking: Job<Result<Listing, String>>,
    /// Updates the package manager can install.
    updates: Option<Vec<SoftwareUpdate>>,
    /// Set instead of `updates` when the package manager is unavailable.
    fallback: Option<Fallback>,
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
            catalog_fallback: settings.general.catalog_fallback,
            ..Default::default()
        };
        self.checking
            .start(ctx, "updates-check", move |progress: Progress| {
                software::check(&options, &progress).map_err(|e| e.to_string())
            });
    }

    fn take_listing(&mut self, listing: Listing) {
        self.selected.clear();
        match listing {
            Listing::PackageManager(updates) => {
                self.selected = updates
                    .iter()
                    .filter(|u| u.can_upgrade() && !u.requires_explicit_upgrade)
                    .map(|u| u.id.clone())
                    .collect();
                self.updates = Some(updates);
                self.fallback = None;
            }
            Listing::Catalog {
                problem,
                updates,
                packages_in_catalog,
            } => {
                self.updates = None;
                self.fallback = Some(Fallback {
                    problem,
                    catalog: Some((updates, packages_in_catalog)),
                    catalog_error: None,
                });
            }
            Listing::Unavailable {
                problem,
                catalog_error,
            } => {
                self.updates = None;
                self.fallback = Some(Fallback {
                    problem,
                    catalog: None,
                    catalog_error,
                });
            }
        }
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;

        if let Some(result) = self.checking.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(listing) => self.take_listing(listing),
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
        let can_install = self.updates.is_some();
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
                ui.add_enabled_ui(can_install && selected > 0 && !self.is_busy(), |ui| {
                    if widgets::primary_button(ui, &pal, "Update selected").clicked() {
                        self.confirm = true;
                    }
                });
                ui.add_enabled_ui(!self.is_busy(), |ui| {
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

        // What kept the package manager from answering, and the way out.
        let mut retry = false;
        if !self.is_busy()
            && let Some(fallback) = &self.fallback
        {
            let problem = &fallback.problem;
            let remedy = problem.remedy();
            if widgets::banner(
                ui,
                &pal,
                Tone::Warn,
                icon::WARNING,
                &format!("{}. {}", problem.title(), problem.explanation()),
                remedy.action_label(),
            ) {
                match remedy {
                    Remedy::InstallPackageManager | Remedy::UpdatePackageManager => {
                        ctx.open_url(egui::OpenUrl::new_tab(software::INSTALL_URL));
                    }
                    Remedy::Retry => retry = true,
                    Remedy::ResetSources | Remedy::AskAdministrator => {}
                }
            }
            if let Some(detail) = problem.detail() {
                ui.label(text::small(detail, &pal));
            }
            ui.add_space(2.0);
        }
        if retry {
            self.start_check(&ctx, cx.settings);
        }

        let mut ignore: Option<String> = None;
        let mut toggled: Option<String> = None;
        let mut toggle_all = false;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - egui::vec2(2.0, 2.0));
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
                    ui.add_space(6.0);
                    if self.upgrading.is_running() {
                        if widgets::button(ui, &pal, "Stop after this one").clicked() {
                            self.upgrading.cancel();
                        }
                    } else if widgets::button(ui, &pal, "Cancel").clicked() {
                        self.checking.cancel();
                    }
                });
                return;
            }

            if let Some(fallback) = &self.fallback {
                catalog_list(ui, &pal, fallback, &mut ignore);
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
            if let Some(Fallback {
                catalog: Some((updates, _)),
                ..
            }) = &mut self.fallback
            {
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

/// The list shown while the package manager is unavailable: what the public
/// catalog knows, for information, with a link to each program's page.
fn catalog_list(ui: &mut Ui, pal: &Palette, fallback: &Fallback, ignore: &mut Option<String>) {
    let Some((updates, packages)) = &fallback.catalog else {
        // Nothing is known. This must not read like "all is well".
        widgets::empty_state(
            ui,
            pal,
            icon::CLOUD_SLASH,
            "Software updates could not be checked",
            fallback.catalog_error.as_deref().unwrap_or(
                "Resolve the problem described above, then check again. The public software catalog was not consulted because that is switched off in Options.",
            ),
            None,
        );
        return;
    };
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(text::strong(match updates.len() {
                0 => "No newer versions found in the public software catalog".to_owned(),
                n => format!(
                    "{} found in the public software catalog",
                    plural(n as u64, "newer version")
                ),
            }));
            ui.add(
                egui::Label::new(text::small(
                    format!(
                        "Your installed programs were compared with Microsoft's community catalog of {} packages. This list is for information: installing from here needs the Windows Package Manager. Programs the catalog does not know are not listed.",
                        format_count(*packages as u64)
                    ),
                    pal,
                ))
                .wrap(),
            );
        });
    if updates.is_empty() {
        return;
    }
    let columns = Columns(vec![
        Col::Flex(1.6),
        Col::Flex(0.8),
        Col::Flex(0.8),
        Col::Fixed(150.0),
    ]);
    let mut sort = (usize::MAX, true);
    widgets::list_header(
        ui,
        pal,
        &columns,
        &[
            ("Program", Align::Min, false),
            ("Installed", Align::Min, false),
            ("Available", Align::Min, false),
            ("", Align::Min, false),
        ],
        &mut sort,
    );
    let row_height = 38.0;
    ScrollArea::vertical()
        .id_salt("updates-catalog")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, updates.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for index in range {
                let update = &updates[index];
                let (row, response) = widgets::list_row(ui, pal, row_height, false, Sense::hover());
                let cells = columns.cells(row, 12.0);
                widgets::paint_text(
                    ui,
                    cells[0],
                    &update.name,
                    text::body_font(14.0),
                    pal.text,
                    Align::Min,
                );
                widgets::paint_text(
                    ui,
                    cells[1],
                    &update.installed_version,
                    text::body_font(13.0),
                    pal.text_muted,
                    Align::Min,
                );
                widgets::paint_text(
                    ui,
                    cells[2],
                    &update.available_version,
                    text::semibold_font(13.0),
                    pal.fresh,
                    Align::Min,
                );
                widgets::in_cell_right(ui, cells[3], |ui| {
                    if widgets::quiet_button(ui, pal, "Ignore")
                        .on_hover_text("Never list this program")
                        .clicked()
                    {
                        *ignore = Some(update.id.clone());
                    }
                    if widgets::quiet_button(ui, pal, "Get it")
                        .on_hover_text(&update.info_url)
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&update.info_url));
                    }
                });
                response.on_hover_text(&update.id);
            }
        });
}
