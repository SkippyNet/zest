//! Disk analyzer: what is using the space, by type, by folder and by file.

use super::Cx;
use crate::job::Job;
use crate::theme::{Palette, mix, text};
use crate::widgets::{self, Col, Columns, Confirm, Segment};
use eframe::egui::{
    self, Align, Color32, Layout, Rect, ScrollArea, Sense, Ui, UiBuilder, pos2, vec2,
};
use egui_phosphor::regular as icon;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zest_engine::core::Progress;
use zest_engine::core::delete::{self, DeleteOptions};
use zest_engine::core::fmt::{format_bytes, format_count, format_duration, plural};
use zest_engine::disk::{
    self, Analysis, AnalyzeOptions, DriveInfo, FileCategory, FileEntry, FolderNode,
};

/// State of the Disk analyzer page.
#[derive(Default)]
pub struct DiskPage {
    drives: Option<Vec<DriveInfo>>,
    target: Option<PathBuf>,
    analyzing: Job<Result<Analysis, String>>,
    analysis: Option<Analysis>,
    tab: usize,
    /// Folder currently shown in the drill-down.
    current: Option<PathBuf>,
    confirm_delete: Option<FileEntry>,
}

/// One colour per file category, chosen to stay distinguishable on both themes.
pub fn category_color(category: FileCategory, pal: &Palette) -> Color32 {
    let base = match category {
        FileCategory::Pictures => Color32::from_rgb(0xE2, 0xB5, 0x4E),
        FileCategory::Music => Color32::from_rgb(0x4C, 0xBD, 0xAB),
        FileCategory::Video => Color32::from_rgb(0x74, 0xAC, 0xEA),
        FileCategory::Documents => Color32::from_rgb(0xB4, 0x8A, 0xE0),
        FileCategory::Archives => Color32::from_rgb(0xE0, 0x7A, 0x9A),
        FileCategory::Email => Color32::from_rgb(0xA8, 0xB8, 0x5C),
        FileCategory::Applications => Color32::from_rgb(0xE8, 0x93, 0x3F),
        FileCategory::Code => Color32::from_rgb(0x3D, 0x9F, 0xB0),
        FileCategory::System => Color32::from_rgb(0x8F, 0xA3, 0xB8),
        FileCategory::Other => Color32::from_rgb(0x6F, 0x7D, 0x8D),
    };
    if pal.dark {
        base
    } else {
        mix(base, Color32::BLACK, 0.18)
    }
}

