//! Duplicate finder: byte-identical files, with one copy always kept.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Check, Confirm};
use eframe::egui::{self, Align, Layout, Rect, ScrollArea, Sense, Ui, pos2, vec2};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use zest_engine::core::Progress;
use zest_engine::core::delete::DeleteOptions;
use zest_engine::core::fmt::{format_age, format_bytes, format_duration, plural};
use zest_engine::core::paths::UserDirs;
use zest_engine::disk::{
    self, DuplicateDeleteReport, DuplicateGroup, DuplicateOptions, DuplicateReport, KeepStrategy,
};

const MIN_SIZES: [(&str, u64); 5] = [
    ("Any size", 1),
    ("8 KB", 8 * 1024),
    ("1 MB", 1024 * 1024),
    ("10 MB", 10 * 1024 * 1024),
    ("100 MB", 100 * 1024 * 1024),
];

#[derive(Clone, Copy)]
enum Row {
    Group(usize),
    File(usize, usize),
}

/// State of the Duplicate finder page.
pub struct DuplicatesPage {
    folders: Vec<PathBuf>,
    match_name: bool,
    min_size: usize,
    scanning: Job<Result<DuplicateReport, String>>,
    report: Option<DuplicateReport>,
    rows: Vec<Row>,
    selected: HashSet<PathBuf>,
    deleting: Job<Result<(DuplicateDeleteReport, Vec<PathBuf>), String>>,
    confirm: bool,
}

impl Default for DuplicatesPage {
    fn default() -> Self {
        let profile = UserDirs::current().profile;
        let folders = [
            "Documents",
            "Downloads",
            "Pictures",
            "Desktop",
            "Music",
            "Videos",
        ]
        .iter()
        .map(|name| profile.join(name))
        .filter(|p| p.is_dir())
        .collect();
        DuplicatesPage {
            folders,
            match_name: false,
            min_size: 1,
            scanning: Job::default(),
            report: None,
            rows: Vec::new(),
            selected: HashSet::new(),
            deleting: Job::default(),
            confirm: false,
        }
    }
}

fn build_rows(groups: &[DuplicateGroup]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (g, group) in groups.iter().enumerate() {
        rows.push(Row::Group(g));
        rows.extend((0..group.files.len()).map(|f| Row::File(g, f)));
    }
    rows
}

