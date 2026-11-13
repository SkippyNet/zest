//! Custom clean: choose what to remove, analyze, review, clean.

use super::Cx;
use crate::job::Job;
use crate::theme::{Palette, mix, text};
use crate::widgets::{self, Check, Confirm, Segment, Tone};
use eframe::egui::{self, Align, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder, pos2, vec2};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use std::time::Instant;
use zest_engine::cleaner::{
    Catalog, CleanReport, Exclusion, Group, GroupIcon, Rule, RuleResult, ScanReport, Section,
    Target,
};
use zest_engine::core::fmt::{format_bytes, format_duration, plural};
use zest_engine::ops::{self, Analysis};
use zest_engine::{RunKind, Settings};

const LEFT_WIDTH: f32 = 350.0;

/// State of the Custom clean page.
#[derive(Default)]
pub struct CleanPage {
    catalog: Option<Catalog>,
    tab: usize,
    collapsed: HashSet<String>,
    analyze: Job<Analysis>,
    analysis: Option<Analysis>,
    cleaning: Job<CleanReport>,
    report: Option<CleanReport>,
    /// Rule whose findings are listed in full, with its rows sorted by size.
    detail: Option<(String, Vec<u32>)>,
    /// Rule waiting for the user to accept its warning.
    warn_rule: Option<String>,
    confirm_clean: bool,
    cleaned_at: Option<Instant>,
}

fn group_glyph(kind: GroupIcon) -> &'static str {
    match kind {
        GroupIcon::System => icon::WINDOWS_LOGO,
        GroupIcon::Explorer => icon::FOLDER_OPEN,
        GroupIcon::Advanced => icon::SLIDERS_HORIZONTAL,
        GroupIcon::Chrome
        | GroupIcon::Edge
        | GroupIcon::Brave
        | GroupIcon::Opera
        | GroupIcon::Vivaldi => icon::GLOBE_HEMISPHERE_WEST,
        GroupIcon::Firefox => icon::GLOBE,
        GroupIcon::Browser => icon::BROWSER,
        GroupIcon::Chat => icon::CHAT_CIRCLE_DOTS,
        GroupIcon::Media => icon::MUSIC_NOTES,
        GroupIcon::Game => icon::GAME_CONTROLLER,
        GroupIcon::Developer => icon::CODE,
        GroupIcon::Office => icon::FILE_TEXT,
        GroupIcon::Graphics => icon::GRAPHICS_CARD,
        GroupIcon::Store => icon::STOREFRONT,
        GroupIcon::App => icon::APP_WINDOW,
        GroupIcon::Custom => icon::FOLDER_PLUS,
    }
}

/// Whether a rule is currently selected, taking the user's choices into account.
fn rule_enabled(settings: &Settings, group: &Group, rule: &Rule) -> bool {
    let default = rule.default_enabled || group.section == Section::Custom;
    settings
        .cleaner
        .rules
        .get(&rule.id)
        .copied()
        .unwrap_or(default)
}

fn set_rule(settings: &mut Settings, group: &Group, rule: &Rule, on: bool) {
    let default = rule.default_enabled || group.section == Section::Custom;
    if on == default {
        settings.cleaner.rules.remove(&rule.id);
    } else {
        settings.cleaner.rules.insert(rule.id.clone(), on);
    }
}

