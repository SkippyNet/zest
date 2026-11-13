//! Health check: privacy, space, speed and security at a glance.

use super::{Cx, Page};
use crate::job::Job;
use crate::theme::{Palette, mix, text};
use crate::widgets::{self, Confirm, Segment, Tone};
use eframe::egui::{self, Align, Align2, Layout, Rect, Sense, Ui, pos2, vec2};
use egui_phosphor::regular as icon;
use std::time::Instant;
use zest_engine::Settings;
use zest_engine::cleaner::CleanReport;
use zest_engine::core::fmt::{format_bytes, format_count, plural};
use zest_engine::health::{self, HealthReport};
use zest_engine::performance::power;

/// State of the Health check page.
#[derive(Default)]
pub struct HealthPage {
    checking: Job<HealthReport>,
    report: Option<HealthReport>,
    fixing: Job<CleanReport>,
    fixed: Option<(CleanReport, u64, Instant)>,
    drive: Option<(u64, u64)>,
    confirm: bool,
}

struct Area<'a> {
    glyph: &'a str,
    title: &'a str,
    finding: String,
    tone: Tone,
    status: &'a str,
    action: Option<(&'a str, Page)>,
}

impl HealthPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.checking.is_running() || self.fixing.is_running()
    }

    /// Fills the page with made-up results (documentation screenshots).
    pub fn load_demo(&mut self, report: HealthReport) {
        self.drive = Some((report.drive_total, report.drive_free));
        self.report = Some(report);
    }

    /// Starts the (read-only) health check.
    pub fn start_check(&mut self, ctx: &egui::Context, settings: &Settings) {
        if self.is_busy() {
            return;
        }
        self.fixed = None;
        let settings = settings.clone();
        self.checking.start(ctx, "health-check", move |progress| {
            health::check(&settings, &progress)
        });
    }

    fn start_fix(&mut self, ctx: &egui::Context) {
        if let Some(report) = self.report.clone() {
            self.fixing.start(ctx, "health-fix", move |progress| {
                health::fix(&report, &progress)
            });
        }
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if self.confirm {
            let body = self.report.as_ref().map_or_else(String::new, |r| {
                format!(
                    "{} of junk and {} will be permanently removed. Startup programs and software updates are not changed.",
                    format_bytes(r.junk_bytes),
                    plural(r.trackers + r.privacy_items, "tracker")
                )
            });
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "health-fix",
                "Clean now?",
                &body,
                "Clean",
                false,
            ) {
                Confirm::Yes => {
                    self.confirm = false;
                    self.start_fix(&ctx);
                }
                Confirm::No => self.confirm = false,
                Confirm::Pending => {}
            }
        }
        if let Some(result) = self.checking.poll(&ctx) {
            match result {
                Ok(report) => {
                    self.drive = Some((report.drive_total, report.drive_free));
                    self.report = Some(report);
                }
                Err(e) => cx.toasts.error(format!("Health check failed: {e}")),
            }
        }
        if let Some(result) = self.fixing.poll(&ctx) {
            match result {
                Ok(clean) => {
                    let trackers = self.report.as_ref().map_or(0, |r| r.trackers);
                    cx.toasts
                        .good(format!("{} freed", format_bytes(clean.bytes)));
                    self.fixed = Some((clean, trackers, Instant::now()));
                    // Keep speed and security findings, clear what was cleaned.
                    if let Some(report) = &mut self.report {
                        report.analysis.scan.results.clear();
                        report.analysis.scan.bytes = 0;
                        report.analysis.scan.items = 0;
                        report.trackers = 0;
                        report.privacy_items = 0;
                        report.junk_bytes = 0;
                        report.junk_items = 0;
                    }
                    let summary = power::system_summary();
                    self.drive = Some((summary.system_drive_total, summary.system_drive_free));
                }
                Err(e) => cx.toasts.error(format!("Cleaning failed: {e}")),
            }
        }
        let (drive_total, drive_free) = *self.drive.get_or_insert_with(|| {
            let summary = power::system_summary();
            (summary.system_drive_total, summary.system_drive_free)
        });

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Health check",
                    "A quick look at privacy, space, speed and security. Checking changes nothing.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                let can_fix = self.report.as_ref().is_some_and(HealthReport::can_fix);
                ui.add_enabled_ui(can_fix && !self.is_busy(), |ui| {
                    if widgets::primary_button(ui, &pal, "Make it better").clicked() {
                        if cx.settings.cleaner.confirm_before_clean {
                            self.confirm = true;
                        } else {
                            self.start_fix(&ctx);
                        }
                    }
                });
                ui.add_enabled_ui(!self.is_busy(), |ui| {
                    let label = if self.report.is_some() {
                        "Check again"
                    } else {
                        "Check now"
                    };
                    let clicked = if can_fix {
                        widgets::button(ui, &pal, label).clicked()
                    } else {
                        widgets::primary_button(ui, &pal, label).clicked()
                    };
                    if clicked {
                        self.start_check(&ctx, cx.settings);
                    }
                });
            });
        });

        // The drive as a usage bar: solid where space is in use,
        // hatched where it can be reclaimed.
        widgets::panel(&pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let junk = self.report.as_ref().map_or(0, |r| r.junk_bytes);
            let used = drive_total.saturating_sub(drive_free);
            let total = drive_total.max(1) as f32;
            let keep = used.saturating_sub(junk) as f32 / total;
            // Give tiny junk shares a visible sliver.
            let junk_share = if junk > 0 {
                (junk as f32 / total).max(0.012)
            } else {
                0.0
            };
            let sheen = self.fixed.as_ref().and_then(|(_, _, at)| {
                let t = at.elapsed().as_secs_f32() / 1.2;
                (t < 1.0).then_some(t)
            });
            if sheen.is_some() {
                ctx.request_repaint();
            }
            ui.horizontal(|ui| {
                ui.label(text::section("System drive"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(text::muted(
                        format!("{} in total", format_bytes(drive_total)),
                        &pal,
                    ));
                });
            });
            ui.add_space(2.0);
            widgets::usage_bar(
                ui,
                &pal,
                &[(keep, Segment::Solid), (junk_share, Segment::Hatched)],
                22.0,
                sheen,
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 22.0;
                widgets::legend(
                    ui,
                    &pal,
                    pal.accent,
                    "in use",
                    &format_bytes(used.saturating_sub(junk)),
                );
                if junk > 0 {
                    widgets::legend(
                        ui,
                        &pal,
                        mix(pal.stale, pal.accent, 0.35),
                        "can be cleaned",
                        &format_bytes(junk),
                    );
                }
                widgets::legend(ui, &pal, pal.well, "free", &format_bytes(drive_free));
            });
        });
        ui.add_space(6.0);

        if self.checking.is_running() || self.fixing.is_running() {
            let (verb, job_progress) = if self.fixing.is_running() {
                ("Cleaning", self.fixing.progress())
            } else {
                ("Checking your PC", self.checking.progress())
            };
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(26.0);
                ui.vertical_centered(|ui| {
                    ui.label(text::figure(format!("{verb}…"), 28.0));
                    ui.add_space(4.0);
                    ui.scope(|ui| {
                        ui.set_max_width(420.0);
                        widgets::progress_bar(
                            ui,
                            &pal,
                            job_progress.and_then(|p| p.fraction()),
                            8.0,
                        );
                    });
                    let message = job_progress.map(|p| p.message()).unwrap_or_default();
                    ui.label(text::muted(
                        if message.is_empty() {
                            " ".to_owned()
                        } else {
                            message
                        },
                        &pal,
                    ));
                });
                ui.add_space(26.0);
            });
            return;
        }

        if let Some((clean, trackers, _)) = &self.fixed {
            let mut message = format!(
                "{} freed, {} removed",
                format_bytes(clean.bytes),
                plural(clean.items, "item")
            );
            if *trackers > 0 {
                message.push_str(&format!(", including {}", plural(*trackers, "tracker")));
            }
            message.push('.');
            if clean.in_use > 0 {
                message.push_str(&format!(
                    " {} in use and left in place.",
                    plural(clean.in_use, "item was").replace("items was", "items were")
                ));
            }
            widgets::banner(ui, &pal, Tone::Good, icon::CHECK_CIRCLE, &message, None);
            ui.add_space(6.0);
        }

        let Some(report) = &self.report else {
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height((ui.available_height() - 40.0).max(160.0));
                if widgets::empty_state(
                    ui,
                    &pal,
                    icon::HEARTBEAT,
                    "See how your PC is doing",
                    "The check measures junk files and trackers, lists what starts with Windows and looks for out-of-date software.",
                    Some("Check now"),
                ) {
                    self.start_check(&ctx, cx.settings);
                }
            });
            return;
        };

        let areas = build_areas(report);
        let mut goto = None;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for area in &areas {
                if let Some(page) = area_row(ui, &pal, area) {
                    goto = Some(page);
                }
            }
        });
        if goto.is_some() {
            cx.goto = goto;
        }
        if !report.blocked.is_empty() {
            ui.add_space(6.0);
            let names = report.blocked.join(" and ");
            let message = if report.blocked.len() == 1 {
                format!(
                    "{names} is running, so its data could not be checked or cleaned. Close it and check again."
                )
            } else {
                format!(
                    "{names} are running, so their data could not be checked or cleaned. Close them and check again."
                )
            };
            widgets::banner(ui, &pal, Tone::Warn, icon::WARNING, &message, None);
        }
    }
}

