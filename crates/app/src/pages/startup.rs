//! Startup: everything that starts by itself, with reversible on/off switches.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, Layout, Rect, ScrollArea, Sense, Ui, pos2};
use egui_phosphor::regular as icon;
use zest_engine::core::reg::RegBackup;
use zest_engine::core::{Error, Progress, paths};
use zest_engine::system::startup::{self, StartupItem, StartupKind};

const TABS: [(&str, &[StartupKind]); 4] = [
    ("Startup apps", &StartupKind::PROGRAMS),
    ("Scheduled tasks", &[StartupKind::ScheduledTask]),
    ("Services", &[StartupKind::Service]),
    ("Context menu", &[StartupKind::ContextMenu]),
];

/// State of the Startup page.
#[derive(Default)]
pub struct StartupPage {
    loading: Job<Result<Vec<StartupItem>, String>>,
    items: Option<Vec<StartupItem>>,
    tab: usize,
    search: String,
    confirm_delete: Option<StartupItem>,
}

impl StartupPage {
    /// True while the list is loading.
    pub fn is_busy(&self) -> bool {
        self.loading.is_running()
    }

    /// Loads every list (read-only).
    pub fn reload(&mut self, ctx: &egui::Context) {
        self.loading
            .start(ctx, "startup-list", |progress: Progress| {
                startup::list_all(&progress).map_err(|e| e.to_string())
            });
    }

    fn toggle(&mut self, cx: &mut Cx, id: &str) {
        let Some(item) = self
            .items
            .as_mut()
            .and_then(|items| items.iter_mut().find(|i| i.id == id))
        else {
            return;
        };
        let target = !item.enabled;
        match startup::set_enabled(item, target) {
            Ok(()) => {
                item.enabled = target;
                let verb = if target { "enabled" } else { "disabled" };
                let mut message = format!("{} {verb}", item.name);
                if item.kind == StartupKind::ContextMenu {
                    message.push_str(". Takes effect after Explorer restarts.");
                } else if item.kind == StartupKind::Service && !target {
                    message.push_str(". It keeps running until the next restart.");
                }
                cx.toasts.info(message);
            }
            Err(Error::NeedsElevation(_)) => {
                cx.toasts.error(
                    "This entry applies to all users. Restart as administrator to change it.",
                );
            }
            Err(e) => cx
                .toasts
                .error(format!("Could not change {}: {e}", item.name)),
        }
    }