impl CleanPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.analyze.is_running() || self.cleaning.is_running()
    }

    /// Fills the page with made-up results (documentation screenshots).
    pub fn load_demo(&mut self, analysis: Analysis) {
        self.catalog = Some(analysis.catalog.clone());
        self.analysis = Some(analysis);
    }

    /// Starts a read-only analysis (also used by screenshot mode).
    pub fn start_analysis(&mut self, ctx: &egui::Context, settings: &Settings) {
        if self.is_busy() {
            return;
        }
        self.report = None;
        self.detail = None;
        let settings = settings.clone();
        self.analyze.start(ctx, "analyze", move |progress| {
            ops::analyze(&settings, None, &progress)
        });
    }

    fn catalog(&mut self, settings: &Settings) -> &Catalog {
        self.catalog.get_or_insert_with(|| ops::catalog(settings))
    }

    /// The analysis restricted to rules that are still selected.
    fn visible_results<'a>(&'a self, settings: &Settings) -> Vec<&'a RuleResult> {
        let Some(analysis) = &self.analysis else {
            return Vec::new();
        };
        analysis
            .scan
            .results
            .iter()
            .filter(|result| {
                analysis
                    .catalog
                    .rule(&result.rule_id)
                    .is_some_and(|(group, rule)| rule_enabled(settings, group, rule))
            })
            .collect()
    }

    /// True when a rule is selected that the current analysis did not cover.
    fn selection_outdated(&self, settings: &Settings) -> bool {
        let (Some(analysis), Some(catalog)) = (&self.analysis, &self.catalog) else {
            return false;
        };
        catalog.rules().any(|(group, rule)| {
            rule_enabled(settings, group, rule) && !analysis.options.enabled.contains(&rule.id)
        })
    }

    fn start_clean(&mut self, ctx: &egui::Context, settings: &Settings) {
        let Some(analysis) = &self.analysis else {
            return;
        };
        let keep: HashSet<&str> = self
            .visible_results(settings)
            .iter()
            .map(|r| r.rule_id.as_str())
            .collect();
        let mut filtered = analysis.clone();
        filtered
            .scan
            .results
            .retain(|r| keep.contains(r.rule_id.as_str()));
        filtered.scan.bytes = filtered.scan.results.iter().map(|r| r.bytes).sum();
        filtered.scan.items = filtered.scan.results.iter().map(|r| r.items).sum();
        self.detail = None;
        self.cleaning.start(ctx, "clean", move |progress| {
            ops::run_clean(&filtered, RunKind::Manual, false, &progress)
        });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;

        if let Some(result) = self.analyze.poll(&ctx) {
            match result {
                Ok(analysis) => {
                    self.catalog = Some(analysis.catalog.clone());
                    self.analysis = Some(analysis);
                }
                Err(e) => cx.toasts.error(format!("Analysis failed: {e}")),
            }
        }
        if let Some(result) = self.cleaning.poll(&ctx) {
            match result {
                Ok(report) => {
                    cx.toasts
                        .good(format!("{} freed", format_bytes(report.bytes)));
                    self.report = Some(report);
                    self.analysis = None;
                    self.catalog = None;
                    self.cleaned_at = Some(Instant::now());
                }
                Err(e) => cx.toasts.error(format!("Cleaning failed: {e}")),
            }
        }
        self.catalog(cx.settings);

        // Header with the two actions of this page.
        let visible_bytes: u64 = self
            .visible_results(cx.settings)
            .iter()
            .map(|r| r.bytes)
            .sum();
        let visible_items: u64 = self
            .visible_results(cx.settings)
            .iter()
            .map(|r| r.items)
            .sum();
        let has_findings = visible_items > 0;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Custom clean",
                    "Choose what to remove. Analyze shows exactly what would go; nothing is deleted until you clean.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(has_findings && !self.is_busy(), |ui| {
                    if widgets::primary_button(ui, &pal, "Clean").clicked() {
                        if cx.settings.cleaner.confirm_before_clean {
                            self.confirm_clean = true;
                        } else {
                            self.start_clean(&ctx, cx.settings);
                        }
                    }
                });
                ui.add_enabled_ui(!self.is_busy(), |ui| {
                    let clicked = if has_findings {
                        widgets::button(ui, &pal, "Analyze").clicked()
                    } else {
                        widgets::primary_button(ui, &pal, "Analyze").clicked()
                    };
                    if clicked {
                        self.start_analysis(&ctx, cx.settings);
                    }
                });
            });
        });

        // Two columns: what to clean on the left, what was found on the right.
        let area = ui.available_rect_before_wrap();
        let left = Rect::from_min_max(area.min, pos2(area.left() + LEFT_WIDTH, area.bottom()));
        let right = Rect::from_min_max(pos2(left.right() + 16.0, area.top()), area.max);
        ui.scope_builder(UiBuilder::new().max_rect(left), |ui| {
            widgets::list_panel(&pal).show(ui, |ui| {
                ui.set_min_size(left.size() - vec2(2.0, 2.0));
                self.rules_ui(ui, cx);
            });
        });
        ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
            widgets::list_panel(&pal).show(ui, |ui| {
                ui.set_min_size(right.size() - vec2(2.0, 2.0));
                self.results_ui(ui, cx, visible_bytes, visible_items);
            });
        });
        ui.allocate_rect(area, Sense::hover());

        self.dialogs(&ctx, cx, visible_bytes, visible_items);
    }

    fn dialogs(&mut self, ctx: &egui::Context, cx: &mut Cx, bytes: u64, items: u64) {
        let pal = *cx.pal;
        if self.confirm_clean {
            let wipe = cx.settings.cleaner.wipe;
            let mut body = format!(
                "{} ({}) will be permanently removed. This cannot be undone.",
                plural(items, "item"),
                format_bytes(bytes)
            );
            if wipe != zest_engine::core::delete::WipeMethod::None {
                body.push_str(&format!(
                    " Files are overwritten first ({}), which takes longer.",
                    wipe.label().to_lowercase()
                ));
            }
            match widgets::confirm_dialog(ctx, &pal, "clean", "Clean now?", &body, "Clean", false) {
                Confirm::Yes => {
                    self.confirm_clean = false;
                    self.start_clean(ctx, cx.settings);
                }
                Confirm::No => self.confirm_clean = false,
                Confirm::Pending => {}
            }
        }
        if let Some(rule_id) = self.warn_rule.clone() {
            let found = self
                .catalog
                .as_ref()
                .and_then(|c| c.rule(&rule_id))
                .map(|(g, r)| (g.clone(), r.clone()));
            let Some((group, rule)) = found else {
                self.warn_rule = None;
                return;
            };
            let title = format!("Include {} – {}?", group.name, rule.name);
            let body = rule.warning.clone().unwrap_or_default();
            match widgets::confirm_dialog(
                ctx,
                &pal,
                "rule-warning",
                &title,
                &body,
                "Include",
                false,
            ) {
                Confirm::Yes => {
                    set_rule(cx.settings, &group, &rule, true);
                    cx.settings_changed = true;
                    self.warn_rule = None;
                }
                Confirm::No => self.warn_rule = None,
                Confirm::Pending => {}
            }
        }
    }

    // ── Left column: the rule tree ───────────────────────────────────

    fn rules_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let Some(catalog) = self.catalog.clone() else {
            return;
        };
        let sections: Vec<Section> = Section::ALL
            .into_iter()
            .filter(|s| catalog.section(*s).next().is_some())
            .collect();
        self.tab = self.tab.min(sections.len().saturating_sub(1));

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let labels: Vec<&str> = sections.iter().map(|s| s.label()).collect();
            widgets::tabs(ui, &pal, &mut self.tab, &labels);
        });
        let Some(section) = sections.get(self.tab).copied() else {
            return;
        };

        // Quick selections for the visible tab.
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            if widgets::quiet_button(ui, &pal, "Use recommended").clicked() {
                for group in catalog.section(section) {
                    for rule in &group.rules {
                        cx.settings.cleaner.rules.remove(&rule.id);
                    }
                }
                cx.settings_changed = true;
            }
            if widgets::quiet_button(ui, &pal, "Clear all").clicked() {
                for group in catalog.section(section) {
                    for rule in &group.rules {
                        set_rule(cx.settings, group, rule, false);
                    }
                }
                cx.settings_changed = true;
            }
        });
        widgets::hairline(ui, &pal);

        let sizes = |rule_id: &str| -> Option<u64> {
            self.analysis
                .as_ref()
                .and_then(|a| a.scan.rule(rule_id))
                .map(|r| r.bytes)
        };
        let mut toggle_collapse: Option<String> = None;
        let mut warn: Option<String> = None;

        ScrollArea::vertical()
            .id_salt(("rules", self.tab))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for group in catalog.section(section) {
                    let collapsed = self.collapsed.contains(&group.id);
                    let state = Check::of(
                        group
                            .rules
                            .iter()
                            .map(|r| rule_enabled(cx.settings, group, r)),
                    );
                    let (row, response) = widgets::list_row(ui, &pal, 40.0, false, Sense::click());
                    let check_rect = Rect::from_center_size(
                        pos2(row.left() + 9.0, row.center().y),
                        vec2(18.0, 18.0),
                    );
                    let toggled = widgets::in_cell(ui, check_rect, |ui| {
                        widgets::check(ui, &pal, state, &group.name).clicked()
                    });
                    if toggled {
                        // Turning a group on never enables rules that carry a
                        // warning; those must be chosen one by one.
                        let turn_on = state != Check::On;
                        for rule in &group.rules {
                            if turn_on && rule.warning.is_some() {
                                continue;
                            }
                            set_rule(cx.settings, group, rule, turn_on);
                        }
                        cx.settings_changed = true;
                    }
                    ui.painter().text(
                        pos2(row.left() + 38.0, row.center().y),
                        egui::Align2::CENTER_CENTER,
                        group_glyph(group.icon),
                        text::body_font(18.0),
                        pal.text_muted,
                    );
                    let caret = if collapsed {
                        icon::CARET_RIGHT
                    } else {
                        icon::CARET_DOWN
                    };
                    ui.painter().text(
                        pos2(row.right() - 6.0, row.center().y),
                        egui::Align2::CENTER_CENTER,
                        caret,
                        text::body_font(13.0),
                        pal.text_faint,
                    );
                    let group_bytes: u64 = group
                        .rules
                        .iter()
                        .filter(|r| rule_enabled(cx.settings, group, r))
                        .filter_map(|r| sizes(&r.id))
                        .sum();
                    let mut name_right = row.right() - 22.0;
                    if group_bytes > 0 {
                        let size_rect = Rect::from_min_max(
                            pos2(row.right() - 100.0, row.top()),
                            pos2(row.right() - 22.0, row.bottom()),
                        );
                        widgets::paint_text(
                            ui,
                            size_rect,
                            &format_bytes(group_bytes),
                            text::body_font(13.0),
                            pal.text_muted,
                            Align::Max,
                        );
                        name_right = size_rect.left() - 6.0;
                    }
                    let name_rect = Rect::from_min_max(
                        pos2(row.left() + 56.0, row.top()),
                        pos2(name_right, row.bottom()),
                    );
                    widgets::paint_text(
                        ui,
                        name_rect,
                        &group.name,
                        text::semibold_font(14.0),
                        pal.text,
                        Align::Min,
                    );
                    if response.clicked() && !toggled {
                        toggle_collapse = Some(group.id.clone());
                    }
                    if collapsed {
                        continue;
                    }

                    for rule in &group.rules {
                        let enabled = rule_enabled(cx.settings, group, rule);
                        let (row, response) = widgets::tree_row(ui, &pal, 32.0, Sense::click());
                        let check_rect = Rect::from_center_size(
                            pos2(row.left() + 37.0, row.center().y),
                            vec2(18.0, 18.0),
                        );
                        let state = if enabled { Check::On } else { Check::Off };
                        let check_clicked = widgets::in_cell(ui, check_rect, |ui| {
                            widgets::check(ui, &pal, state, &rule.name).clicked()
                        });
                        if check_clicked || response.clicked() {
                            if !enabled && rule.warning.is_some() {
                                warn = Some(rule.id.clone());
                            } else {
                                set_rule(cx.settings, group, rule, !enabled);
                                cx.settings_changed = true;
                            }
                        }
                        let mut right = row.right() - 2.0;
                        if let Some(bytes) = sizes(&rule.id).filter(|_| enabled) {
                            let size_rect = Rect::from_min_max(
                                pos2(right - 84.0, row.top()),
                                pos2(right, row.bottom()),
                            );
                            widgets::paint_text(
                                ui,
                                size_rect,
                                &format_bytes(bytes),
                                text::body_font(13.0),
                                pal.text_muted,
                                Align::Max,
                            );
                            right = size_rect.left() - 6.0;
                        }
                        let needs_admin = rule.requires_admin && !cx.elevated;
                        if needs_admin {
                            right -= 22.0;
                        }
                        let name_rect = Rect::from_min_max(
                            pos2(row.left() + 56.0, row.top()),
                            pos2(right, row.bottom()),
                        );
                        let color = if enabled { pal.text } else { pal.text_muted };
                        let name_width = widgets::elided(
                            ui,
                            &rule.name,
                            text::body_font(14.0),
                            color,
                            name_rect.width(),
                        )
                        .size()
                        .x;
                        widgets::paint_text(
                            ui,
                            name_rect,
                            &rule.name,
                            text::body_font(14.0),
                            color,
                            Align::Min,
                        );
                        if needs_admin {
                            // The shield sits right after the name so it reads as part of it.
                            ui.painter().text(
                                pos2(name_rect.left() + name_width + 12.0, row.center().y),
                                egui::Align2::CENTER_CENTER,
                                icon::SHIELD,
                                text::body_font(14.0),
                                pal.text_faint,
                            );
                        }
                        let mut tip = rule.description.clone();
                        if rule.requires_admin && !cx.elevated {
                            tip.push_str("\n\nSome of these items need administrator rights.");
                        }
                        response.on_hover_text(tip);
                    }
                }
                ui.add_space(8.0);
            });

        if let Some(id) = toggle_collapse
            && !self.collapsed.remove(&id)
        {
            self.collapsed.insert(id);
        }
        if warn.is_some() {
            self.warn_rule = warn;
        }
    }

    // ── Right column: progress, results, detail, report ──────────────

    fn results_ui(&mut self, ui: &mut Ui, cx: &mut Cx, bytes: u64, items: u64) {
        let pal = *cx.pal;
        if self.cleaning.is_running() {
            progress_view(ui, &pal, "Cleaning", &self.cleaning, true);
            return;
        }
        if self.analyze.is_running() {
            progress_view(ui, &pal, "Analyzing", &self.analyze, false);
            return;
        }
        if let Some(report) = &self.report {
            let again = report_view(ui, &pal, report, self.cleaned_at);
            if again {
                let ctx = ui.ctx().clone();
                self.start_analysis(&ctx, cx.settings);
            }
            return;
        }
        if self.analysis.is_none() {
            if widgets::empty_state(
                ui,
                &pal,
                icon::BROOM,
                "Nothing analyzed yet",
                "Analyze lists every file and entry that matches your selection, with sizes. You decide what happens next.",
                Some("Analyze"),
            ) {
                let ctx = ui.ctx().clone();
                self.start_analysis(&ctx, cx.settings);
            }
            return;
        }
        if self.detail.is_some() {
            self.detail_view(ui, cx);
            return;
        }
        self.summary_view(ui, cx, bytes, items);
    }

    fn summary_view(&mut self, ui: &mut Ui, cx: &mut Cx, bytes: u64, items: u64) {
        let pal = *cx.pal;
        let Some(analysis) = &self.analysis else {
            return;
        };
        let results = self.visible_results(cx.settings);
        let outdated = self.selection_outdated(cx.settings);
        let mut open_detail: Option<String> = None;
        let mut reanalyze = false;

        egui::Frame::new().inner_margin(egui::Margin::symmetric(20, 16)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if items == 0 {
                ui.label(text::figure("All clean", 34.0).color(pal.fresh));
                ui.label(text::muted("Nothing in your selection needs removing.", &pal));
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(text::figure(format_bytes(bytes), 40.0));
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        ui.label(text::muted("can be removed", &pal));
                        ui.add_space(-6.0);
                        ui.label(text::small(
                            format!(
                                "{} found in {}",
                                plural(items, "item"),
                                format_duration(analysis.scan.elapsed)
                            ),
                            &pal,
                        ));
                    });
                });
            }
            let blockers: Vec<&str> = analysis
                .catalog
                .groups
                .iter()
                .filter(|g| results.iter().any(|r| r.group_id == g.id && r.blocked_by.is_some()))
                .map(|g| g.name.as_str())
                .collect();
            if !blockers.is_empty() {
                ui.add_space(6.0);
                let message = if let [only] = blockers.as_slice() {
                    format!("{only} is running. Close it and analyze again to include its data; it is skipped otherwise.")
                } else {
                    format!(
                        "{} are running. Close them and analyze again to include their data; they are skipped otherwise.",
                        blockers.join(" and ")
                    )
                };
                if widgets::banner(ui, &pal, Tone::Warn, icon::WARNING, &message, Some("Analyze again")) {
                    reanalyze = true;
                }
            }
            if !cx.elevated && results.iter().any(|r| r.needs_admin) {
                ui.add_space(6.0);
                if widgets::banner(
                    ui,
                    &pal,
                    Tone::Info,
                    icon::SHIELD,
                    "Some system items need administrator rights and will be skipped.",
                    Some("Restart as administrator"),
                ) {
                    cx.request_elevation = true;
                }
            }
            if outdated {
                ui.add_space(6.0);
                if widgets::banner(
                    ui,
                    &pal,
                    Tone::Accent,
                    icon::INFO,
                    "Your selection changed since the last analysis.",
                    Some("Analyze again"),
                ) {
                    reanalyze = true;
                }
            }
        });
        widgets::hairline(ui, &pal);

        ScrollArea::vertical()
            .id_salt("results")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for group in &analysis.catalog.groups {
                    let group_results: Vec<&&RuleResult> =
                        results.iter().filter(|r| r.group_id == group.id).collect();
                    if group_results.is_empty() {
                        continue;
                    }
                    let group_bytes: u64 = group_results.iter().map(|r| r.bytes).sum();
                    let group_items: u64 = group_results.iter().map(|r| r.items).sum();
                    let (row, _) = widgets::list_row(ui, &pal, 42.0, false, Sense::hover());
                    ui.painter().text(
                        pos2(row.left() + 10.0, row.center().y),
                        egui::Align2::CENTER_CENTER,
                        group_glyph(group.icon),
                        text::body_font(18.0),
                        pal.text_muted,
                    );
                    let size_rect =
                        Rect::from_min_max(pos2(row.right() - 96.0, row.top()), row.max);
                    widgets::paint_text(
                        ui,
                        size_rect,
                        &format_bytes(group_bytes),
                        text::semibold_font(14.0),
                        pal.text,
                        Align::Max,
                    );
                    let items_rect = Rect::from_min_max(
                        pos2(size_rect.left() - 110.0, row.top()),
                        pos2(size_rect.left() - 8.0, row.bottom()),
                    );
                    widgets::paint_text(
                        ui,
                        items_rect,
                        &plural(group_items, "item"),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    // Share of the total, drawn as a sliver of stale.
                    let bar_rect = Rect::from_min_max(
                        pos2(items_rect.left() - 130.0, row.center().y - 3.0),
                        pos2(items_rect.left() - 14.0, row.center().y + 3.0),
                    );
                    if bytes > 0 && bar_rect.left() > row.left() + 150.0 {
                        ui.painter().rect_filled(bar_rect, 3.0, pal.well);
                        let mut fill = bar_rect;
                        fill.set_width(
                            (bar_rect.width() * (group_bytes as f32 / bytes as f32)).max(2.0),
                        );
                        ui.painter()
                            .rect_filled(fill, 3.0, mix(pal.stale, pal.accent, 0.25));
                    }
                    let name_right = if bar_rect.left() > row.left() + 150.0 {
                        bar_rect.left() - 10.0
                    } else {
                        items_rect.left() - 8.0
                    };
                    let name_rect = Rect::from_min_max(
                        pos2(row.left() + 30.0, row.top()),
                        pos2(name_right, row.bottom()),
                    );
                    widgets::paint_text(
                        ui,
                        name_rect,
                        &group.name,
                        text::semibold_font(14.0),
                        pal.text,
                        Align::Min,
                    );

                    for result in group_results {
                        let Some(rule) = group.rules.iter().find(|r| r.id == result.rule_id) else {
                            continue;
                        };
                        let (row, response) =
                            widgets::list_row(ui, &pal, 34.0, false, Sense::click());
                        ui.painter().text(
                            pos2(row.right() - 6.0, row.center().y),
                            egui::Align2::CENTER_CENTER,
                            icon::CARET_RIGHT,
                            text::body_font(13.0),
                            if response.hovered() {
                                pal.text
                            } else {
                                pal.text_faint
                            },
                        );
                        let size_rect = Rect::from_min_max(
                            pos2(row.right() - 110.0, row.top()),
                            pos2(row.right() - 20.0, row.bottom()),
                        );
                        widgets::paint_text(
                            ui,
                            size_rect,
                            &format_bytes(result.bytes),
                            text::body_font(14.0),
                            pal.text,
                            Align::Max,
                        );
                        let items_rect = Rect::from_min_max(
                            pos2(size_rect.left() - 110.0, row.top()),
                            pos2(size_rect.left() - 8.0, row.bottom()),
                        );
                        widgets::paint_text(
                            ui,
                            items_rect,
                            &plural(result.items, "item"),
                            text::body_font(13.0),
                            pal.text_muted,
                            Align::Max,
                        );
                        let name_rect = Rect::from_min_max(
                            pos2(row.left() + 30.0, row.top()),
                            pos2(items_rect.left() - 8.0, row.bottom()),
                        );
                        let mut label = rule.name.clone();
                        if result.blocked_by.is_some() {
                            label.push_str("  (program running)");
                        } else if result.needs_admin && !cx.elevated {
                            label.push_str("  (needs administrator)");
                        }
                        widgets::paint_text(
                            ui,
                            name_rect,
                            &label,
                            text::body_font(14.0),
                            pal.text,
                            Align::Min,
                        );
                        if response
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("Show every item")
                            .clicked()
                        {
                            open_detail = Some(result.rule_id.clone());
                        }
                    }
                }
                ui.add_space(8.0);
            });

        if let Some(rule_id) = open_detail
            && let Some(result) = analysis.scan.rule(&rule_id)
        {
            let mut order: Vec<u32> = (0..result.findings.len() as u32).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(result.findings[i as usize].size));
            self.detail = Some((rule_id, order));
        }
        if reanalyze {
            let ctx = ui.ctx().clone();
            self.start_analysis(&ctx, cx.settings);
        }
    }

    fn detail_view(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let Some((rule_id, order)) = self.detail.clone() else {
            return;
        };
        let Some(analysis) = &self.analysis else {
            self.detail = None;
            return;
        };
        let (Some(result), Some((group, rule))) = (
            analysis.scan.rule(&rule_id),
            analysis.catalog.rule(&rule_id),
        ) else {
            self.detail = None;
            return;
        };
        let mut back = false;
        let mut exclude: Option<usize> = None;

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if widgets::icon_button(ui, &pal, icon::ARROW_LEFT, "Back to results").clicked()
                    {
                        back = true;
                    }
                    ui.vertical(|ui| {
                        ui.label(text::section(format!("{} – {}", group.name, rule.name)));
                        ui.add_space(-6.0);
                        ui.label(text::small(
                            format!(
                                "{}, {}",
                                plural(result.items, "item"),
                                format_bytes(result.bytes)
                            ),
                            &pal,
                        ));
                    });
                });
            });
        widgets::hairline(ui, &pal);

        let row_height = 30.0;
        ScrollArea::vertical()
            .id_salt(("detail", &rule_id))
            .auto_shrink([false, false])
            .show_rows(ui, row_height, order.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for row_index in range {
                    let Some(finding) = order
                        .get(row_index)
                        .and_then(|i| result.findings.get(*i as usize))
                    else {
                        continue;
                    };
                    let (row, response) =
                        widgets::list_row(ui, &pal, row_height, false, Sense::hover());
                    let size_rect =
                        Rect::from_min_max(pos2(row.right() - 90.0, row.top()), row.max);
                    let size_text = match (&finding.note, finding.size) {
                        (Some(note), 0) => note.clone(),
                        _ => format_bytes(finding.size),
                    };
                    let can_exclude = matches!(finding.target, Target::File(_) | Target::Dir(_));
                    let hovered = response.hovered() || ui.rect_contains_pointer(row);
                    let mut text_right = size_rect.left() - 8.0;
                    if can_exclude && hovered {
                        let button_rect = Rect::from_min_max(
                            pos2(size_rect.left() - 96.0, row.top()),
                            pos2(size_rect.left() - 6.0, row.bottom()),
                        );
                        let clicked = widgets::in_cell_right(ui, button_rect, |ui| {
                            widgets::quiet_button(ui, &pal, "Exclude")
                                .on_hover_text("Never clean this item")
                                .clicked()
                        });
                        if clicked {
                            exclude = Some(order[row_index] as usize);
                        }
                        text_right = button_rect.left() - 6.0;
                    }
                    widgets::paint_text(
                        ui,
                        size_rect,
                        &size_text,
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    let path_rect = Rect::from_min_max(row.min, pos2(text_right, row.bottom()));
                    widgets::paint_text(
                        ui,
                        path_rect,
                        &finding.target.display(),
                        text::body_font(13.0),
                        pal.text,
                        Align::Min,
                    );
                }
            });

        if back {
            self.detail = None;
        }
        if let Some(index) = exclude {
            self.exclude_finding(cx, &rule_id, index);
        }
    }

    /// Adds a finding's path to the user's exclusions and drops it from the
    /// current results.
    fn exclude_finding(&mut self, cx: &mut Cx, rule_id: &str, index: usize) {
        let Some(analysis) = &mut self.analysis else {
            return;
        };
        let scan: &mut ScanReport = &mut analysis.scan;
        let Some(result) = scan.results.iter_mut().find(|r| r.rule_id == rule_id) else {
            return;
        };
        if index >= result.findings.len() {
            return;
        }
        let finding = result.findings.remove(index);
        result.bytes = result.bytes.saturating_sub(finding.size);
        result.items = result.items.saturating_sub(finding.items);
        scan.bytes = scan.bytes.saturating_sub(finding.size);
        scan.items = scan.items.saturating_sub(finding.items);
        if let Some(path) = finding.target.path() {
            let exclusion = Exclusion::Path(path.to_path_buf());
            if !cx.settings.cleaner.exclusions.contains(&exclusion) {
                cx.settings.cleaner.exclusions.push(exclusion.clone());
                analysis.options.exclusions.push(exclusion);
                cx.settings_changed = true;
            }
            cx.toasts
                .info("Added to your exclusions. Manage them in Options.");
        }
        // Rebuild the sorted order for the shortened list.
        let mut order: Vec<u32> = (0..result.findings.len() as u32).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(result.findings[i as usize].size));
        self.detail = if order.is_empty() {
            None
        } else {
            Some((rule_id.to_owned(), order))
        };
    }
}