impl DuplicatesPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.scanning.is_running() || self.deleting.is_running()
    }

    /// Scans the chosen folders (read-only).
    pub fn start(&mut self, ctx: &egui::Context) {
        if self.is_busy() || self.folders.is_empty() {
            return;
        }
        let mut options = DuplicateOptions::for_roots(self.folders.clone());
        options.match_name = self.match_name;
        options.min_size = MIN_SIZES[self.min_size].1;
        self.selected.clear();
        self.scanning
            .start(ctx, "duplicates", move |progress: Progress| {
                disk::find_duplicates(&options, &progress).map_err(|e| e.to_string())
            });
    }

    fn select_with(&mut self, strategy: &KeepStrategy) {
        if let Some(report) = &self.report {
            self.selected = disk::select_for_removal(&report.groups, strategy)
                .into_iter()
                .collect();
        }
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.scanning.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(report) => {
                    self.rows = build_rows(&report.groups);
                    self.report = Some(report);
                }
                Err(e) => cx.toasts.error(format!("The scan stopped: {e}")),
            }
        }
        if let Some(result) = self.deleting.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok((outcome, requested)) => {
                    let mut kept: HashSet<&PathBuf> = outcome.kept_last_copy.iter().collect();
                    kept.extend(outcome.changed.iter());
                    kept.extend(outcome.not_duplicates.iter());
                    kept.extend(outcome.deleted.failures.iter().map(|(p, _)| p));
                    let removed: HashSet<&PathBuf> =
                        requested.iter().filter(|p| !kept.contains(p)).collect();
                    if let Some(report) = &mut self.report {
                        for group in &mut report.groups {
                            group.files.retain(|f| !removed.contains(&f.path));
                        }
                        report.groups.retain(|g| g.files.len() > 1);
                        report.wasted_bytes =
                            report.groups.iter().map(DuplicateGroup::wasted_bytes).sum();
                        self.rows = build_rows(&report.groups);
                    }
                    self.selected.clear();
                    let mut message = format!(
                        "{} removed, {} freed.",
                        plural(outcome.deleted.files, "duplicate"),
                        format_bytes(outcome.deleted.bytes)
                    );
                    if !outcome.kept_last_copy.is_empty() {
                        message.push_str(&format!(
                            " {} kept as the last remaining copy.",
                            plural(outcome.kept_last_copy.len() as u64, "file")
                        ));
                    }
                    if outcome.deleted.failed > 0 {
                        message.push_str(&format!(
                            " {} could not be removed.",
                            outcome.deleted.failed
                        ));
                    }
                    cx.toasts.good(message);
                }
                Err(e) => cx.toasts.error(format!("Nothing was deleted: {e}")),
            }
        }

        let selected_bytes: u64 = self
            .report
            .iter()
            .flat_map(|r| r.groups.iter())
            .flat_map(|g| g.files.iter())
            .filter(|f| self.selected.contains(&f.path))
            .map(|f| f.size)
            .sum();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Duplicate finder",
                    "Finds files with identical contents. One copy of every file is always kept, whatever you select.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(!self.selected.is_empty() && !self.is_busy(), |ui| {
                    if widgets::primary_button(ui, &pal, "Delete selected").clicked() {
                        self.confirm = true;
                    }
                });
                ui.add_enabled_ui(!self.is_busy() && !self.folders.is_empty(), |ui| {
                    let clicked = if self.selected.is_empty() {
                        widgets::primary_button(ui, &pal, "Scan").clicked()
                    } else {
                        widgets::button(ui, &pal, "Scan").clicked()
                    };
                    if clicked {
                        self.start(&ctx);
                    }
                });
            });
        });

        // Where to look and how to match.
        widgets::panel(&pal)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut remove: Option<usize> = None;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    ui.label(text::strong("Look in"));
                    for (index, folder) in self.folders.iter().enumerate() {
                        egui::Frame::new()
                            .fill(pal.well)
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(10, 3))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label(text::body(folder.display().to_string()));
                                let close = ui.add(
                                    egui::Label::new(
                                        text::icon(icon::X, 13.0).color(pal.text_muted),
                                    )
                                    .sense(Sense::click()),
                                );
                                if close
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text("Do not look here")
                                    .clicked()
                                {
                                    remove = Some(index);
                                }
                            });
                    }
                    if widgets::quiet_button(ui, &pal, "Add folder").clicked()
                        && let Some(folder) = rfd::FileDialog::new()
                            .set_title("Choose a folder to scan")
                            .pick_folder()
                        && !self.folders.contains(&folder)
                    {
                        self.folders.push(folder);
                    }
                });
                if let Some(index) = remove {
                    self.folders.remove(index);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(text::muted("Ignore files smaller than", &pal));
                    egui::ComboBox::from_id_salt("dupe-min-size")
                        .selected_text(MIN_SIZES[self.min_size].0)
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for (index, (label, _)) in MIN_SIZES.iter().enumerate() {
                                ui.selectable_value(&mut self.min_size, index, *label);
                            }
                        });
                    ui.add_space(12.0);
                    widgets::checkbox(ui, &pal, &mut self.match_name, "Names must match too");
                });
            });
        ui.add_space(6.0);

        let mut toggle: Option<PathBuf> = None;
        let mut strategy: Option<Option<KeepStrategy>> = None;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - vec2(2.0, 2.0));
            if self.is_busy() {
                let (verb, progress) = if self.deleting.is_running() {
                    ("Deleting", self.deleting.progress())
                } else {
                    ("Comparing files", self.scanning.progress())
                };
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.25).clamp(30.0, 160.0));
                    ui.label(text::figure(format!("{verb}…"), 28.0));
                    ui.scope(|ui| {
                        ui.set_max_width(420.0);
                        widgets::progress_bar(ui, &pal, progress.and_then(|p| p.fraction()), 8.0);
                    });
                    let message = progress.map(|p| p.message()).unwrap_or_default();
                    ui.label(text::muted(if message.is_empty() { " ".to_owned() } else { message }, &pal));
                    ui.add_space(6.0);
                    if self.scanning.is_running() && widgets::button(ui, &pal, "Cancel").clicked() {
                        self.scanning.cancel();
                    }
                });
                return;
            }
            let Some(report) = &self.report else {
                if widgets::empty_state(
                    ui,
                    &pal,
                    icon::COPY,
                    "Find files you have more than once",
                    "Files are compared by content, so renamed copies are found too. Scanning only reads.",
                    Some("Scan"),
                ) {
                    self.start(&ctx);
                }
                return;
            };
            if report.groups.is_empty() {
                widgets::empty_state(
                    ui,
                    &pal,
                    icon::CHECK_CIRCLE,
                    "No duplicates",
                    "Every file in the chosen folders is unique.",
                    None,
                );
                return;
            }
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(text::strong(format!(
                            "{} wasted by {}",
                            format_bytes(report.wasted_bytes),
                            plural(report.groups.len() as u64, "set")
                        )));
                        ui.label(text::small(
                            format!(
                                "{} compared in {}",
                                plural(report.files_scanned, "file"),
                                format_duration(Duration::from_millis(report.elapsed_ms))
                            ),
                            &pal,
                        ));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            if widgets::quiet_button(ui, &pal, "Select none").clicked() {
                                strategy = Some(None);
                            }
                            if widgets::quiet_button(ui, &pal, "Keep shortest path").clicked() {
                                strategy = Some(Some(KeepStrategy::ShortestPath));
                            }
                            if widgets::quiet_button(ui, &pal, "Keep oldest").clicked() {
                                strategy = Some(Some(KeepStrategy::Oldest));
                            }
                            if widgets::quiet_button(ui, &pal, "Keep newest").clicked() {
                                strategy = Some(Some(KeepStrategy::Newest));
                            }
                        });
                    });
                });
            widgets::hairline(ui, &pal);

            let row_height = 34.0;
            ScrollArea::vertical()
                .id_salt("duplicates")
                .auto_shrink([false, false])
                .show_rows(ui, row_height, self.rows.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for index in range {
                        match self.rows[index] {
                            Row::Group(g) => {
                                let Some(group) = report.groups.get(g) else { continue };
                                let (row, _) = widgets::list_row(ui, &pal, row_height, false, Sense::hover());
                                ui.painter().rect_filled(row.expand2(vec2(widgets::ROW_PAD, 0.0)), 0.0, pal.tint(pal.text, 0.035));
                                let name = group
                                    .files
                                    .first()
                                    .and_then(|f| f.path.file_name())
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                let right = Rect::from_min_max(pos2(row.right() - 300.0, row.top()), row.max);
                                widgets::paint_text(
                                    ui,
                                    right,
                                    &format!(
                                        "{} copies of {}, {} wasted",
                                        group.files.len(),
                                        format_bytes(group.size),
                                        format_bytes(group.wasted_bytes())
                                    ),
                                    text::body_font(13.0),
                                    pal.text_muted,
                                    Align::Max,
                                );
                                let left = Rect::from_min_max(row.min, pos2(right.left() - 8.0, row.bottom()));
                                widgets::paint_text(ui, left, &name, text::semibold_font(14.0), pal.text, Align::Min);
                            }
                            Row::File(g, f) => {
                                let Some(file) = report.groups.get(g).and_then(|group| group.files.get(f)) else { continue };
                                let is_selected = self.selected.contains(&file.path);
                                let (row, response) = widgets::list_row(ui, &pal, row_height, is_selected, Sense::click());
                                let check_rect = Rect::from_center_size(pos2(row.left() + 9.0, row.center().y), vec2(18.0, 18.0));
                                let state = if is_selected { Check::On } else { Check::Off };
                                let clicked = widgets::in_cell(ui, check_rect, |ui| {
                                    widgets::check(ui, &pal, state, &file.path.display().to_string()).clicked()
                                });
                                if clicked || response.clicked() {
                                    toggle = Some(file.path.clone());
                                }
                                let age = file.modified.map(format_age).unwrap_or_default();
                                let age_rect = Rect::from_min_max(pos2(row.right() - 130.0, row.top()), row.max);
                                widgets::paint_text(ui, age_rect, &age, text::body_font(13.0), pal.text_muted, Align::Max);
                                let path_rect = Rect::from_min_max(pos2(row.left() + 30.0, row.top()), pos2(age_rect.left() - 8.0, row.bottom()));
                                let color = if is_selected { pal.text } else { pal.text_muted };
                                widgets::paint_text(ui, path_rect, &file.path.display().to_string(), text::body_font(13.0), color, Align::Min);
                            }
                        }
                    }
                });
        });
        if let Some(path) = toggle
            && !self.selected.remove(&path)
        {
            self.selected.insert(path);
        }
        match strategy {
            Some(Some(keep)) => self.select_with(&keep),
            Some(None) => self.selected.clear(),
            None => {}
        }

        if self.confirm {
            let recycle = cx.settings.general.tools_use_recycle_bin;
            let body = format!(
                "{} ({}) will be {}. If you selected every copy of a file, one is kept.",
                plural(self.selected.len() as u64, "file"),
                format_bytes(selected_bytes),
                if recycle {
                    "moved to the Recycle Bin"
                } else {
                    "deleted permanently"
                }
            );
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "dupes-delete",
                "Delete selected duplicates?",
                &body,
                "Delete",
                !recycle,
            ) {
                Confirm::Yes => {
                    self.confirm = false;
                    if let Some(report) = &self.report {
                        let groups = report.groups.clone();
                        let remove: Vec<PathBuf> = self.selected.iter().cloned().collect();
                        let options = if recycle {
                            DeleteOptions::recycle()
                        } else {
                            DeleteOptions::permanent()
                        };
                        self.deleting.start(
                            &ctx,
                            "duplicates-delete",
                            move |progress: Progress| {
                                disk::delete_duplicates(&groups, &remove, &options, &progress)
                                    .map(|outcome| (outcome, remove))
                                    .map_err(|e| e.to_string())
                            },
                        );
                    }
                }
                Confirm::No => self.confirm = false,
                Confirm::Pending => {}
            }
        }
    }
}