fn build_areas(report: &HealthReport) -> Vec<Area<'static>> {
    let privacy_total = report.trackers + report.privacy_items;
    let privacy = Area {
        glyph: icon::EYE_SLASH,
        title: "Privacy",
        finding: if privacy_total == 0 {
            "No trackers found.".to_owned()
        } else if report.privacy_items == 0 {
            format!(
                "{} can be removed. Sites on your keep list stay signed in.",
                plural(report.trackers, "tracker")
            )
        } else {
            format!(
                "{} and {} can be removed.",
                plural(report.trackers, "tracker"),
                plural(report.privacy_items, "history entry").replace("entrys", "entries")
            )
        },
        tone: if privacy_total == 0 {
            Tone::Good
        } else {
            Tone::Accent
        },
        status: if privacy_total == 0 {
            "Good"
        } else {
            "Can improve"
        },
        action: Some(("Choose what to keep", Page::Options)),
    };
    let space = Area {
        glyph: icon::HARD_DRIVES,
        title: "Space",
        finding: if report.junk_bytes == 0 {
            "No junk to remove.".to_owned()
        } else {
            format!(
                "{} of junk in {} can be removed.",
                format_bytes(report.junk_bytes),
                plural(report.junk_items, "item")
            )
        },
        tone: if report.junk_bytes == 0 {
            Tone::Good
        } else {
            Tone::Accent
        },
        status: if report.junk_bytes == 0 {
            "Good"
        } else {
            "Can improve"
        },
        action: Some(("Review items", Page::Clean)),
    };
    let startup = report.startup.len();
    let speed = Area {
        glyph: icon::ROCKET_LAUNCH,
        title: "Speed",
        finding: match startup {
            0 => "Nothing extra starts with Windows.".to_owned(),
            1 => format!("1 program starts with Windows: {}.", report.startup[0].name),
            n => {
                let names: Vec<&str> = report
                    .startup
                    .iter()
                    .take(3)
                    .map(|s| s.name.as_str())
                    .collect();
                let more = if n > 3 {
                    format!(" and {} more", n - 3)
                } else {
                    String::new()
                };
                format!(
                    "{} programs start with Windows: {}{more}.",
                    format_count(n as u64),
                    names.join(", ")
                )
            }
        },
        tone: if startup <= 3 {
            Tone::Good
        } else {
            Tone::Accent
        },
        status: if startup <= 3 { "Good" } else { "Can improve" },
        action: Some(("Review startup", Page::Startup)),
    };
    let security = match &report.updates {
        None => Area {
            glyph: icon::SHIELD_CHECK,
            title: "Security",
            finding: "Software updates could not be checked on this PC.".to_owned(),
            tone: Tone::Neutral,
            status: "Not checked",
            action: Some(("Open updates", Page::Updates)),
        },
        Some(updates) if updates.is_empty() => Area {
            glyph: icon::SHIELD_CHECK,
            title: "Security",
            finding: "Your software is up to date.".to_owned(),
            tone: Tone::Good,
            status: "Good",
            action: None,
        },
        Some(updates) => Area {
            glyph: icon::SHIELD_CHECK,
            title: "Security",
            finding: format!(
                "{} can be updated. Updates often fix security problems.",
                plural(updates.len() as u64, "program")
            ),
            tone: Tone::Warn,
            status: "Needs attention",
            action: Some(("Review updates", Page::Updates)),
        },
    };
    vec![privacy, space, speed, security]
}