fn show_in_folder(path: &Path) {
    let _ = zest_engine::core::process::hidden_command("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}

impl DiskPage {
    /// True while an analysis is running.
    pub fn is_busy(&self) -> bool {
        self.analyzing.is_running()
    }

    /// Fills the page with made-up results (documentation screenshots).
    pub fn load_demo(&mut self, drives: Vec<DriveInfo>, analysis: Analysis) {
        self.target = drives.first().map(|d| d.root.clone());
        self.current = analysis.tree.first().map(|n| n.path.clone());
        self.drives = Some(drives);
        self.analysis = Some(analysis);
    }

    /// Analyzes the selected drive or folder (read-only).
    pub fn start(&mut self, ctx: &egui::Context) {
        let Some(target) = self.target.clone().or_else(|| {
            self.drives
                .as_ref()
                .and_then(|d| d.iter().find(|d| d.is_system).or(d.first()))
                .map(|d| d.root.clone())
        }) else {
            return;
        };
        self.target = Some(target.clone());
        self.current = None;
        self.analyzing
            .start(ctx, "disk-analyze", move |progress: Progress| {
                disk::analyze(&AnalyzeOptions::for_path(target), &progress)
                    .map_err(|e| e.to_string())
            });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.analyzing.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(analysis) => {
                    self.current = analysis.tree.first().map(|n| n.path.clone());
                    self.analysis = Some(analysis);
                }
                Err(e) => cx.toasts.error(format!("Analysis stopped: {e}")),
            }
        }
        let drives = self
            .drives
            .get_or_insert_with(disk::list_local_drives)
            .clone();
        if self.target.is_none() {
            self.target = drives
                .iter()
                .find(|d| d.is_system)
                .or(drives.first())
                .map(|d| d.root.clone());
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Disk analyzer",
                    "See what is using your space. Analyzing only reads; nothing is changed.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(!self.is_busy() && self.target.is_some(), |ui| {
                    if widgets::primary_button(ui, &pal, "Analyze").clicked() {
                        self.start(&ctx);
                    }
                });
                ui.add_enabled_ui(!self.is_busy(), |ui| {
                    if widgets::icon_text_button(ui, &pal, icon::FOLDER_OPEN, "Choose folder")
                        .clicked()
                        && let Some(folder) = rfd::FileDialog::new()
                            .set_title("Choose a folder to analyze")
                            .pick_folder()
                    {
                        self.target = Some(folder);
                    }
                });
            });
        });

        // Drives, each as a selectable usage bar.
        let row = ui.available_rect_before_wrap();
        let count = drives.len().clamp(1, 4) as f32;
        let gap = 14.0;
        let width = ((row.width() - gap * (count - 1.0)) / count).min(420.0);
        let height = 92.0;
        for (index, drive) in drives.iter().take(4).enumerate() {
            let rect = Rect::from_min_size(
                pos2(row.left() + (width + gap) * index as f32, row.top()),
                vec2(width, height),
            );
            let selected = self.target.as_deref() == Some(drive.root.as_path());
            let response = ui.interact(rect, ui.id().with(("drive", index)), Sense::click());
            ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                let mut frame = widgets::panel(&pal).inner_margin(egui::Margin::symmetric(16, 12));
                if selected {
                    frame = frame.stroke(egui::Stroke::new(1.5, pal.accent));
                } else if response.hovered() {
                    frame = frame.fill(pal.surface_hi);
                }
                frame.show(ui, |ui| {
                    ui.set_min_size(rect.size() - vec2(34.0, 26.0));
                    ui.horizontal(|ui| {
                        ui.label(text::strong(drive.display_name()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(text::small(
                                match drive.is_ssd {
                                    Some(true) => "SSD",
                                    Some(false) => "Hard disk",
                                    None => drive.kind.label(),
                                },
                                &pal,
                            ));
                        });
                    });
                    widgets::usage_bar(
                        ui,
                        &pal,
                        &[(drive.used_fraction(), Segment::Solid)],
                        10.0,
                        None,
                    );
                    ui.label(text::small(
                        format!(
                            "{} free of {}",
                            format_bytes(drive.free_bytes),
                            format_bytes(drive.total_bytes)
                        ),
                        &pal,
                    ));
                });
            });
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.target = Some(drive.root.clone());
            }
        }
        ui.allocate_rect(
            Rect::from_min_size(row.min, vec2(row.width(), height)),
            Sense::hover(),
        );
        ui.add_space(6.0);

        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - vec2(2.0, 2.0));
            if self.analyzing.is_running() {
                let progress = self.analyzing.progress();
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.25).clamp(30.0, 160.0));
                    ui.label(text::figure("Analyzing…", 28.0));
                    if let Some(p) = progress {
                        ui.label(text::strong(format!(
                            "{} in {}",
                            format_bytes(p.bytes()),
                            plural(p.done(), "file")
                        )));
                        ui.scope(|ui| {
                            ui.set_max_width(420.0);
                            widgets::progress_bar(ui, &pal, None, 8.0);
                            ui.add(egui::Label::new(text::small(p.message(), &pal)).truncate());
                        });
                    }
                    ui.add_space(6.0);
                    if widgets::button(ui, &pal, "Cancel").clicked() {
                        self.analyzing.cancel();
                    }
                });
                return;
            }
            let Some(analysis) = &self.analysis else {
                let target = self
                    .target
                    .as_ref()
                    .map(|t| t.display().to_string())
                    .unwrap_or_default();
                if widgets::empty_state(
                    ui,
                    &pal,
                    icon::CHART_PIE_SLICE,
                    "Find out where the space went",
                    &format!(
                        "Ready to analyze {target}. A whole drive usually takes a few seconds."
                    ),
                    Some("Analyze"),
                ) {
                    self.start(&ctx);
                }
                return;
            };

            // Summary and composition.
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(18, 14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.label(text::figure(format_bytes(analysis.total_bytes), 32.0));
                        ui.vertical(|ui| {
                            ui.add_space(4.0);
                            ui.label(text::muted(
                                format!(
                                    "in {} and {}",
                                    plural(analysis.total_files, "file"),
                                    plural(analysis.total_dirs, "folder")
                                ),
                                &pal,
                            ));
                            ui.add_space(-6.0);
                            ui.label(text::small(
                                format!(
                                    "{} analyzed in {}",
                                    analysis
                                        .roots
                                        .first()
                                        .map(|r| r.display().to_string())
                                        .unwrap_or_default(),
                                    format_duration(Duration::from_millis(analysis.elapsed_ms))
                                ),
                                &pal,
                            ));
                        });
                    });
                    let total = analysis.total_bytes.max(1) as f32;
                    let sections: Vec<(f32, Segment)> = analysis
                        .categories
                        .iter()
                        .filter(|c| c.bytes > 0)
                        .map(|c| {
                            (
                                c.bytes as f32 / total,
                                Segment::Flat(category_color(c.category, &pal)),
                            )
                        })
                        .collect();
                    ui.add_space(2.0);
                    widgets::usage_bar(ui, &pal, &sections, 14.0, None);
                    ui.add_space(2.0);
                    let items: Vec<(Color32, &str, String)> = analysis
                        .categories
                        .iter()
                        .filter(|c| c.bytes > 0)
                        .map(|c| {
                            (
                                category_color(c.category, &pal),
                                c.category.label(),
                                format_bytes(c.bytes),
                            )
                        })
                        .collect();
                    widgets::legends(ui, &pal, &items);
                });
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                widgets::tabs(
                    ui,
                    &pal,
                    &mut self.tab,
                    &["Folders", "Largest files", "File types"],
                );
            });
            match self.tab {
                0 => folders_ui(ui, &pal, analysis, &mut self.current),
                1 => {
                    if let Some(file) = files_ui(ui, &pal, analysis) {
                        self.confirm_delete = Some(file);
                    }
                }
                _ => types_ui(ui, &pal, analysis),
            }
        });

        if let Some(file) = self.confirm_delete.clone() {
            let recycle = cx.settings.general.tools_use_recycle_bin;
            let body = if recycle {
                format!(
                    "{} ({}) is moved to the Recycle Bin.",
                    file.path.display(),
                    format_bytes(file.size)
                )
            } else {
                format!(
                    "{} ({}) is deleted permanently.",
                    file.path.display(),
                    format_bytes(file.size)
                )
            };
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "disk-delete",
                "Delete this file?",
                &body,
                "Delete",
                true,
            ) {
                Confirm::Yes => {
                    self.confirm_delete = None;
                    let options = if recycle {
                        DeleteOptions::recycle()
                    } else {
                        DeleteOptions::permanent()
                    };
                    match delete::delete_file(&file.path, &options) {
                        Ok(bytes) => {
                            if let Some(analysis) = &mut self.analysis {
                                analysis.largest_files.retain(|f| f.path != file.path);
                            }
                            cx.toasts.good(format!("{} freed", format_bytes(bytes)));
                        }
                        Err(e) => cx.toasts.error(format!("Could not delete the file: {e}")),
                    }
                }
                Confirm::No => self.confirm_delete = None,
                Confirm::Pending => {}
            }
        }
    }
}

