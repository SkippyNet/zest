//! Performance: what is running, what it costs, and putting background apps to sleep.

use super::Cx;
use crate::theme::{Palette, mix, text};
use crate::widgets::{self, Col, Columns, Tone};
use eframe::egui::{self, Align, Layout, Rect, ScrollArea, Sense, Stroke, Ui, pos2, vec2};
use egui_phosphor::regular as icon;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use zest_engine::core::fmt::{format_bytes, format_duration};
use zest_engine::performance::control::default_journal_path;
use zest_engine::performance::power::{self, PowerPlan};
use zest_engine::performance::{
    AppUsage, BackgroundImpact, ControlState, Sampler, SleepMode, SleepRule, SleepRules,
    SystemSnapshot, apply_rules, trim_all,
};

const SAMPLE_EVERY: Duration = Duration::from_millis(1500);
const RULES_KEY: &str = "sleep_rules";

/// State of the Performance page.
pub struct PerformancePage {
    sampler: Sampler,
    snapshot: Option<SystemSnapshot>,
    last_sample: Option<Instant>,
    cpu_history: VecDeque<f32>,
    control: ControlState,
    rules: Option<SleepRules>,
    tab: usize,
    freeze: bool,
    plans: Option<Vec<PowerPlan>>,
}

impl Default for PerformancePage {
    fn default() -> Self {
        PerformancePage {
            sampler: Sampler::new(),
            snapshot: None,
            last_sample: None,
            cpu_history: VecDeque::new(),
            control: ControlState::with_journal(default_journal_path("app")),
            rules: None,
            tab: 0,
            freeze: false,
            plans: None,
        }
    }
}

impl PerformancePage {
    /// This page has no long-running jobs.
    pub fn is_busy(&self) -> bool {
        false
    }

    /// Keeps sleep rules applied; called every frame, whichever page is shown.
    pub fn tick(&mut self, ctx: &egui::Context, cx: &mut Cx, visible: bool) {
        let rules = self
            .rules
            .get_or_insert_with(|| cx.settings.extra::<SleepRules>(RULES_KEY));
        // Sampling costs a few milliseconds, so it only runs when it is
        // needed: while the page is open, or while rules are active.
        if !visible && rules.is_empty() {
            return;
        }
        let due = self
            .last_sample
            .is_none_or(|at| at.elapsed() >= SAMPLE_EVERY);
        if due {
            self.last_sample = Some(Instant::now());
            match self.sampler.sample() {
                Ok(snapshot) => {
                    if !rules.is_empty() {
                        apply_rules(rules, &snapshot, &mut self.control);
                    }
                    self.cpu_history.push_back(snapshot.cpu_percent);
                    while self.cpu_history.len() > 60 {
                        self.cpu_history.pop_front();
                    }
                    self.snapshot = Some(snapshot);
                }
                Err(e) => log::warn!("sampling failed: {e}"),
            }
        }
        ctx.request_repaint_after(SAMPLE_EVERY);
    }

    fn save_rules(&self, cx: &mut Cx) {
        if let Some(rules) = &self.rules {
            cx.settings.set_extra(RULES_KEY, rules);
            cx.settings_changed = true;
        }
    }

