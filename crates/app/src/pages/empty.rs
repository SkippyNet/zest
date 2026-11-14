//! Empty folders: find and remove folders that hold nothing.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Check, Col, Columns, Confirm};
use eframe::egui::{self, Align, Layout, Rect, ScrollArea, Sense, Ui, pos2, vec2};
use egui_phosphor::regular as icon;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zest_engine::Settings;
use zest_engine::core::Progress;
use zest_engine::core::delete::DeleteReport;
use zest_engine::core::fmt::{format_count, format_duration, plural};
use zest_engine::core::paths::UserDirs;
use zest_engine::core::process::hidden_command;
use zest_engine::disk::{self, EmptyFolderOptions};

/// What a finished scan found.
struct Found {
    folders: Vec<PathBuf>,
    /// Folders the scan looked into.
    examined: u64,
    elapsed: Duration,
}

/// State of the Empty folders page.
pub struct EmptyPage {
    roots: Vec<PathBuf>,
    include_hidden: bool,
    scanning: Job<Result<Found, String>>,
    found: Option<Found>,
    selected: HashSet<PathBuf>,
    deleting: Job<(DeleteReport, Vec<PathBuf>)>,
    confirm: bool,
}

impl Default for EmptyPage {
    fn default() -> Self {
        EmptyPage {
            roots: vec![UserDirs::current().profile],
            include_hidden: false,
            scanning: Job::default(),
            found: None,
            selected: HashSet::new(),
            deleting: Job::default(),
            confirm: false,
        }
    }
}

/// Folder name and the folder it sits in, for the two columns of the list.
fn split(path: &Path) -> (String, String) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let parent = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    (name, parent)
}

/// Drops the removed folders from the list; folders that could not be
/// removed stay, so they can be looked at.
fn after_delete(found: &mut Vec<PathBuf>, requested: &[PathBuf], report: &DeleteReport) {
    let kept: HashSet<&PathBuf> = report.failures.iter().map(|(path, _)| path).collect();
    // The report lists only its first failures by name. When there were
    // more, which of the remaining folders failed is unknown: keep them all
    // rather than hide a folder that still exists.
    let all_named = report.failed as usize == report.failures.len();
    let removed: HashSet<&PathBuf> = requested
        .iter()
        .filter(|path| all_named && !kept.contains(path))
        .collect();
    found.retain(|path| !removed.contains(path));
}