fn share_bar(ui: &Ui, pal: &Palette, rect: Rect, share: f32, color: Color32) {
    let track = Rect::from_min_max(
        pos2(rect.left(), rect.center().y - 3.0),
        pos2(rect.right(), rect.center().y + 3.0),
    );
    ui.painter().rect_filled(track, 3.0, pal.well);
    let mut fill = track;
    fill.set_width((track.width() * share.clamp(0.0, 1.0)).max(2.0));
    ui.painter().rect_filled(fill, 3.0, color);
}

fn folders_ui(ui: &mut Ui, pal: &Palette, analysis: &Analysis, current: &mut Option<PathBuf>) {
    let Some(root) = analysis.tree.first() else {
        return;
    };
    let node: &FolderNode = current
        .as_ref()
        .and_then(|p| analysis.find_folder(p))
        .unwrap_or(root);

    // Breadcrumb from the analyzed root down to the current folder.
    let mut goto: Option<PathBuf> = None;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let mut crumbs: Vec<&Path> = node
                    .path
                    .ancestors()
                    .take_while(|p| p.starts_with(&root.path))
                    .collect();
                crumbs.reverse();
                for (i, crumb) in crumbs.iter().enumerate() {
                    let label = if i == 0 {
                        root.path.display().to_string()
                    } else {
                        crumb
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    };
                    if i + 1 == crumbs.len() {
                        ui.add_space(10.0);
                        ui.label(text::strong(label));
                    } else {
                        if widgets::quiet_button(ui, pal, &label).clicked() {
                            goto = Some(crumb.to_path_buf());
                        }
                        ui.label(text::icon(icon::CARET_RIGHT, 12.0).color(pal.text_faint));
                    }
                }
            });
        });
    widgets::hairline(ui, pal);

    let total = node.bytes.max(1) as f32;
    let columns = Columns(vec![
        Col::Fixed(22.0),
        Col::Flex(1.4),
        Col::Flex(0.9),
        Col::Fixed(110.0),
        Col::Fixed(90.0),
    ]);
    ScrollArea::vertical()
        .id_salt(("folders", &node.path))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for child in &node.children {
                let (row, response) = widgets::list_row(ui, pal, 38.0, false, Sense::click());
                let cells = columns.cells(row, 12.0);
                ui.painter().text(
                    cells[0].center(),
                    egui::Align2::CENTER_CENTER,
                    icon::FOLDER,
                    text::body_font(17.0),
                    pal.accent_text,
                );
                widgets::paint_text(
                    ui,
                    cells[1],
                    &child.name,
                    text::body_font(14.0),
                    pal.text,
                    Align::Min,
                );
                share_bar(ui, pal, cells[2], child.bytes as f32 / total, pal.accent);
                widgets::paint_text(
                    ui,
                    cells[3],
                    &plural(child.files, "file"),
                    text::body_font(13.0),
                    pal.text_muted,
                    Align::Max,
                );
                widgets::paint_text(
                    ui,
                    cells[4],
                    &format_bytes(child.bytes),
                    text::body_font(14.0),
                    pal.text,
                    Align::Max,
                );
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    goto = Some(child.path.clone());
                }
            }
            for file in &node.top_files {
                let (row, response) = widgets::list_row(ui, pal, 38.0, false, Sense::click());
                let cells = columns.cells(row, 12.0);
                ui.painter().text(
                    cells[0].center(),
                    egui::Align2::CENTER_CENTER,
                    icon::FILE,
                    text::body_font(17.0),
                    category_color(file.category, pal),
                );
                let name = file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                widgets::paint_text(
                    ui,
                    cells[1],
                    &name,
                    text::body_font(14.0),
                    pal.text,
                    Align::Min,
                );
                share_bar(
                    ui,
                    pal,
                    cells[2],
                    file.size as f32 / total,
                    category_color(file.category, pal),
                );
                widgets::paint_text(
                    ui,
                    cells[3],
                    file.category.label(),
                    text::body_font(13.0),
                    pal.text_muted,
                    Align::Max,
                );
                widgets::paint_text(
                    ui,
                    cells[4],
                    &format_bytes(file.size),
                    text::body_font(14.0),
                    pal.text,
                    Align::Max,
                );
                if response
                    .on_hover_text("Show in File Explorer")
                    .double_clicked()
                {
                    show_in_folder(&file.path);
                }
            }
            if node.unlisted_bytes() > 0 {
                let (row, _) = widgets::list_row(ui, pal, 38.0, false, Sense::hover());
                let cells = columns.cells(row, 12.0);
                widgets::paint_text(
                    ui,
                    cells[1],
                    "Everything else here",
                    text::body_font(14.0),
                    pal.text_muted,
                    Align::Min,
                );
                share_bar(
                    ui,
                    pal,
                    cells[2],
                    node.unlisted_bytes() as f32 / total,
                    pal.text_faint,
                );
                widgets::paint_text(
                    ui,
                    cells[4],
                    &format_bytes(node.unlisted_bytes()),
                    text::body_font(14.0),
                    pal.text_muted,
                    Align::Max,
                );
            }
        });
    if goto.is_some() {
        *current = goto;
    }
}