    fn set_sleep(&mut self, cx: &mut Cx, app: &AppUsage, asleep: bool) {
        let Some(rules) = &mut self.rules else {
            return;
        };
        if asleep {
            let mode = if self.freeze {
                SleepMode::Suspend
            } else {
                SleepMode::Efficiency
            };
            rules.upsert(SleepRule::for_app(app, mode));
            match self.control.sleep(app, mode) {
                Ok(report) if report.is_success() => {
                    cx.toasts.info(format!("{} is asleep", app.name))
                }
                Ok(report) if report.needs_admin() => {
                    cx.toasts.error(format!("{} runs as administrator. Restart Zest as administrator to put it to sleep.", app.name));
                }
                Ok(_) => cx
                    .toasts
                    .error(format!("{} could not be put to sleep", app.name)),
                Err(e) => cx.toasts.error(e.to_string()),
            }
        } else {
            if let Some(target) = rules.find(app).map(|r| r.target.clone()) {
                rules.remove(&target);
            }
            self.control.wake(&app.id);
            cx.toasts.info(format!("{} is awake", app.name));
        }
        self.save_rules(cx);
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let mut trim = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Performance",
                    "Background apps use processor time, memory and battery. Put the ones you rarely need to sleep.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                if widgets::button(ui, &pal, "Free up memory")
                    .on_hover_text("Asks background apps to release memory they are not using. Windows gives it back to them as needed.")
                    .clicked()
                {
                    trim = true;
                }
            });
        });
        let Some(snapshot) = self.snapshot.clone() else {
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                widgets::spinner(ui, &pal, 28.0);
            });
            return;
        };
        if trim {
            match trim_all(&snapshot) {
                Ok(report) => cx.toasts.good(format!(
                    "{} released by {} processes.",
                    format_bytes(report.before.saturating_sub(report.after)),
                    report.processes
                )),
                Err(e) => cx.toasts.error(format!("Could not free memory: {e}")),
            }
        }

        // Three gauges across the top.
        let gauges = ui.available_rect_before_wrap();
        let gap = 14.0;
        let width = (gauges.width() - gap * 2.0) / 3.0;
        let height = 118.0;
        for index in 0..3 {
            let rect = Rect::from_min_size(
                pos2(gauges.left() + (width + gap) * index as f32, gauges.top()),
                vec2(width, height),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                widgets::panel(&pal).show(ui, |ui| {
                    ui.set_min_size(rect.size() - vec2(38.0, 38.0));
                    match index {
                        0 => {
                            ui.label(text::small("Processor", &pal));
                            ui.horizontal(|ui| {
                                ui.label(text::figure(
                                    format!("{:.0} %", snapshot.cpu_percent),
                                    30.0,
                                ));
                                let spark = ui.available_rect_before_wrap().shrink2(vec2(6.0, 4.0));
                                sparkline(ui, &pal, spark, &self.cpu_history);
                            });
                        }
                        1 => {
                            ui.label(text::small("Memory", &pal));
                            ui.horizontal(|ui| {
                                ui.label(text::figure(format_bytes(snapshot.memory.used), 30.0));
                                ui.label(text::muted(
                                    format!("of {}", format_bytes(snapshot.memory.total)),
                                    &pal,
                                ));
                            });
                            widgets::progress_bar(
                                ui,
                                &pal,
                                Some(snapshot.memory.used_percent / 100.0),
                                6.0,
                            );
                        }
                        _ => {
                            ui.label(text::small("Power plan", &pal));
                            let plans = self.plans.get_or_insert_with(|| {
                                power::list_power_plans().unwrap_or_default()
                            });
                            let active = plans
                                .iter()
                                .find(|p| p.active)
                                .map_or("Unknown", |p| p.name.as_str())
                                .to_owned();
                            let mut chosen: Option<String> = None;
                            egui::ComboBox::from_id_salt("power-plan")
                                .selected_text(text::strong(active))
                                .width(ui.available_width() - 8.0)
                                .show_ui(ui, |ui| {
                                    for plan in plans.iter() {
                                        if ui.selectable_label(plan.active, &plan.name).clicked()
                                            && !plan.active
                                        {
                                            chosen = Some(plan.guid.clone());
                                        }
                                    }
                                });
                            if let Some(guid) = chosen {
                                match power::set_active_power_plan(&guid) {
                                    Ok(()) => cx.toasts.info("Power plan changed"),
                                    Err(e) => cx
                                        .toasts
                                        .error(format!("Could not change the power plan: {e}")),
                                }
                                self.plans = None;
                            }
                            ui.label(text::small(
                                format!("Running for {}", format_duration(snapshot.uptime)),
                                &pal,
                            ));
                        }
                    }
                });
            });
        }
        ui.allocate_rect(
            Rect::from_min_size(gauges.min, vec2(gauges.width(), height)),
            Sense::hover(),
        );
        ui.add_space(6.0);

        // Sleep rules are enforced by whichever part of Zest is running.
        let has_rules = self.rules.as_ref().is_some_and(|r| !r.is_empty());
        if has_rules && !cx.settings.smart.enabled {
            if widgets::banner(
                ui,
                &pal,
                Tone::Info,
                icon::INFO,
                "Apps stay asleep only while Zest is open. Turn on background watching to keep them asleep after you close it.",
                Some("Open options"),
            ) {
                cx.goto = Some(super::Page::Options);
            }
            ui.add_space(6.0);
        }

        let mut change: Option<(AppUsage, bool)> = None;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - vec2(2.0, 2.0));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                widgets::tabs(ui, &pal, &mut self.tab, &["Background apps", "All apps", "Asleep"]);
            });
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        widgets::checkbox(ui, &pal, &mut self.freeze, "Freeze apps completely when they sleep");
                        ui.label(text::small(
                            "Off: the app keeps running at low priority. On: it stops entirely until you open it again.",
                            &pal,
                        ));
                    });
                });

            let rules = self.rules.clone().unwrap_or_default();
            let visible: Vec<&AppUsage> = snapshot
                .apps
                .iter()
                .filter(|app| match self.tab {
                    0 => app.is_background() && !app.is_protected(),
                    2 => rules.find(app).is_some() || self.control.is_sleeping(&app.id),
                    _ => true,
                })
                .collect();
            if visible.is_empty() {
                let (title, body) = if self.tab == 2 {
                    ("No apps are asleep", "Switch on “Sleep” for a background app to keep it out of the way.")
                } else {
                    ("Nothing running in the background", "Only Windows itself and the apps you have open are running.")
                };
                widgets::empty_state(ui, &pal, icon::MOON, title, body, None);
                return;
            }
            let columns = Columns(vec![
                Col::Flex(1.6),
                Col::Fixed(70.0),
                Col::Fixed(90.0),
                Col::Fixed(120.0),
                Col::Fixed(76.0),
            ]);
            let mut sort = (usize::MAX, true);
            widgets::list_header(
                ui,
                &pal,
                &columns,
                &[
                    ("App", Align::Min, false),
                    ("Processor", Align::Max, false),
                    ("Memory", Align::Max, false),
                    ("Impact", Align::Min, false),
                    ("Sleep", Align::Min, false),
                ],
                &mut sort,
            );
            let row_height = 44.0;
            ScrollArea::vertical()
                .id_salt(("perf", self.tab))
                .auto_shrink([false, false])
                .show_rows(ui, row_height, visible.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for index in range {
                        let app = visible[index];
                        let asleep = rules.find(app).is_some() || self.control.is_sleeping(&app.id);
                        let (row, response) = widgets::list_row(ui, &pal, row_height, false, Sense::hover());
                        let cells = columns.cells(row, 12.0);
                        let name_rect = Rect::from_min_max(cells[0].min, pos2(cells[0].right(), cells[0].center().y + 3.0));
                        let sub_rect = Rect::from_min_max(pos2(cells[0].left(), cells[0].center().y - 1.0), cells[0].max);
                        let mut name = app.name.clone();
                        if app.process_count() > 1 {
                            name.push_str(&format!("  ({})", app.process_count()));
                        }
                        widgets::paint_text(ui, name_rect.translate(vec2(0.0, 3.0)), &name, text::body_font(14.0), pal.text, Align::Min);
                        widgets::paint_text(
                            ui,
                            sub_rect.translate(vec2(0.0, -2.0)),
                            app.publisher.as_deref().unwrap_or(&app.exe_name),
                            text::body_font(12.0),
                            pal.text_faint,
                            Align::Min,
                        );
                        widgets::paint_text(ui, cells[1], &format!("{:.1} %", app.avg_cpu_percent), text::body_font(13.0), pal.text_muted, Align::Max);
                        widgets::paint_text(ui, cells[2], &format_bytes(app.memory), text::body_font(13.0), pal.text_muted, Align::Max);
                        widgets::in_cell(ui, cells[3], |ui| {
                            if app.is_protected() {
                                ui.label(text::small("Protected", &pal)).on_hover_text(
                                    app.protection.map_or("Part of Windows", |p| p.label()),
                                );
                            } else {
                                let (label, tone) = match app.impact {
                                    BackgroundImpact::High => ("High", Tone::Warn),
                                    BackgroundImpact::Medium => ("Medium", Tone::Accent),
                                    BackgroundImpact::Low => ("Low", Tone::Neutral),
                                };
                                widgets::pill(ui, &pal, label, tone);
                                if app.recommended && !asleep {
                                    ui.label(text::icon(icon::STAR, 14.0).color(pal.accent_text))
                                        .on_hover_text("A good candidate: no window, and noticeable use of resources.");
                                }
                            }
                        });
                        if !app.is_protected() {
                            widgets::in_cell(ui, cells[4], |ui| {
                                let mut on = asleep;
                                if widgets::switch(ui, &pal, &mut on, &app.name) {
                                    change = Some((app.clone(), on));
                                }
                            });
                        }
                        if let Some(path) = &app.exe_path {
                            response.on_hover_text(path.display().to_string());
                        }
                    }
                });
        });
        if let Some((app, asleep)) = change {
            self.set_sleep(cx, &app, asleep);
        }
    }
}

fn sparkline(ui: &Ui, pal: &Palette, rect: Rect, values: &VecDeque<f32>) {
    if values.len() < 2 || rect.width() < 20.0 {
        return;
    }
    let max = values.iter().copied().fold(25.0f32, f32::max);
    let step = rect.width() / 59.0;
    let offset = (60 - values.len()) as f32 * step;
    let points: Vec<egui::Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            pos2(
                rect.left() + offset + i as f32 * step,
                rect.bottom() - rect.height() * (v / max).clamp(0.0, 1.0),
            )
        })
        .collect();
    ui.painter().hline(
        rect.x_range(),
        rect.bottom(),
        Stroke::new(1.0, mix(pal.surface, pal.line, 0.8)),
    );
    ui.painter().line(points, Stroke::new(1.6, pal.accent));
}
