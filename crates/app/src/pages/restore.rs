//! System restore: list, create and remove restore points.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, ScrollArea, Sense, Ui};
use egui_phosphor::regular as icon;
use zest_engine::system::restore::{self, CreateOutcome, RestorePoint};

/// State of the System restore page.
#[derive(Default)]
pub struct RestorePage {
    protection: Option<Option<bool>>,
    loading: Job<Result<Vec<RestorePoint>, String>>,
    points: Option<Vec<RestorePoint>>,
    description: String,
    working: Job<Result<String, String>>,
    confirm_delete: Option<RestorePoint>,
}

impl RestorePage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.loading.is_running() || self.working.is_running()
    }

    fn reload(&mut self, ctx: &egui::Context) {
        self.loading.start(ctx, "restore-list", |_| {
            restore::list_restore_points().map_err(|e| e.to_string())
        });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        let protection = *self
            .protection
            .get_or_insert_with(|| restore::is_protection_enabled().ok());
        if let Some(result) = self.loading.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(mut points) => {
                    points.sort_by_key(|p| std::cmp::Reverse(p.sequence_number));
                    self.points = Some(points);
                }
                Err(e) => {
                    self.points = Some(Vec::new());
                    cx.toasts
                        .error(format!("Could not read restore points: {e}"));
                }
            }
        }
        if let Some(result) = self.working.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(message) => {
                    cx.toasts.good(message);
                    self.points = None;
                }
                Err(e) => cx.toasts.error(e),
            }
        }
        if cx.elevated && self.points.is_none() && !self.is_busy() {
            self.reload(&ctx);
        }

        widgets::page_header(
            ui,
            &pal,
            "System restore",
            "Restore points let Windows roll back system files and settings. Old ones can use several gigabytes.",
        );

        match protection {
            Some(false) => {
                widgets::banner(
                    ui,
                    &pal,
                    Tone::Warn,
                    icon::WARNING,
                    "System protection is switched off on this PC, so Windows is not creating restore points. Turn it on under System > About > System protection.",
                    None,
                );
                ui.add_space(8.0);
            }
            None => {
                widgets::banner(
                    ui,
                    &pal,
                    Tone::Neutral,
                    icon::INFO,
                    "The state of system protection could not be read.",
                    None,
                );
                ui.add_space(8.0);
            }
            Some(true) => {}
        }

        if !cx.elevated {
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_min_size(ui.available_size() - egui::vec2(38.0, 38.0));
                if widgets::empty_state(
                    ui,
                    &pal,
                    icon::SHIELD,
                    "Administrator rights needed",
                    "Windows only shows restore points to administrators.",
                    Some("Restart as administrator"),
                ) {
                    cx.request_elevation = true;
                }
            });
            return;
        }

        widgets::panel(&pal)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(text::strong("New restore point"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.description)
                            .hint_text("Describe why, for example “Before installing drivers”")
                            .desired_width(380.0),
                    );
                    let description = self.description.trim().to_owned();
                    ui.add_enabled_ui(!description.is_empty() && !self.is_busy(), |ui| {
                        if widgets::primary_button(ui, &pal, "Create").clicked() {
                            self.description.clear();
                            self.working.start(&ctx, "restore-create", move |_| {
                                match restore::create_restore_point(&description) {
                                    Ok(CreateOutcome::Created(_)) => Ok("Restore point created".to_owned()),
                                    Ok(CreateOutcome::SkippedByWindows) => Err(
                                        "Windows skipped it: a restore point was already created in the last 24 hours.".to_owned(),
                                    ),
                                    Err(e) => Err(format!("Could not create the restore point: {e}")),
                                }
                            });
                        }
                    });
                    if self.working.is_running() {
                        widgets::spinner(ui, &pal, 20.0);
                    }
                });
            });
        ui.add_space(6.0);

        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - egui::vec2(2.0, 2.0));
            let Some(points) = &self.points else {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    widgets::spinner(ui, &pal, 28.0);
                });
                return;
            };
            if points.is_empty() {
                widgets::empty_state(
                    ui,
                    &pal,
                    icon::CLOCK_COUNTER_CLOCKWISE,
                    "No restore points",
                    "Create one above before making big changes to the system.",
                    None,
                );
                return;
            }
            let columns = Columns(vec![
                Col::Flex(1.6),
                Col::Flex(0.8),
                Col::Fixed(150.0),
                Col::Fixed(40.0),
            ]);
            let mut sort = (usize::MAX, true);
            widgets::list_header(
                ui,
                &pal,
                &columns,
                &[
                    ("Description", Align::Min, false),
                    ("Type", Align::Min, false),
                    ("Created", Align::Min, false),
                    ("", Align::Min, false),
                ],
                &mut sort,
            );
            let newest = points.first().map(|p| p.sequence_number);
            ScrollArea::vertical()
                .id_salt("restore-points")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for point in points {
                        let (row, _) = widgets::list_row(ui, &pal, 42.0, false, Sense::hover());
                        let cells = columns.cells(row, 12.0);
                        widgets::paint_text(
                            ui,
                            cells[0],
                            &point.description,
                            text::body_font(14.0),
                            pal.text,
                            Align::Min,
                        );
                        widgets::paint_text(
                            ui,
                            cells[1],
                            point.kind.label(),
                            text::body_font(13.0),
                            pal.text_muted,
                            Align::Min,
                        );
                        let created = point
                            .created
                            .map(|c| c.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        widgets::paint_text(
                            ui,
                            cells[2],
                            &created,
                            text::body_font(13.0),
                            pal.text_muted,
                            Align::Min,
                        );
                        widgets::in_cell_right(ui, cells[3], |ui| {
                            // The newest point is the safety net; it stays.
                            if Some(point.sequence_number) == newest {
                                ui.label(text::icon(icon::LOCK_SIMPLE, 15.0).color(pal.text_faint))
                                    .on_hover_text("The most recent restore point is kept.");
                            } else if widgets::icon_button(
                                ui,
                                &pal,
                                icon::TRASH,
                                "Remove this restore point",
                            )
                            .clicked()
                            {
                                self.confirm_delete = Some(point.clone());
                            }
                        });
                    }
                });
        });

        if let Some(point) = self.confirm_delete.clone() {
            let body = format!(
                "“{}” is removed for good. You will no longer be able to roll Windows back to it.",
                point.description
            );
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "restore-delete",
                "Remove this restore point?",
                &body,
                "Remove",
                true,
            ) {
                Confirm::Yes => {
                    self.confirm_delete = None;
                    self.working.start(&ctx, "restore-delete", move |_| {
                        restore::delete_restore_point(point.sequence_number)
                            .map(|()| "Restore point removed".to_owned())
                            .map_err(|e| format!("Could not remove the restore point: {e}"))
                    });
                }
                Confirm::No => self.confirm_delete = None,
                Confirm::Pending => {}
            }
        }
    }
}