/// Lists the largest files. Returns a file the user asked to delete.
fn files_ui(ui: &mut Ui, pal: &Palette, analysis: &Analysis) -> Option<FileEntry> {
    let mut delete = None;
    let columns = Columns(vec![
        Col::Flex(1.1),
        Col::Flex(1.5),
        Col::Fixed(96.0),
        Col::Fixed(84.0),
        Col::Fixed(72.0),
    ]);
    let mut sort = (usize::MAX, true);
    widgets::list_header(
        ui,
        pal,
        &columns,
        &[
            ("File", Align::Min, false),
            ("Folder", Align::Min, false),
            ("Type", Align::Min, false),
            ("Size", Align::Max, false),
            ("", Align::Min, false),
        ],
        &mut sort,
    );
    let row_height = 38.0;
    ScrollArea::vertical()
        .id_salt("largest")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, analysis.largest_files.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for index in range {
                let file = &analysis.largest_files[index];
                let (row, response) = widgets::list_row(ui, pal, row_height, false, Sense::hover());
                let cells = columns.cells(row, 12.0);
                let name = file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let folder = file
                    .path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                widgets::paint_text(
                    ui,
                    cells[0],
                    &name,
                    text::body_font(14.0),
                    pal.text,
                    Align::Min,
                );
                widgets::paint_text(
                    ui,
                    cells[1],
                    &folder,
                    text::body_font(13.0),
                    pal.text_muted,
                    Align::Min,
                );
                widgets::paint_text(
                    ui,
                    cells[2],
                    file.category.label(),
                    text::body_font(13.0),
                    category_color(file.category, pal),
                    Align::Min,
                );
                widgets::paint_text(
                    ui,
                    cells[3],
                    &format_bytes(file.size),
                    text::body_font(14.0),
                    pal.text,
                    Align::Max,
                );
                widgets::in_cell_right(ui, cells[4], |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if widgets::icon_button(ui, pal, icon::TRASH, "Delete this file").clicked() {
                        delete = Some(file.clone());
                    }
                    if widgets::icon_button(ui, pal, icon::FOLDER_OPEN, "Show in File Explorer")
                        .clicked()
                    {
                        show_in_folder(&file.path);
                    }
                });
                response.on_hover_text(file.path.display().to_string());
            }
        });
    delete
}

