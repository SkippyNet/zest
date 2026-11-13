//! Browser extensions: what is installed, how much access it has, and removal.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder, pos2, vec2};
use egui_phosphor::regular as icon;
use zest_engine::cleaner::extensions::{self, AccessLevel, BrowserExtension};
use zest_engine::core::fmt::format_bytes;
use zest_engine::ops;

/// State of the Browser extensions page.
#[derive(Default)]
pub struct ExtensionsPage {
    loading: Job<Vec<BrowserExtension>>,
    extensions: Option<Vec<BrowserExtension>>,
    search: String,
    selected: Option<usize>,
    confirm: Option<BrowserExtension>,
}

fn access_tone(level: AccessLevel) -> Tone {
    match level {
        AccessLevel::Low => Tone::Neutral,
        AccessLevel::Medium => Tone::Accent,
        AccessLevel::High => Tone::Warn,
    }
}

impl ExtensionsPage {
    /// True while the list is loading.
    pub fn is_busy(&self) -> bool {
        self.loading.is_running()
    }

    /// Reads the extensions of every browser profile (read-only).
    pub fn reload(&mut self, ctx: &egui::Context, cx: &Cx) {
        let users = ops::users(cx.settings);
        self.selected = None;
        self.loading.start(ctx, "extensions", move |_| {
            extensions::list_extensions(&users)
        });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.loading.poll(&ctx) {
            self.extensions = Some(result.unwrap_or_default());
        }
        if self.extensions.is_none() && !self.loading.is_running() {
            self.reload(&ctx, cx);
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Browser extensions",
                    "Extensions can read what you browse. Review how much access each one has and remove the ones you do not use.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(!self.is_busy(), |ui| {
                    if widgets::icon_text_button(ui, &pal, icon::ARROWS_CLOCKWISE, "Refresh").clicked() {
                        self.reload(&ctx, cx);
                    }
                });
            });
        });

        let Some(list) = self.extensions.clone() else {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                widgets::spinner(ui, &pal, 28.0);
            });
            return;
        };
        let needle = self.search.trim().to_lowercase();
        let visible: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                needle.is_empty()
                    || e.name.to_lowercase().contains(&needle)
                    || e.browser.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();

        let area = ui.available_rect_before_wrap();
        let detail_width = if self.selected.is_some() { 360.0 } else { 0.0 };
        let left = Rect::from_min_max(
            area.min,
            pos2(
                area.right() - detail_width - if detail_width > 0.0 { 16.0 } else { 0.0 },
                area.bottom(),
            ),
        );
        ui.scope_builder(UiBuilder::new().max_rect(left), |ui| {
            widgets::list_panel(&pal).show(ui, |ui| {
                ui.set_min_size(left.size() - vec2(2.0, 2.0));
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        widgets::search_field(ui, &pal, &mut self.search, "Search extensions", 280.0);
                    });
                if visible.is_empty() {
                    widgets::empty_state(
                        ui,
                        &pal,
                        icon::PUZZLE_PIECE,
                        "No extensions found",
                        "None of your browsers has an extension installed, or nothing matches the search.",
                        None,
                    );
                    return;
                }
                let columns = Columns(vec![Col::Flex(1.5), Col::Flex(1.0), Col::Fixed(80.0), Col::Fixed(74.0)]);
                let mut sort = (usize::MAX, true);
                widgets::list_header(
                    ui,
                    &pal,
                    &columns,
                    &[
                        ("Extension", Align::Min, false),
                        ("Browser", Align::Min, false),
                        ("Access", Align::Min, false),
                        ("Size", Align::Max, false),
                    ],
                    &mut sort,
                );
                ScrollArea::vertical()
                    .id_salt("extensions")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for index in &visible {
                            let ext = &list[*index];
                            let is_selected = self.selected == Some(*index);
                            let (row, response) = widgets::list_row(ui, &pal, 44.0, is_selected, Sense::click());
                            let cells = columns.cells(row, 12.0);
                            let color = if ext.enabled == Some(false) { pal.text_muted } else { pal.text };
                            let mut name = ext.name.clone();
                            if ext.enabled == Some(false) {
                                name.push_str("  (off)");
                            }
                            widgets::paint_text(ui, cells[0], &name, text::body_font(14.0), color, Align::Min);
                            widgets::paint_text(
                                ui,
                                cells[1],
                                &format!("{}, {}", ext.browser, ext.profile),
                                text::body_font(13.0),
                                pal.text_muted,
                                Align::Min,
                            );
                            widgets::in_cell(ui, cells[2], |ui| {
                                widgets::pill(ui, &pal, ext.access.label(), access_tone(ext.access));
                            });
                            widgets::paint_text(ui, cells[3], &format_bytes(ext.size), text::body_font(13.0), pal.text_muted, Align::Max);
                            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                self.selected = if is_selected { None } else { Some(*index) };
                            }
                        }
                    });
            });
        });

        if let Some(ext) = self.selected.and_then(|i| list.get(i)) {
            let right = Rect::from_min_max(pos2(left.right() + 16.0, area.top()), area.max);
            ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
                widgets::panel(&pal).show(ui, |ui| {
                    ui.set_min_size(right.size() - vec2(38.0, 38.0));
                    ScrollArea::vertical().id_salt("extension-detail").show(ui, |ui| {
                        ui.label(text::section(&ext.name));
                        ui.label(text::small(format!("Version {} in {}, {}", ext.version, ext.browser, ext.profile), &pal));
                        if !ext.description.is_empty() {
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(text::muted(&ext.description, &pal)).wrap());
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.label(text::strong("Access"));
                            widgets::pill(ui, &pal, ext.access.label(), access_tone(ext.access));
                        });
                        if ext.access_reasons.is_empty() {
                            ui.label(text::muted("Works only on specific sites and uses no sensitive browser features.", &pal));
                        }
                        for reason in &ext.access_reasons {
                            ui.horizontal(|ui| {
                                ui.label(text::icon(icon::DOT_OUTLINE, 14.0).color(pal.text_faint));
                                ui.add(egui::Label::new(text::body(reason)).wrap());
                            });
                        }
                        if !ext.hosts.is_empty() {
                            ui.add_space(8.0);
                            ui.label(text::strong("Sites"));
                            let shown: Vec<&str> = ext.hosts.iter().take(8).map(String::as_str).collect();
                            let mut sites = shown.join("\n");
                            if ext.hosts.len() > 8 {
                                sites.push_str(&format!("\nand {} more", ext.hosts.len() - 8));
                            }
                            ui.label(text::small(sites, &pal));
                        }
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(ext.removable, |ui| {
                                if widgets::danger_button(ui, &pal, "Remove").clicked() {
                                    self.confirm = Some(ext.clone());
                                }
                            })
                            .response
                            .on_disabled_hover_text("Installed by the browser or by a policy; it cannot be removed here.");
                            if let Some(url) = &ext.store_url
                                && widgets::quiet_button(ui, &pal, "Store page").clicked()
                            {
                                ctx.open_url(egui::OpenUrl::new_tab(url));
                            }
                        });
                    });
                });
            });
        }
        ui.allocate_rect(area, Sense::hover());

        if let Some(ext) = self.confirm.clone() {
            let body = format!(
                "Its files are moved to the Recycle Bin, so this can be undone. Close {} first. The browser may still list it as damaged until you remove it there too.",
                ext.browser
            );
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "extension-remove",
                &format!("Remove {}?", ext.name),
                &body,
                "Remove",
                true,
            ) {
                Confirm::Yes => {
                    self.confirm = None;
                    match extensions::remove_extension(&ext) {
                        Ok(()) => {
                            cx.toasts.good(format!("{} removed", ext.name));
                            self.reload(&ctx, cx);
                        }
                        Err(e) => cx.toasts.error(e.to_string()),
                    }
                }
                Confirm::No => self.confirm = None,
                Confirm::Pending => {}
            }
        }
    }
}
