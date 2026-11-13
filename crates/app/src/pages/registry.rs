//! Registry cleaner: scan, review, fix with a backup, restore.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Check, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder, pos2, vec2};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use zest_engine::Settings;
use zest_engine::core::Progress;
use zest_engine::core::fmt::{format_age, format_bytes, format_duration, plural};
use zest_engine::registry::{
    self, BackupInfo, Category, Confidence, FixReport, Issue, RegBackup, ScanOptions,
};

const LEFT_WIDTH: f32 = 300.0;

/// State of the Registry page.
#[derive(Default)]
pub struct RegistryPage {
    scanning: Job<Result<(Vec<Issue>, Duration), String>>,
    issues: Option<Vec<Issue>>,
    scan_time: Duration,
    selected: HashSet<String>,
    fixing: Job<Result<FixReport, String>>,
    last_fix: Option<FixReport>,
    tab: usize,
    backups: Option<Vec<BackupInfo>>,
    confirm_fix: bool,
    confirm_restore: Option<PathBuf>,
    restoring: Job<Result<(), String>>,
}

fn category_enabled(settings: &Settings, category: Category) -> bool {
    settings
        .registry
        .categories
        .get(category.slug())
        .copied()
        .unwrap_or_else(|| category.default_enabled())
}

impl RegistryPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.scanning.is_running() || self.fixing.is_running() || self.restoring.is_running()
    }

    /// Starts a (read-only) registry scan.
    pub fn start_scan(&mut self, ctx: &egui::Context, settings: &Settings) {
        if self.is_busy() {
            return;
        }
        self.last_fix = None;
        self.tab = 0;
        let options = ScanOptions {
            categories: Category::ALL
                .into_iter()
                .filter(|c| category_enabled(settings, *c))
                .collect(),
            excluded: settings.registry.excluded.clone(),
        };
        self.scanning.start(ctx, "registry-scan", move |progress| {
            let started = std::time::Instant::now();
            registry::scan(&options, &progress)
                .map(|issues| (issues, started.elapsed()))
                .map_err(|e| e.to_string())
        });
    }

    fn start_fix(&mut self, ctx: &egui::Context, settings: &Settings) {
        let Some(issues) = &self.issues else {
            return;
        };
        let chosen: Vec<Issue> = issues
            .iter()
            .filter(|i| self.selected.contains(&i.id))
            .cloned()
            .collect();
        let with_backup = settings.registry.backup_before_fix;
        self.fixing
            .start(ctx, "registry-fix", move |progress: Progress| {
                let mut backup = with_backup.then(|| RegBackup::new("registry-clean"));
                registry::fix(&chosen, backup.as_mut(), &progress).map_err(|e| e.to_string())
            });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;

        if let Some(result) = self.scanning.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok((issues, elapsed)) => {
                    self.selected = issues
                        .iter()
                        .filter(|i| i.selected_by_default() && (cx.elevated || !i.requires_admin))
                        .map(|i| i.id.clone())
                        .collect();
                    self.scan_time = elapsed;
                    self.issues = Some(issues);
                }
                Err(e) => cx.toasts.error(format!("Registry scan failed: {e}")),
            }
        }
        if let Some(result) = self.fixing.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(report) => {
                    if let Some(issues) = &mut self.issues {
                        issues.retain(|i| !report.fixed_ids.contains(&i.id));
                    }
                    for id in &report.fixed_ids {
                        self.selected.remove(id);
                    }
                    cx.toasts
                        .good(format!("{} fixed", plural(report.fixed as u64, "issue")));
                    self.last_fix = Some(report);
                    self.backups = None;
                }
                Err(e) => cx.toasts.error(format!("Nothing was changed: {e}")),
            }
        }
        if let Some(result) = self.restoring.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(()) => cx.toasts.good("Backup restored"),
                Err(e) => cx.toasts.error(format!("Restore failed: {e}")),
            }
        }

        let selected_count = self.selected.len();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Registry",
                    "Finds entries that point at programs and files that no longer exist. Every fix is backed up first.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(selected_count > 0 && !self.is_busy(), |ui| {
                    if widgets::primary_button(ui, &pal, "Fix selected").clicked() {
                        self.confirm_fix = true;
                    }
                });
                ui.add_enabled_ui(!self.is_busy(), |ui| {
                    let clicked = if selected_count > 0 {
                        widgets::button(ui, &pal, "Scan").clicked()
                    } else {
                        widgets::primary_button(ui, &pal, "Scan").clicked()
                    };
                    if clicked {
                        self.start_scan(&ctx, cx.settings);
                    }
                });
            });
        });

        let area = ui.available_rect_before_wrap();
        let left = Rect::from_min_max(area.min, pos2(area.left() + LEFT_WIDTH, area.bottom()));
        let right = Rect::from_min_max(pos2(left.right() + 16.0, area.top()), area.max);
        ui.scope_builder(UiBuilder::new().max_rect(left), |ui| {
            widgets::list_panel(&pal).show(ui, |ui| {
                ui.set_min_size(left.size() - vec2(2.0, 2.0));
                self.categories_ui(ui, cx);
            });
        });
        ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
            widgets::list_panel(&pal).show(ui, |ui| {
                ui.set_min_size(right.size() - vec2(2.0, 2.0));
                self.main_ui(ui, cx);
            });
        });
        ui.allocate_rect(area, Sense::hover());

        if self.confirm_fix {
            let body = if cx.settings.registry.backup_before_fix {
                format!(
                    "{} will be removed from the registry. A backup is saved first, so you can undo this from the Backups tab.",
                    plural(selected_count as u64, "entry").replace("entrys", "entries")
                )
            } else {
                format!(
                    "{} will be removed from the registry. Backups are switched off, so this cannot be undone.",
                    plural(selected_count as u64, "entry").replace("entrys", "entries")
                )
            };
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "registry-fix",
                "Fix selected issues?",
                &body,
                "Fix",
                false,
            ) {
                Confirm::Yes => {
                    self.confirm_fix = false;
                    self.start_fix(&ctx, cx.settings);
                }
                Confirm::No => self.confirm_fix = false,
                Confirm::Pending => {}
            }
        }
        if let Some(file) = self.confirm_restore.clone() {
            let body = "The entries saved in this backup are written back to the registry. Entries for all users need administrator rights.";
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "registry-restore",
                "Restore this backup?",
                body,
                "Restore",
                false,
            ) {
                Confirm::Yes => {
                    self.confirm_restore = None;
                    self.restoring.start(&ctx, "registry-restore", move |_| {
                        registry::import_reg_file(&file).map_err(|e| e.to_string())
                    });
                }
                Confirm::No => self.confirm_restore = None,
                Confirm::Pending => {}
            }
        }
    }

    fn categories_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let counts = self
            .issues
            .as_ref()
            .map(|issues| registry::count_by_category(issues));
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.label(text::section("What to look for"));
            });
        widgets::hairline(ui, &pal);
        ScrollArea::vertical()
            .id_salt("registry-categories")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(4.0);
                for category in Category::ALL {
                    let enabled = category_enabled(cx.settings, category);
                    let (row, response) = widgets::tree_row(ui, &pal, 32.0, Sense::click());
                    let check_rect = Rect::from_center_size(pos2(row.left() + 9.0, row.center().y), vec2(18.0, 18.0));
                    let state = if enabled { Check::On } else { Check::Off };
                    let clicked =
                        widgets::in_cell(ui, check_rect, |ui| widgets::check(ui, &pal, state, category.label()).clicked());
                    if clicked || response.clicked() {
                        let now = !enabled;
                        if now == category.default_enabled() {
                            cx.settings.registry.categories.remove(category.slug());
                        } else {
                            cx.settings.registry.categories.insert(category.slug().to_owned(), now);
                        }
                        cx.settings_changed = true;
                    }
                    let count = counts
                        .as_ref()
                        .and_then(|c| c.iter().find(|(cat, _)| *cat == category))
                        .map_or(0, |(_, n)| *n);
                    let mut right = row.right();
                    if count > 0 {
                        let count_rect = Rect::from_min_max(pos2(row.right() - 44.0, row.top()), row.max);
                        widgets::paint_text(ui, count_rect, &count.to_string(), text::semibold_font(13.0), pal.accent_text, Align::Max);
                        right = count_rect.left() - 6.0;
                    }
                    let name_rect = Rect::from_min_max(pos2(row.left() + 28.0, row.top()), pos2(right, row.bottom()));
                    let color = if enabled { pal.text } else { pal.text_muted };
                    widgets::paint_text(ui, name_rect, category.label(), text::body_font(14.0), color, Align::Min);
                    response.on_hover_text(category.description());
                }
                ui.add_space(10.0);
                widgets::hairline(ui, &pal);
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        if widgets::checkbox(
                            ui,
                            &pal,
                            &mut cx.settings.registry.backup_before_fix,
                            "Back up before fixing",
                        ) {
                            cx.settings_changed = true;
                        }
                        ui.label(text::small(
                            "Backups are ordinary .reg files. Restore one here, or by opening it in Windows.",
                            &pal,
                        ));
                    });
            });
    }

    fn main_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        if self.scanning.is_running() || self.fixing.is_running() {
            let (verb, progress) = if self.fixing.is_running() {
                ("Fixing", self.fixing.progress())
            } else {
                ("Scanning the registry", self.scanning.progress())
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
                ui.label(text::muted(
                    if message.is_empty() {
                        " ".to_owned()
                    } else {
                        message
                    },
                    &pal,
                ));
            });
            return;
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            if widgets::tabs(ui, &pal, &mut self.tab, &["Issues", "Backups"]) && self.tab == 1 {
                self.backups = None;
            }
        });
        if self.tab == 1 {
            self.backups_ui(ui, cx);
            return;
        }

        if let Some(report) = &self.last_fix {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let mut message = format!("{} fixed.", plural(report.fixed as u64, "issue"));
                    if report.skipped_needs_admin > 0 {
                        message.push_str(&format!(
                            " {} skipped because they need administrator rights.",
                            report.skipped_needs_admin
                        ));
                    }
                    if report.skipped_changed > 0 {
                        message.push_str(&format!(
                            " {} skipped because they changed since the scan.",
                            report.skipped_changed
                        ));
                    }
                    if !report.failed.is_empty() {
                        message
                            .push_str(&format!(" {} could not be removed.", report.failed.len()));
                    }
                    if report.backup_file.is_some() {
                        message.push_str(" A backup was saved.");
                    }
                    let needs_admin = report.skipped_needs_admin > 0 && !cx.elevated;
                    let action = needs_admin.then_some("Restart as administrator");
                    if widgets::banner(ui, &pal, Tone::Good, icon::CHECK_CIRCLE, &message, action) {
                        cx.request_elevation = true;
                    }
                });
        }

        let Some(issues) = &self.issues else {
            if widgets::empty_state(
                ui,
                &pal,
                icon::TREE_STRUCTURE,
                "No scan yet",
                "A scan only reads the registry. You review every finding before anything is changed.",
                Some("Scan"),
            ) {
                let ctx = ui.ctx().clone();
                self.start_scan(&ctx, cx.settings);
            }
            return;
        };
        if issues.is_empty() {
            widgets::empty_state(
                ui,
                &pal,
                icon::CHECK_CIRCLE,
                "No issues found",
                "The selected areas of the registry are tidy.",
                None,
            );
            return;
        }

        // Summary line and select-all.
        let all_selectable: Vec<&Issue> = issues
            .iter()
            .filter(|i| cx.elevated || !i.requires_admin)
            .collect();
        let state = Check::of(all_selectable.iter().map(|i| self.selected.contains(&i.id)));
        let admin_only = issues.len() - all_selectable.len();
        let mut toggle_all = false;
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
                        plural(issues.len() as u64, "issue")
                    )));
                    ui.label(text::small(
                        format!("found in {}", format_duration(self.scan_time)),
                        &pal,
                    ));
                });
                if admin_only > 0 {
                    ui.add_space(4.0);
                    let message = format!(
                        "{} affect all users and can only be fixed with administrator rights.",
                        plural(admin_only as u64, "issue")
                    );
                    if widgets::banner(
                        ui,
                        &pal,
                        Tone::Info,
                        icon::SHIELD,
                        &message,
                        Some("Restart as administrator"),
                    ) {
                        cx.request_elevation = true;
                    }
                }
            });
        if toggle_all {
            if state == Check::On {
                self.selected.clear();
            } else {
                self.selected = all_selectable.iter().map(|i| i.id.clone()).collect();
            }
        }

        let columns = Columns(vec![
            Col::Fixed(22.0),
            Col::Flex(1.7),
            Col::Flex(1.2),
            Col::Flex(1.5),
        ]);
        let mut sort = (usize::MAX, true);
        widgets::list_header(
            ui,
            &pal,
            &columns,
            &[
                ("", Align::Min, false),
                ("Problem", Align::Min, false),
                ("Data", Align::Min, false),
                ("Registry key", Align::Min, false),
            ],
            &mut sort,
        );
        let row_height = 34.0;
        let mut toggled: Option<String> = None;
        ScrollArea::vertical()
            .id_salt("registry-issues")
            .auto_shrink([false, false])
            .show_rows(ui, row_height, issues.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for index in range {
                    let issue = &issues[index];
                    let locked = issue.requires_admin && !cx.elevated;
                    let selected = self.selected.contains(&issue.id);
                    let (row, response) =
                        widgets::list_row(ui, &pal, row_height, selected, Sense::click());
                    let cells = columns.cells(row, 12.0);
                    if locked {
                        ui.painter().text(
                            cells[0].center(),
                            egui::Align2::CENTER_CENTER,
                            icon::SHIELD,
                            text::body_font(15.0),
                            pal.text_faint,
                        );
                    } else {
                        let state = if selected { Check::On } else { Check::Off };
                        let clicked = widgets::in_cell(ui, cells[0], |ui| {
                            widgets::check(ui, &pal, state, &issue.title).clicked()
                        });
                        if clicked || response.clicked() {
                            toggled = Some(issue.id.clone());
                        }
                    }
                    let title_color = if locked { pal.text_muted } else { pal.text };
                    let mut title_rect = cells[1];
                    if issue.confidence == Confidence::Review {
                        let pill_rect = Rect::from_min_max(
                            pos2(cells[1].right() - 64.0, cells[1].top()),
                            cells[1].max,
                        );
                        widgets::in_cell_right(ui, pill_rect, |ui| {
                            widgets::pill(ui, &pal, "Review", Tone::Warn)
                                .on_hover_text("Less certain. Check before including it.");
                        });
                        title_rect.set_right(pill_rect.left() - 6.0);
                    }
                    widgets::paint_text(
                        ui,
                        title_rect,
                        &issue.title,
                        text::body_font(14.0),
                        title_color,
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        cells[2],
                        &issue.detail,
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        cells[3],
                        &issue.location(),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Min,
                    );
                    let tip = if locked {
                        format!(
                            "{}\n{}\n\nNeeds administrator rights.",
                            issue.detail,
                            issue.location()
                        )
                    } else {
                        format!("{}\n{}", issue.detail, issue.location())
                    };
                    response.on_hover_text(tip);
                }
            });
        if let Some(id) = toggled
            && !self.selected.remove(&id)
        {
            self.selected.insert(id);
        }
    }

    fn backups_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let backups = self
            .backups
            .get_or_insert_with(registry::list_backups)
            .clone();
        if backups.is_empty() {
            widgets::empty_state(
                ui,
                &pal,
                icon::ARCHIVE,
                "No backups yet",
                "A backup is saved here each time you fix registry issues.",
                None,
            );
            return;
        }
        let columns = Columns(vec![
            Col::Flex(1.6),
            Col::Fixed(130.0),
            Col::Fixed(90.0),
            Col::Fixed(210.0),
        ]);
        let mut sort = (usize::MAX, true);
        widgets::list_header(
            ui,
            &pal,
            &columns,
            &[
                ("Backup", Align::Min, false),
                ("Saved", Align::Min, false),
                ("Size", Align::Max, false),
                ("", Align::Min, false),
            ],
            &mut sort,
        );
        ScrollArea::vertical()
            .id_salt("registry-backups")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for backup in &backups {
                    let (row, _) = widgets::list_row(ui, &pal, 40.0, false, Sense::hover());
                    let cells = columns.cells(row, 12.0);
                    widgets::paint_text(
                        ui,
                        cells[0],
                        &backup.label,
                        text::body_font(14.0),
                        pal.text,
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        cells[1],
                        &format_age(backup.created),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        cells[2],
                        &format_bytes(backup.size),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    widgets::in_cell_right(ui, cells[3], |ui| {
                        if widgets::quiet_button(ui, &pal, "Show in folder").clicked() {
                            let _ = zest_engine::core::process::hidden_command("explorer.exe")
                                .arg(format!("/select,{}", backup.path.display()))
                                .spawn();
                        }
                        ui.add_enabled_ui(!self.restoring.is_running(), |ui| {
                            if widgets::quiet_button(ui, &pal, "Restore").clicked() {
                                self.confirm_restore = Some(backup.path.clone());
                            }
                        });
                    });
                }
            });
    }
}