/// Draws one area row. Returns the page to open when its action is clicked.
fn area_row(ui: &mut Ui, pal: &Palette, area: &Area) -> Option<Page> {
    let (row, _) = widgets::list_row(ui, pal, 76.0, false, Sense::hover());
    let mut goto = None;

    let badge = Rect::from_center_size(pos2(row.left() + 22.0, row.center().y), vec2(44.0, 44.0));
    let tone = area.tone.color(pal);
    ui.painter()
        .circle_filled(badge.center(), 22.0, pal.tint(tone, 0.14));
    ui.painter().text(
        badge.center(),
        Align2::CENTER_CENTER,
        area.glyph,
        text::body_font(22.0),
        tone,
    );

    // Right side: status and the action.
    let right = Rect::from_min_max(pos2(row.right() - 330.0, row.top()), row.max);
    widgets::in_cell_right(ui, right, |ui| {
        if let Some((label, page)) = area.action
            && widgets::button(ui, pal, label).clicked()
        {
            goto = Some(page);
        }
        ui.add_space(4.0);
        widgets::pill(ui, pal, area.status, area.tone);
    });

    let text_rect = Rect::from_min_max(
        pos2(row.left() + 62.0, row.top() + 14.0),
        pos2(right.left() - 10.0, row.bottom() - 12.0),
    );
    let title_rect = Rect::from_min_max(
        text_rect.min,
        pos2(text_rect.right(), text_rect.top() + 24.0),
    );
    widgets::paint_text(
        ui,
        title_rect,
        area.title,
        text::semibold_font(16.0),
        pal.text,
        Align::Min,
    );
    let body_rect = Rect::from_min_max(pos2(text_rect.left(), title_rect.bottom()), text_rect.max);
    widgets::paint_text(
        ui,
        body_rect,
        &area.finding,
        text::body_font(14.0),
        pal.text_muted,
        Align::Min,
    );
    goto
}
