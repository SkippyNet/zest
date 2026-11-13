//! Drivers: what is installed, and updates offered through Windows Update.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Check, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, Layout, ScrollArea, Sense, Ui};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use zest_engine::core::Progress;
use zest_engine::core::fmt::{format_bytes, plural};
use zest_engine::updates::drivers::{
    self, DriverBackupReport, DriverInstallOutcome, DriverPackage, DriverUpdate, DriverUpdateKind,
};

/// State of the Drivers page.
#[derive(Default)]
pub struct DriversPage {
    tab: usize,
    listing: Job<Result<Vec<DriverPackage>, String>>,
    packages: Option<Vec<DriverPackage>>,
    search: String,
    third_party_only: bool,
    searching: Job<Result<Vec<DriverUpdate>, String>>,
    updates: Option<Vec<DriverUpdate>>,
    selected: HashSet<String>,
    installing: Job<Result<Vec<DriverInstallOutcome>, String>>,
    outcomes: Vec<DriverInstallOutcome>,
    backing_up: Job<Result<DriverBackupReport, String>>,
    confirm_install: bool,
}

fn kind_label(kind: DriverUpdateKind) -> &'static str {
    match kind {
        DriverUpdateKind::Driver => "Driver",
        DriverUpdateKind::Extension => "Extension",
        DriverUpdateKind::Firmware => "Firmware",
        DriverUpdateKind::SoftwareComponent => "Component",
    }
}