    fn delete(&mut self, cx: &mut Cx, item: &StartupItem) {
        let mut backup = RegBackup::new("startup");
        let task_dir = paths::backups_dir().join("tasks");
        match startup::delete(item, Some(&mut backup), Some(&task_dir)) {
            Ok(_) => {
                if let Err(e) = backup.save() {
                    log::warn!("could not save startup backup: {e}");
                }
                if let Some(items) = &mut self.items {
                    items.retain(|i| i.id != item.id);
                }
                cx.toasts
                    .good(format!("{} removed. A backup was saved.", item.name));
            }
            Err(Error::NeedsElevation(_)) => {
                cx.toasts.error(
                    "This entry applies to all users. Restart as administrator to remove it.",
                );
            }
            Err(e) => cx
                .toasts
                .error(format!("Could not remove {}: {e}", item.name)),
        }
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.loading.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(items) => self.items = Some(items),
                Err(e) => {
                    self.items = Some(Vec::new());
                    cx.toasts
                        .error(format!("Could not read startup entries: {e}"));
                }
            }
        }
        if self.items.is_none() && !self.loading.is_running() {
            self.reload(&ctx);
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Startup",
                    "Programs, tasks and services that start by themselves. Switching one off is reversible.",
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

        let mut toggle: Option<String> = None;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - egui::vec2(2.0, 2.0));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                let labels: Vec<&str> = TABS.iter().map(|(label, _)| *label).collect();
                widgets::tabs(ui, &pal, &mut self.tab, &labels);
            });
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        widgets::search_field(ui, &pal, &mut self.search, "Search", 260.0);
                        ui.add_space(8.0);
                        if widgets::checkbox(
                            ui,
                            &pal,
                            &mut cx.settings.general.show_system_entries,
                            "Show Windows entries",
                        ) {
                            cx.settings_changed = true;
                        }
                    });
                });

            let Some(items) = &self.items else {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    widgets::spinner(ui, &pal, 28.0);
                    ui.label(text::muted("Reading startup entries", &pal));
                });
                return;
            };
            let kinds = TABS[self.tab].1;
            let needle = self.search.trim().to_lowercase();
            let show_system = cx.settings.general.show_system_entries;
            let visible: Vec<&StartupItem> = items
                .iter()
                .filter(|i| kinds.contains(&i.kind))
                .filter(|i| show_system || !i.is_microsoft)
                .filter(|i| {
                    needle.is_empty()
                        || i.name.to_lowercase().contains(&needle)
                        || i.command.to_lowercase().contains(&needle)
                        || i.publisher.as_deref().unwrap_or("").to_lowercase().contains(&needle)
                })
                .collect();
            if visible.is_empty() {
                let hidden = items.iter().filter(|i| kinds.contains(&i.kind)).count();
                let body = if hidden > 0 && !show_system {
                    "Only Windows entries exist here. Tick “Show Windows entries” to see them."
                } else {
                    "Nothing matches."
                };
                widgets::empty_state(ui, &pal, icon::ROCKET_LAUNCH, "Nothing to show", body, None);
                return;
            }

            let columns = Columns(vec![
                Col::Fixed(44.0),
                Col::Flex(1.3),
                Col::Flex(0.9),
                Col::Flex(1.6),
                Col::Fixed(36.0),
            ]);
            let mut sort = (usize::MAX, true);
            widgets::list_header(
                ui,
                &pal,
                &columns,
                &[
                    ("On", Align::Min, false),
                    ("Name", Align::Min, false),
                    ("Publisher", Align::Min, false),
                    ("Starts", Align::Min, false),
                    ("", Align::Min, false),
                ],
                &mut sort,
            );
            let row_height = 44.0;
            ScrollArea::vertical()
                .id_salt(("startup", self.tab))
                .auto_shrink([false, false])
                .show_rows(ui, row_height, visible.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for index in range {
                        let item = visible[index];
                        let (row, response) = widgets::list_row(ui, &pal, row_height, false, Sense::hover());
                        let cells = columns.cells(row, 12.0);
                        let locked = item.toggle_requires_admin && !cx.elevated;
                        widgets::in_cell(ui, cells[0], |ui| {
                            if !item.can_toggle {
                                ui.label(text::small("Fixed", &pal))
                                    .on_hover_text("Windows does not allow switching this entry off. It can only be removed.");
                            } else {
                                ui.add_enabled_ui(!locked, |ui| {
                                    let mut on = item.enabled;
                                    if widgets::switch(ui, &pal, &mut on, &item.name) {
                                        toggle = Some(item.id.clone());
                                    }
                                })
                                .response
                                .on_disabled_hover_text("Applies to all users. Restart as administrator to change it.");
                            }
                        });
                        let name_color = if item.enabled { pal.text } else { pal.text_muted };
                        let mut name_rect = cells[1];
                        if item.target_missing {
                            let pill = Rect::from_min_max(pos2(cells[1].right() - 74.0, cells[1].top()), cells[1].max);
                            widgets::in_cell_right(ui, pill, |ui| {
                                widgets::pill(ui, &pal, "Missing", Tone::Warn)
                                    .on_hover_text("The program this entry starts no longer exists. It is safe to remove.");
                            });
                            name_rect.set_right(pill.left() - 6.0);
                        }
                        widgets::paint_text(ui, name_rect, &item.name, text::body_font(14.0), name_color, Align::Min);
                        widgets::paint_text(
                            ui,
                            cells[2],
                            item.publisher.as_deref().unwrap_or("Unknown"),
                            text::body_font(13.0),
                            pal.text_muted,
                            Align::Min,
                        );
                        let starts = if item.command.is_empty() { &item.location } else { &item.command };
                        widgets::paint_text(ui, cells[3], starts, text::body_font(13.0), pal.text_muted, Align::Min);
                        if item.can_delete {
                            widgets::in_cell_right(ui, cells[4], |ui| {
                                if widgets::icon_button(ui, &pal, icon::TRASH, "Remove this entry").clicked() {
                                    self.confirm_delete = Some(item.clone());
                                }
                            });
                        }
                        let mut tip = format!("{}\n{}", item.location, item.command);
                        if let Some(description) = &item.description {
                            tip = format!("{description}\n\n{tip}");
                        }
                        response.on_hover_text(tip);
                    }
                });
        });

        if let Some(id) = toggle {
            self.toggle(cx, &id);
        }
        if let Some(item) = self.confirm_delete.clone() {
            let body = format!(
                "“{}” will no longer start by itself. The program itself is not uninstalled, and a backup of the entry is saved.",
                item.name
            );
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "startup-delete",
                "Remove this startup entry?",
                &body,
                "Remove",
                true,
            ) {
                Confirm::Yes => {
                    self.confirm_delete = None;
                    self.delete(cx, &item);
                }
                Confirm::No => self.confirm_delete = None,
                Confirm::Pending => {}
            }
        }
    }
}