fn progress_view<T: Send + 'static>(
    ui: &mut Ui,
    pal: &Palette,
    verb: &str,
    job: &Job<T>,
    show_bytes: bool,
) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.3).clamp(40.0, 200.0));
        ui.label(text::figure(format!("{verb}…"), 30.0));
        let Some(progress) = job.progress() else {
            return;
        };
        ui.add_space(4.0);
        ui.scope(|ui| {
            ui.set_max_width(420.0);
            widgets::progress_bar(ui, pal, progress.fraction(), 8.0);
        });
        ui.add_space(2.0);
        let message = progress.message();
        let detail = if show_bytes && progress.bytes() > 0 {
            format!("{} freed so far", format_bytes(progress.bytes()))
        } else if progress.bytes() > 0 {
            format!("{} found so far", format_bytes(progress.bytes()))
        } else {
            String::new()
        };
        ui.label(text::muted(
            if message.is_empty() {
                " ".to_owned()
            } else {
                message
            },
            pal,
        ));
        ui.label(text::small(
            if detail.is_empty() {
                " ".to_owned()
            } else {
                detail
            },
            pal,
        ));
        ui.add_space(8.0);
        if widgets::button(ui, pal, "Cancel").clicked() {
            job.cancel();
        }
    });
}

/// Shows the outcome of a clean. Returns true when "Analyze again" was clicked.
fn report_view(
    ui: &mut Ui,
    pal: &Palette,
    report: &CleanReport,
    cleaned_at: Option<Instant>,
) -> bool {
    let mut again = false;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(24, 22))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                text::figure(format!("{} freed", format_bytes(report.bytes)), 40.0)
                    .color(pal.fresh),
            );
            ui.label(text::muted(
                format!(
                    "{} removed in {}",
                    plural(report.items, "item"),
                    format_duration(report.elapsed)
                ),
                pal,
            ));
            ui.add_space(10.0);
            // The fresh moment: one sweep of light across a clean bar.
            let sheen = cleaned_at.and_then(|at| {
                let t = at.elapsed().as_secs_f32() / 1.1;
                (t < 1.0).then_some(t)
            });
            if sheen.is_some() {
                ui.ctx().request_repaint();
            }
            widgets::usage_bar(ui, pal, &[(1.0, Segment::Solid)], 14.0, sheen);
            ui.add_space(14.0);

            let skipped: Vec<&str> = report
                .rules
                .iter()
                .filter_map(|r| r.skipped.as_deref())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            if !skipped.is_empty() {
                widgets::banner(
                    ui,
                    pal,
                    Tone::Warn,
                    icon::WARNING,
                    &format!(
                        "Skipped because the program was open: {}.",
                        skipped.join(", ")
                    ),
                    None,
                );
                ui.add_space(6.0);
            }
            if report.in_use > 0 {
                ui.label(text::muted(
                    format!(
                        "{} in use by running programs and left in place.",
                        plural(report.in_use, "item was").replace("items was", "items were")
                    ),
                    pal,
                ));
            }
            if report.failed > 0 {
                ui.label(
                    text::body(format!(
                        "{} could not be removed:",
                        plural(report.failed, "item")
                    ))
                    .color(pal.warn),
                );
                ScrollArea::vertical()
                    .id_salt("failures")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for (what, why) in &report.failures {
                            ui.label(text::small(format!("{what}\n{why}"), pal));
                        }
                    });
            }
            ui.add_space(12.0);
            if widgets::button(ui, pal, "Analyze again").clicked() {
                again = true;
            }
        });
    again
}