impl DriversPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.listing.is_running()
            || self.searching.is_running()
            || self.installing.is_running()
            || self.backing_up.is_running()
    }

    /// Lists installed drivers (read-only).
    pub fn reload(&mut self, ctx: &egui::Context) {
        self.listing
            .start(ctx, "drivers-list", |progress: Progress| {
                drivers::list_drivers(&progress)
                    .map(|list| drivers::group_by_package(&list))
                    .map_err(|e| e.to_string())
            });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;

        if let Some(result) = self.listing.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(packages) => self.packages = Some(packages),
                Err(e) => {
                    self.packages = Some(Vec::new());
                    cx.toasts.error(format!("Could not list drivers: {e}"));
                }
            }
        }
        if self.packages.is_none() && !self.listing.is_running() {
            self.third_party_only = true;
            self.reload(&ctx);
        }
        if let Some(result) = self.searching.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(updates) => {
                    // Firmware is never pre-selected: it deserves a deliberate choice.
                    self.selected = updates
                        .iter()
                        .filter(|u| u.kind() != DriverUpdateKind::Firmware)
                        .map(|u| u.update_id.clone())
                        .collect();
                    self.updates = Some(updates);
                }
                Err(e) => cx
                    .toasts
                    .error(format!("Windows Update could not be searched: {e}")),
            }
        }
        if let Some(result) = self.installing.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(outcomes) => {
                    let done: HashSet<&str> = outcomes
                        .iter()
                        .filter(|o| o.status.is_success())
                        .map(|o| o.update_id.as_str())
                        .collect();
                    if let Some(updates) = &mut self.updates {
                        updates.retain(|u| !done.contains(u.update_id.as_str()));
                    }
                    self.selected.retain(|id| !done.contains(id.as_str()));
                    let reboot = outcomes.iter().any(|o| o.reboot_required);
                    let mut message =
                        format!("{} installed.", plural(done.len() as u64, "driver update"));
                    if reboot {
                        message.push_str(" Restart Windows to finish.");
                    }
                    cx.toasts.good(message);
                    self.outcomes = outcomes;
                    self.packages = None;
                }
                Err(e) => cx
                    .toasts
                    .error(format!("Driver updates were not installed: {e}")),
            }
        }
        if let Some(result) = self.backing_up.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(report) => cx.toasts.good(format!(
                    "{} backed up to {} ({}).",
                    plural(report.packages, "driver package"),
                    report.dest.display(),
                    format_bytes(report.bytes)
                )),
                Err(e) => cx.toasts.error(format!("Driver backup failed: {e}")),
            }
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Drivers",
                    "Updates come only from Windows Update, where drivers are signed and matched to your hardware by Microsoft.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                let count = self.selected.len();
                ui.add_enabled_ui(count > 0 && !self.is_busy(), |ui| {
                    if widgets::primary_button(ui, &pal, "Install selected").clicked() {
                        if cx.elevated {
                            self.confirm_install = true;
                        } else {
                            cx.toasts.error("Installing drivers needs administrator rights. Restart as administrator first.");
                        }
                    }
                });
                ui.add_enabled_ui(!self.is_busy(), |ui| {
                    let clicked = if count > 0 {
                        widgets::button(ui, &pal, "Check Windows Update").clicked()
                    } else {
                        widgets::primary_button(ui, &pal, "Check Windows Update").clicked()
                    };
                    if clicked {
                        self.tab = 0;
                        self.outcomes.clear();
                        self.searching.start(&ctx, "drivers-search", |progress: Progress| {
                            drivers::check_driver_updates(&progress).map_err(|e| e.to_string())
                        });
                    }
                });
            });
        });

        let mut toggled: Option<String> = None;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - egui::vec2(2.0, 2.0));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                widgets::tabs(ui, &pal, &mut self.tab, &["Updates", "Installed drivers"]);
            });
            if self.tab == 0 {
                self.updates_ui(ui, cx, &mut toggled);
            } else {
                self.installed_ui(ui, cx);
            }
        });
        if let Some(id) = toggled
            && !self.selected.remove(&id)
        {
            self.selected.insert(id);
        }

        if self.confirm_install {
            let chosen: Vec<DriverUpdate> = self
                .updates
                .iter()
                .flatten()
                .filter(|u| self.selected.contains(&u.update_id))
                .cloned()
                .collect();
            let firmware = chosen
                .iter()
                .any(|u| u.kind() == DriverUpdateKind::Firmware);
            let mut body = format!(
                "{} will be downloaded from Windows Update and installed. The screen may flicker and a restart may be needed.",
                plural(chosen.len() as u64, "update")
            );
            if firmware {
                body.push_str(" Your selection includes a firmware (UEFI/BIOS) update: keep the PC plugged in and do not switch it off while it installs.");
            }
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "drivers-install",
                "Install driver updates?",
                &body,
                "Install",
                false,
            ) {
                Confirm::Yes => {
                    self.confirm_install = false;
                    self.installing
                        .start(&ctx, "drivers-install", move |progress: Progress| {
                            drivers::install_driver_updates(&chosen, &progress)
                                .map_err(|e| e.to_string())
                        });
                }
                Confirm::No => self.confirm_install = false,
                Confirm::Pending => {}
            }
        }
    }

    fn updates_ui(&mut self, ui: &mut Ui, cx: &mut Cx, toggled: &mut Option<String>) {
        let pal = *cx.pal;
        if self.searching.is_running() || self.installing.is_running() {
            let (verb, progress, note) = if self.installing.is_running() {
                (
                    "Installing",
                    self.installing.progress(),
                    "Do not switch off the PC.",
                )
            } else {
                (
                    "Asking Windows Update",
                    self.searching.progress(),
                    "This usually takes one or two minutes.",
                )
            };
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.28).clamp(40.0, 200.0));
                ui.label(text::figure(format!("{verb}…"), 28.0));
                ui.add_space(4.0);
                ui.scope(|ui| {
                    ui.set_max_width(420.0);
                    widgets::progress_bar(ui, &pal, progress.and_then(|p| p.fraction()), 8.0);
                });
                let message = progress.map(|p| p.message()).unwrap_or_default();
                ui.label(text::muted(
                    if message.is_empty() {
                        note.to_owned()
                    } else {
                        message
                    },
                    &pal,
                ));
                if self.searching.is_running() {
                    ui.add_space(6.0);
                    if widgets::button(ui, &pal, "Cancel").clicked() {
                        self.searching.cancel();
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
                            let tone = if outcome.status.is_success() {
                                Tone::Good
                            } else {
                                Tone::Danger
                            };
                            widgets::pill(
                                ui,
                                &pal,
                                if outcome.status.is_success() {
                                    "Installed"
                                } else {
                                    "Failed"
                                },
                                tone,
                            );
                            ui.label(text::strong(&outcome.title));
                            ui.add(
                                egui::Label::new(text::muted(&outcome.message, &pal)).truncate(),
                            );
                        });
                    }
                });
            widgets::hairline(ui, &pal);
        }
        let Some(updates) = &self.updates else {
            widgets::empty_state(
                ui,
                &pal,
                icon::CPU,
                "Ask Windows Update for newer drivers",
                "Searching only reads. You choose what to install afterwards. Use “Check Windows Update” above.",
                None,
            );
            return;
        };
        if updates.is_empty() {
            widgets::empty_state(
                ui,
                &pal,
                icon::CHECK_CIRCLE,
                "Your drivers are current",
                "Windows Update has no newer drivers for this PC.",
                None,
            );
            return;
        }
        if !cx.elevated {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if widgets::banner(
                        ui,
                        &pal,
                        Tone::Info,
                        icon::SHIELD,
                        "Installing drivers needs administrator rights.",
                        Some("Restart as administrator"),
                    ) {
                        cx.request_elevation = true;
                    }
                });
        }
        let columns = Columns(vec![
            Col::Fixed(22.0),
            Col::Flex(1.7),
            Col::Flex(1.2),
            Col::Fixed(96.0),
            Col::Fixed(96.0),
            Col::Fixed(76.0),
        ]);
        let mut sort = (usize::MAX, true);
        widgets::list_header(
            ui,
            &pal,
            &columns,
            &[
                ("", Align::Min, false),
                ("Update", Align::Min, false),
                ("For device", Align::Min, false),
                ("Type", Align::Min, false),
                ("Released", Align::Min, false),
                ("Size", Align::Max, false),
            ],
            &mut sort,
        );
        ScrollArea::vertical()
            .id_salt("driver-updates")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for update in updates {
                    let is_selected = self.selected.contains(&update.update_id);
                    let (row, response) =
                        widgets::list_row(ui, &pal, 44.0, is_selected, Sense::click());
                    let cells = columns.cells(row, 12.0);
                    let state = if is_selected { Check::On } else { Check::Off };
                    let clicked = widgets::in_cell(ui, cells[0], |ui| {
                        widgets::check(ui, &pal, state, &update.title).clicked()
                    });
                    if clicked || response.clicked() {
                        *toggled = Some(update.update_id.clone());
                    }
                    widgets::paint_text(
                        ui,
                        cells[1],
                        &update.title,
                        text::body_font(14.0),
                        pal.text,
                        Align::Min,
                    );
                    let device = update
                        .applies_to
                        .as_ref()
                        .map(|d| format!("{} (now {})", d.device_name, d.driver_version))
                        .unwrap_or_else(|| update.driver_model.clone());
                    widgets::paint_text(
                        ui,
                        cells[2],
                        &device,
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Min,
                    );
                    let kind = update.kind();
                    widgets::in_cell(ui, cells[3], |ui| {
                        let tone = if kind == DriverUpdateKind::Firmware {
                            Tone::Warn
                        } else {
                            Tone::Neutral
                        };
                        let pill = widgets::pill(ui, &pal, kind_label(kind), tone);
                        if kind == DriverUpdateKind::Firmware {
                            pill.on_hover_text(
                                "A UEFI/BIOS update. Keep the PC plugged in while it installs.",
                            );
                        }
                    });
                    let date = update
                        .driver_date
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_default();
                    widgets::paint_text(
                        ui,
                        cells[4],
                        &date,
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        cells[5],
                        &format_bytes(update.size_bytes),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    if !update.description.is_empty() {
                        response.on_hover_text(&update.description);
                    }
                }
            });
    }

    fn installed_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let ctx = ui.ctx().clone();
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    widgets::search_field(ui, &pal, &mut self.search, "Search drivers", 260.0);
                    ui.add_space(8.0);
                    widgets::checkbox(ui, &pal, &mut self.third_party_only, "Hide Windows' own drivers");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::quiet_button(ui, &pal, "Open Device Manager").clicked()
                            && let Err(e) = drivers::open_device_manager()
                        {
                            cx.toasts.error(format!("Could not open Device Manager: {e}"));
                        }
                        ui.add_enabled_ui(!self.is_busy(), |ui| {
                            if widgets::quiet_button(ui, &pal, "Back up drivers")
                                .on_hover_text("Export every third-party driver package to a folder, so it can be reinstalled later")
                                .clicked()
                            {
                                if !cx.elevated {
                                    cx.toasts.error("Backing up drivers needs administrator rights. Restart as administrator first.");
                                } else if let Some(dest) = rfd::FileDialog::new()
                                    .set_title("Choose a folder for the driver backup")
                                    .pick_folder()
                                {
                                    self.backing_up.start(&ctx, "drivers-backup", move |progress: Progress| {
                                        drivers::backup_drivers(&dest, &progress).map_err(|e| e.to_string())
                                    });
                                }
                            }
                        });
                    });
                });
            });
        let Some(packages) = &self.packages else {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                widgets::spinner(ui, &pal, 28.0);
                ui.label(text::muted("Reading installed drivers", &pal));
            });
            return;
        };
        let needle = self.search.trim().to_lowercase();
        let visible: Vec<&DriverPackage> = packages
            .iter()
            .filter(|p| !self.third_party_only || !(p.is_inbox || p.is_microsoft))
            .filter(|p| {
                needle.is_empty()
                    || p.provider.to_lowercase().contains(&needle)
                    || p.class_description.to_lowercase().contains(&needle)
                    || p.devices.iter().any(|d| d.to_lowercase().contains(&needle))
            })
            .collect();
        if visible.is_empty() {
            widgets::empty_state(
                ui,
                &pal,
                icon::CPU,
                "Nothing to show",
                "No driver matches.",
                None,
            );
            return;
        }
        let columns = Columns(vec![
            Col::Flex(1.7),
            Col::Flex(0.9),
            Col::Flex(0.9),
            Col::Fixed(130.0),
            Col::Fixed(92.0),
            Col::Fixed(84.0),
        ]);
        let mut sort = (usize::MAX, true);
        widgets::list_header(
            ui,
            &pal,
            &columns,
            &[
                ("Device", Align::Min, false),
                ("Type", Align::Min, false),
                ("Provider", Align::Min, false),
                ("Version", Align::Min, false),
                ("Date", Align::Min, false),
                ("", Align::Min, false),
            ],
            &mut sort,
        );
        let row_height = 38.0;
        ScrollArea::vertical()
            .id_salt("drivers-installed")
            .auto_shrink([false, false])
            .show_rows(ui, row_height, visible.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for index in range {
                    let package = visible[index];
                    let (row, response) = widgets::list_row(ui, &pal, row_height, false, Sense::hover());
                    let cells = columns.cells(row, 12.0);
                    let mut name = package.devices.first().cloned().unwrap_or_else(|| package.inf_name.clone());
                    if package.devices.len() > 1 {
                        name.push_str(&format!("  (+{} more)", package.devices.len() - 1));
                    }
                    widgets::paint_text(ui, cells[0], &name, text::body_font(14.0), pal.text, Align::Min);
                    widgets::paint_text(ui, cells[1], &package.class_description, text::body_font(13.0), pal.text_muted, Align::Min);
                    widgets::paint_text(ui, cells[2], &package.provider, text::body_font(13.0), pal.text_muted, Align::Min);
                    widgets::paint_text(ui, cells[3], &package.driver_version, text::body_font(13.0), pal.text_muted, Align::Min);
                    let date = package.driver_date.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
                    widgets::paint_text(ui, cells[4], &date, text::body_font(13.0), pal.text_muted, Align::Min);
                    widgets::in_cell_right(ui, cells[5], |ui| {
                        if package.has_problem {
                            widgets::pill(ui, &pal, "Problem", Tone::Danger);
                        } else if package.is_signed == Some(false) {
                            widgets::pill(ui, &pal, "Unsigned", Tone::Warn);
                        } else if package.is_outdated_hint {
                            widgets::pill(ui, &pal, "Old", Tone::Neutral)
                                .on_hover_text("This driver is several years old. That is often fine; check Windows Update for a newer one.");
                        }
                    });
                    response.on_hover_text(format!("{}\n{}", package.devices.join("\n"), package.inf_name));
                }
            });
    }
}