impl EmptyPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.scanning.is_running() || self.deleting.is_running()
    }

    /// Starts the page's read-only analysis (used by screenshot mode).
    pub fn start_scan(&mut self, ctx: &egui::Context, _settings: &Settings) {
        if self.is_busy() || self.roots.is_empty() {
            return;
        }
        let options = EmptyFolderOptions {
            include_hidden: self.include_hidden,
            ..EmptyFolderOptions::for_roots(self.roots.clone())
        };
        self.selected.clear();
        self.scanning
            .start(ctx, "empty-folders", move |progress: Progress| {
                let started = std::time::Instant::now();
                disk::find_empty_folders(&options, &progress)
                    .map(|folders| Found {
                        folders,
                        examined: progress.done(),
                        elapsed: started.elapsed(),
                    })
                    .map_err(|e| e.to_string())
            });
    }

    /// Fills the page with made-up data for documentation screenshots.
    pub fn load_demo(&mut self) {
        let base = PathBuf::from(r"C:\Users\Alex");
        self.roots = vec![base.clone()];
        let folders: Vec<PathBuf> = [
            r"Desktop\New folder",
            r"Desktop\New folder (2)",
            r"Documents\Scans\2019",
            r"Documents\Invoices\Drafts",
            r"Documents\Old laptop\Desktop\Shortcuts",
            r"Downloads\setup_extracted",
            r"Downloads\photos-export",
            r"Music\Playlists",
            r"Pictures\Camera Roll\2021-08",
            r"Pictures\Camera Roll\2022-01",
            r"Pictures\Screenshots\Old",
            r"Projects\garden-plan\exports",
            r"Projects\website\build\cache",
            r"Videos\Captures\Raw",
        ]
        .iter()
        .map(|relative| base.join(relative))
        .collect();
        self.selected = folders.iter().take(9).cloned().collect();
        self.found = Some(Found {
            folders,
            examined: 18_412,
            elapsed: Duration::from_millis(640),
        });
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.scanning.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(found) => self.found = Some(found),
                Err(e) => cx.toasts.error(format!("The scan stopped: {e}")),
            }
        }
        if let Some(result) = self.deleting.poll(&ctx) {
            match result {
                Ok((report, requested)) => {
                    if let Some(found) = &mut self.found {
                        after_delete(&mut found.folders, &requested, &report);
                    }
                    self.selected.clear();
                    let mut message = format!("{} removed.", plural(report.dirs, "folder"));
                    if report.failed > 0 {
                        message.push_str(&format!(
                            " {} left in place: {}.",
                            format_count(report.failed),
                            report
                                .failures
                                .first()
                                .map_or("see the list", |(_, reason)| reason.as_str())
                        ));
                    }
                    if report.dirs > 0 {
                        cx.toasts.good(message);
                    } else {
                        cx.toasts.error(message);
                    }
                }
                Err(e) => cx.toasts.error(format!("Nothing was removed: {e}")),
            }
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Empty folders",
                    "Finds folders with nothing in them, however deeply they are nested. Scanning only reads.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(!self.selected.is_empty() && !self.is_busy(), |ui| {
                    if widgets::primary_button(ui, &pal, "Remove selected").clicked() {
                        self.confirm = true;
                    }
                });
                ui.add_enabled_ui(!self.is_busy() && !self.roots.is_empty(), |ui| {
                    let clicked = if self.selected.is_empty() {
                        widgets::primary_button(ui, &pal, "Scan").clicked()
                    } else {
                        widgets::button(ui, &pal, "Scan").clicked()
                    };
                    if clicked {
                        self.start_scan(&ctx, cx.settings);
                    }
                });
            });
        });

        // Where to look.
        widgets::panel(&pal)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut remove: Option<usize> = None;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    ui.label(text::strong("Look in"));
                    for (index, root) in self.roots.iter().enumerate() {
                        egui::Frame::new()
                            .fill(pal.well)
                            .corner_radius(egui::CornerRadius::same(6))
                            .inner_margin(egui::Margin::symmetric(10, 3))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label(text::body(root.display().to_string()));
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
                            .set_title("Choose a folder to search")
                            .pick_folder()
                        && !self.roots.contains(&folder)
                    {
                        self.roots.push(folder);
                    }
                });
                if let Some(index) = remove {
                    self.roots.remove(index);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    widgets::checkbox(
                        ui,
                        &pal,
                        &mut self.include_hidden,
                        "Look inside hidden folders too",
                    );
                    ui.label(text::small(
                        "Hidden folders usually belong to programs that expect them to exist. Windows and program folders are always left out.",
                        &pal,
                    ));
                });
            });
        ui.add_space(6.0);

        let mut toggle: Option<PathBuf> = None;
        let mut select_all: Option<bool> = None;
        let mut reveal: Option<PathBuf> = None;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - vec2(2.0, 2.0));
            if self.is_busy() {
                let (verb, progress) = if self.deleting.is_running() {
                    ("Removing", self.deleting.progress())
                } else {
                    ("Looking for empty folders", self.scanning.progress())
                };
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.25).clamp(30.0, 160.0));
                    ui.label(text::figure(format!("{verb}…"), 28.0));
                    ui.scope(|ui| {
                        ui.set_max_width(420.0);
                        widgets::progress_bar(ui, &pal, progress.and_then(|p| p.fraction()), 8.0);
                    });
                    let examined = progress.map_or(0, |p| p.done());
                    let note = if self.scanning.is_running() && examined > 0 {
                        format!("{} looked into", plural(examined, "folder"))
                    } else {
                        " ".to_owned()
                    };
                    ui.label(text::muted(note, &pal));
                    ui.add_space(6.0);
                    if self.scanning.is_running() && widgets::button(ui, &pal, "Cancel").clicked() {
                        self.scanning.cancel();
                    }
                });
                return;
            }
            let Some(found) = &self.found else {
                if widgets::empty_state(
                    ui,
                    &pal,
                    icon::FOLDER_DASHED,
                    "Find folders that hold nothing",
                    "Uninstalled programs and tidied-up files leave empty folders behind. A folder counts as empty when there is no file anywhere inside it.",
                    Some("Scan"),
                ) {
                    self.start_scan(&ctx, cx.settings);
                }
                return;
            };
            if found.folders.is_empty() {
                widgets::empty_state(
                    ui,
                    &pal,
                    icon::CHECK_CIRCLE,
                    "No empty folders",
                    "Every folder in the places you chose has something in it.",
                    None,
                );
                return;
            }

            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(text::strong(plural(
                            found.folders.len() as u64,
                            "empty folder",
                        )));
                        ui.label(text::small(
                            format!(
                                "{} looked into in {}",
                                plural(found.examined, "folder"),
                                format_duration(found.elapsed)
                            ),
                            &pal,
                        ));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            if widgets::quiet_button(ui, &pal, "Select none").clicked() {
                                select_all = Some(false);
                            }
                            if widgets::quiet_button(ui, &pal, "Select all").clicked() {
                                select_all = Some(true);
                            }
                            if !self.selected.is_empty() {
                                ui.add_space(8.0);
                                ui.label(text::small(
                                    format!("{} selected", format_count(self.selected.len() as u64)),
                                    &pal,
                                ));
                            }
                        });
                    });
                });
            widgets::hairline(ui, &pal);

            let columns = Columns(vec![
                Col::Fixed(18.0),
                Col::Flex(1.0),
                Col::Flex(1.6),
                Col::Fixed(32.0),
            ]);
            let row_height = 36.0;
            ScrollArea::vertical()
                .id_salt("empty-folders")
                .auto_shrink([false, false])
                .show_rows(ui, row_height, found.folders.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for index in range {
                        let path = &found.folders[index];
                        let is_selected = self.selected.contains(path);
                        let (row, response) =
                            widgets::list_row(ui, &pal, row_height, is_selected, Sense::click());
                        let cells = columns.cells(row, 12.0);
                        let (name, parent) = split(path);
                        let check_rect = Rect::from_center_size(
                            pos2(cells[0].center().x, row.center().y),
                            vec2(18.0, 18.0),
                        );
                        let state = if is_selected { Check::On } else { Check::Off };
                        let clicked = widgets::in_cell(ui, check_rect, |ui| {
                            widgets::check(ui, &pal, state, &path.display().to_string()).clicked()
                        });
                        let color = if is_selected { pal.text } else { pal.text_muted };
                        widgets::paint_text(
                            ui,
                            cells[1],
                            &name,
                            text::body_font(14.0),
                            pal.text,
                            Align::Min,
                        );
                        widgets::paint_text(
                            ui,
                            cells[2],
                            &parent,
                            text::body_font(13.0),
                            color,
                            Align::Min,
                        );
                        let shown = widgets::in_cell_right(ui, cells[3], |ui| {
                            widgets::icon_button(
                                ui,
                                &pal,
                                icon::FOLDER_OPEN,
                                "Show in File Explorer",
                            )
                            .clicked()
                        });
                        if shown {
                            reveal = Some(path.clone());
                        } else if clicked || response.clicked() {
                            toggle = Some(path.clone());
                        }
                        response.on_hover_text(path.display().to_string());
                    }
                });
        });
        if let Some(path) = toggle
            && !self.selected.remove(&path)
        {
            self.selected.insert(path);
        }
        match (select_all, &self.found) {
            (Some(true), Some(found)) => self.selected = found.folders.iter().cloned().collect(),
            (Some(false), _) => self.selected.clear(),
            _ => {}
        }
        if let Some(path) = reveal {
            // Explorer opens the parent folder with this one highlighted.
            let mut select = std::ffi::OsString::from("/select,");
            select.push(path.as_os_str());
            if let Err(e) = hidden_command("explorer.exe").arg(select).spawn() {
                cx.toasts
                    .error(format!("File Explorer could not be opened: {e}"));
            }
        }

        if self.confirm {
            let recycle = cx.settings.general.tools_use_recycle_bin;
            let body = format!(
                "{} will be {}. Each one is checked again first; a folder that has gained a file since the scan is left alone.",
                plural(self.selected.len() as u64, "empty folder"),
                if recycle {
                    "moved to the Recycle Bin"
                } else {
                    "deleted permanently"
                }
            );
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "empty-delete",
                "Remove the selected folders?",
                &body,
                "Remove",
                !recycle,
            ) {
                Confirm::Yes => {
                    self.confirm = false;
                    // Scan order, so progress runs down the list as shown.
                    let folders: Vec<PathBuf> = self
                        .found
                        .iter()
                        .flat_map(|found| found.folders.iter())
                        .filter(|path| self.selected.contains(*path))
                        .cloned()
                        .collect();
                    self.deleting
                        .start(&ctx, "empty-delete", move |progress: Progress| {
                            let report = disk::delete_empty_folders(&folders, recycle, &progress);
                            (report, folders)
                        });
                }
                Confirm::No => self.confirm = false,
                Confirm::Pending => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_split_into_name_and_location() {
        let (name, parent) = split(Path::new(r"C:\Users\Alex\Documents\Scans\2019"));
        assert_eq!(name, "2019");
        assert_eq!(parent, r"C:\Users\Alex\Documents\Scans");
        let (name, parent) = split(Path::new(r"C:\"));
        assert_eq!(name, r"C:\");
        assert_eq!(parent, "");
    }

    #[test]
    fn only_folders_that_were_really_removed_leave_the_list() {
        let all: Vec<PathBuf> = ["a", "b", "c", "d"].iter().map(PathBuf::from).collect();
        let requested = vec![all[0].clone(), all[1].clone(), all[2].clone()];

        let mut report = DeleteReport {
            dirs: 2,
            ..DeleteReport::default()
        };
        report.fail(&all[1], "the folder is no longer empty");
        let mut found = all.clone();
        after_delete(&mut found, &requested, &report);
        assert_eq!(found, vec![all[1].clone(), all[3].clone()]);

        // More failures than the report names: nothing is hidden.
        let vague = DeleteReport {
            failed: 2,
            failures: vec![(all[0].clone(), "in use".to_owned())],
            ..DeleteReport::default()
        };
        let mut found = all.clone();
        after_delete(&mut found, &requested, &vague);
        assert_eq!(found, all);
    }

    #[test]
    fn scans_and_removes_on_a_temporary_tree_only() {
        // The page's two jobs, run directly against a throw-away folder.
        let tmp = std::env::temp_dir().join(format!("zest-empty-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("empty/nested")).unwrap();
        std::fs::create_dir_all(tmp.join("full")).unwrap();
        std::fs::write(tmp.join("full/keep.txt"), b"keep").unwrap();

        let options = EmptyFolderOptions::for_roots(vec![tmp.clone()]);
        let folders = disk::find_empty_folders(&options, &Progress::new()).unwrap();
        assert_eq!(folders, vec![tmp.join("empty")]);

        let report = disk::delete_empty_folders(&folders, false, &Progress::new());
        assert_eq!((report.dirs, report.failed), (2, 0));
        let mut found = folders.clone();
        after_delete(&mut found, &folders, &report);
        assert!(found.is_empty());
        assert!(!tmp.join("empty").exists());
        assert!(tmp.join("full/keep.txt").exists());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