fn types_ui(ui: &mut Ui, pal: &Palette, analysis: &Analysis) {
    let columns = Columns(vec![
        Col::Flex(1.0),
        Col::Flex(1.0),
        Col::Flex(1.2),
        Col::Fixed(110.0),
        Col::Fixed(90.0),
    ]);
    let mut sort = (usize::MAX, true);
    widgets::list_header(
        ui,
        pal,
        &columns,
        &[
            ("Extension", Align::Min, false),
            ("Type", Align::Min, false),
            ("", Align::Min, false),
            ("Files", Align::Max, false),
            ("Size", Align::Max, false),
        ],
        &mut sort,
    );
    let total = analysis.total_bytes.max(1) as f32;
    let row_height = 34.0;
    ScrollArea::vertical()
        .id_salt("types")
        .auto_shrink([false, false])
        .show_rows(ui, row_height, analysis.extensions.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for index in range {
                let stat = &analysis.extensions[index];
                let (row, _) = widgets::list_row(ui, pal, row_height, false, Sense::hover());
                let cells = columns.cells(row, 12.0);
                let label = match stat.extension.as_str() {
                    "" => "(no extension)".to_owned(),
                    "*" => "All other extensions".to_owned(),
                    ext => format!(".{ext}"),
                };
                widgets::paint_text(
                    ui,
                    cells[0],
                    &label,
                    text::body_font(14.0),
                    pal.text,
                    Align::Min,
                );
                widgets::paint_text(
                    ui,
                    cells[1],
                    stat.category.label(),
                    text::body_font(13.0),
                    pal.text_muted,
                    Align::Min,
                );
                share_bar(
                    ui,
                    pal,
                    cells[2],
                    stat.bytes as f32 / total,
                    category_color(stat.category, pal),
                );
                widgets::paint_text(
                    ui,
                    cells[3],
                    &format_count(stat.files),
                    text::body_font(13.0),
                    pal.text_muted,
                    Align::Max,
                );
                widgets::paint_text(
                    ui,
                    cells[4],
                    &format_bytes(stat.bytes),
                    text::body_font(14.0),
                    pal.text,
                    Align::Max,
                );
            }
        });
}
