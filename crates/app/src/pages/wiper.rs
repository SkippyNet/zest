//! Drive wiper: overwrite free space, erase a whole drive, shred chosen files.

use super::Cx;
use crate::job::Job;
use crate::theme::text;
use crate::widgets::{self, Confirm, Tone};
use eframe::egui::{self, ScrollArea, Ui};
use egui_phosphor::regular as icon;
use std::path::PathBuf;
use std::time::Duration;
use zest_engine::core::Progress;
use zest_engine::core::delete::WipeMethod;
use zest_engine::core::fmt::{format_bytes, format_duration, plural};
use zest_engine::disk::{self, DriveInfo};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    FreeSpace,
    EraseDrive,
    Shred,
}

/// State of the Drive wiper page.
pub struct WiperPage {
    drives: Option<Vec<DriveInfo>>,
    free_drive: usize,
    erase_drive: usize,
    method: WipeMethod,
    typed: String,
    shred_paths: Vec<PathBuf>,
    job: Job<Result<String, String>>,
    job_title: String,
    confirm: Option<Pending>,
}

impl Default for WiperPage {
    fn default() -> Self {
        WiperPage {
            drives: None,
            free_drive: 0,
            erase_drive: 0,
            method: WipeMethod::Simple,
            typed: String::new(),
            shred_paths: Vec::new(),
            job: Job::default(),
            job_title: String::new(),
            confirm: None,
        }
    }
}

fn method_picker(ui: &mut Ui, id: &str, method: &mut WipeMethod) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(method.label())
        .width(270.0)
        .show_ui(ui, |ui| {
            for option in WipeMethod::ALL
                .into_iter()
                .filter(|m| *m != WipeMethod::None)
            {
                ui.selectable_value(method, option, option.label());
            }
        });
}

fn drive_picker(ui: &mut Ui, id: &str, drives: &[&DriveInfo], index: &mut usize) {
    *index = (*index).min(drives.len().saturating_sub(1));
    let current = drives
        .get(*index)
        .map(|d| d.display_name())
        .unwrap_or_default();
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(230.0)
        .show_ui(ui, |ui| {
            for (i, drive) in drives.iter().enumerate() {
                ui.selectable_value(
                    index,
                    i,
                    format!(
                        "{}  ({} free)",
                        drive.display_name(),
                        format_bytes(drive.free_bytes)
                    ),
                );
            }
        });
}

impl WiperPage {
    /// True while a wipe is running.
    pub fn is_busy(&self) -> bool {
        self.job.is_running()
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.job.poll(&ctx) {
            match result.and_then(|r| r) {
                Ok(summary) => {
                    cx.toasts.good(summary);
                    self.drives = None;
                }
                Err(e) => cx.toasts.error(format!("{} stopped: {e}", self.job_title)),
            }
        }
        let drives = self
            .drives
            .get_or_insert_with(disk::list_local_drives)
            .clone();
        let all: Vec<&DriveInfo> = drives.iter().filter(|d| d.is_ready).collect();
        let erasable: Vec<&DriveInfo> = all
            .iter()
            .copied()
            .filter(|d| disk::check_drive_wipe_allowed(&d.root).is_ok())
            .collect();

        widgets::page_header(
            ui,
            &pal,
            "Drive wiper",
            "Deleted files stay recoverable until something overwrites them. These tools do the overwriting.",
        );

        if self.job.is_running() {
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(text::figure(format!("{}…", self.job_title), 28.0));
                    let progress = self.job.progress();
                    ui.scope(|ui| {
                        ui.set_max_width(460.0);
                        widgets::progress_bar(ui, &pal, progress.and_then(|p| p.fraction()), 8.0);
                    });
                    if let Some(p) = progress {
                        ui.label(text::muted(
                            format!(
                                "{} written, {} elapsed",
                                format_bytes(p.bytes()),
                                format_duration(Duration::from_secs(self.job.elapsed().as_secs()))
                            ),
                            &pal,
                        ));
                    }
                    ui.add_space(8.0);
                    if widgets::button(ui, &pal, "Stop").clicked() {
                        self.job.cancel();
                    }
                });
                ui.add_space(20.0);
            });
            return;
        }

        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let ssd = all.get(self.free_drive).is_some_and(|d| d.is_ssd == Some(true));
            if ssd {
                widgets::banner(
                    ui,
                    &pal,
                    Tone::Info,
                    icon::INFO,
                    "This drive is an SSD. SSDs move data around internally, so overwriting cannot guarantee that old data is gone. For that guarantee, use device encryption.",
                    None,
                );
                ui.add_space(8.0);
            }

            // Free space.
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(text::section("Wipe free space"));
                ui.label(text::muted(
                    "Overwrites the unused part of a drive so files you already deleted cannot be recovered. Your existing files are not touched.",
                    &pal,
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    drive_picker(ui, "wipe-free-drive", &all, &mut self.free_drive);
                    method_picker(ui, "wipe-free-method", &mut self.method);
                    ui.add_enabled_ui(!all.is_empty(), |ui| {
                        if widgets::primary_button(ui, &pal, "Wipe free space").clicked() {
                            self.confirm = Some(Pending::FreeSpace);
                        }
                    });
                });
            });
            ui.add_space(8.0);

            // Shredder.
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(text::section("Shred files and folders"));
                ui.label(text::muted(
                    "Overwrites the contents of the files you choose, then deletes them. They do not go to the Recycle Bin.",
                    &pal,
                ));
                ui.add_space(6.0);
                let mut remove: Option<usize> = None;
                for (index, path) in self.shred_paths.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if widgets::icon_button(ui, &pal, icon::X, "Remove from the list").clicked() {
                            remove = Some(index);
                        }
                        ui.add(egui::Label::new(text::body(path.display().to_string())).truncate());
                    });
                }
                if let Some(index) = remove {
                    self.shred_paths.remove(index);
                }
                ui.horizontal(|ui| {
                    if widgets::icon_text_button(ui, &pal, icon::FILE_PLUS, "Add files").clicked()
                        && let Some(files) = rfd::FileDialog::new().set_title("Choose files to shred").pick_files()
                    {
                        for file in files {
                            if !self.shred_paths.contains(&file) {
                                self.shred_paths.push(file);
                            }
                        }
                    }
                    if widgets::icon_text_button(ui, &pal, icon::FOLDER_PLUS, "Add folder").clicked()
                        && let Some(folder) = rfd::FileDialog::new().set_title("Choose a folder to shred").pick_folder()
                        && !self.shred_paths.contains(&folder)
                    {
                        self.shred_paths.push(folder);
                    }
                    method_picker(ui, "shred-method", &mut self.method);
                    ui.add_enabled_ui(!self.shred_paths.is_empty(), |ui| {
                        if widgets::danger_button(ui, &pal, "Shred").clicked() {
                            self.confirm = Some(Pending::Shred);
                        }
                    });
                });
            });
            ui.add_space(8.0);

            // Entire drive.
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(text::section("Erase an entire drive"));
                ui.label(text::muted(
                    "Securely deletes everything on a drive and then wipes its free space. For drives you are selling, returning or throwing away. The Windows drive cannot be chosen.",
                    &pal,
                ));
                ui.add_space(6.0);
                if erasable.is_empty() {
                    ui.label(text::body("No other drive is connected."));
                    return;
                }
                ui.horizontal(|ui| {
                    drive_picker(ui, "erase-drive", &erasable, &mut self.erase_drive);
                    method_picker(ui, "erase-method", &mut self.method);
                });
                let Some(drive) = erasable.get(self.erase_drive) else {
                    return;
                };
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(text::body(format!("Type {} to confirm:", drive.letter)));
                    ui.add(egui::TextEdit::singleline(&mut self.typed).desired_width(48.0).char_limit(2));
                    let matches = self
                        .typed
                        .trim()
                        .trim_end_matches(':')
                        .eq_ignore_ascii_case(&drive.letter.to_string());
                    ui.add_enabled_ui(matches, |ui| {
                        if widgets::danger_button(ui, &pal, "Erase everything").clicked() {
                            self.confirm = Some(Pending::EraseDrive);
                        }
                    });
                });
            });
        });

        let Some(pending) = self.confirm else {
            return;
        };
        let method = self.method;
        let (title, body, label) = match pending {
            Pending::FreeSpace => {
                let drive = all
                    .get(self.free_drive)
                    .map(|d| d.display_name())
                    .unwrap_or_default();
                (
                    "Wipe free space?".to_owned(),
                    format!(
                        "The free space of {drive} is overwritten ({}). This can take a long time and the drive will be nearly full while it runs. You can stop at any point.",
                        method.label().to_lowercase()
                    ),
                    "Wipe",
                )
            }
            Pending::Shred => (
                "Shred these items?".to_owned(),
                format!(
                    "{} will be overwritten and deleted. This cannot be undone.",
                    plural(self.shred_paths.len() as u64, "item")
                ),
                "Shred",
            ),
            Pending::EraseDrive => {
                let drive = erasable
                    .get(self.erase_drive)
                    .map(|d| d.display_name())
                    .unwrap_or_default();
                (
                    format!("Erase everything on {drive}?"),
                    "Every file and folder on this drive is destroyed. This cannot be undone and cannot be stopped halfway without losing data.".to_owned(),
                    "Erase drive",
                )
            }
        };
        match widgets::confirm_dialog(&ctx, &pal, "wiper", &title, &body, label, true) {
            Confirm::Yes => {
                self.confirm = None;
                match pending {
                    Pending::FreeSpace => {
                        if let Some(root) = all.get(self.free_drive).map(|d| d.root.clone()) {
                            self.job_title = "Wiping free space".to_owned();
                            self.job
                                .start(&ctx, "wipe-free", move |progress: Progress| {
                                    disk::wipe_free_space(&root, method, None, &progress)
                                        .map(|r| {
                                            format!(
                                                "{} of free space overwritten.",
                                                format_bytes(r.bytes_wiped)
                                            )
                                        })
                                        .map_err(|e| e.to_string())
                                });
                        }
                    }
                    Pending::Shred => {
                        let paths = std::mem::take(&mut self.shred_paths);
                        self.job_title = "Shredding".to_owned();
                        self.job.start(&ctx, "shred", move |progress: Progress| {
                            let report = disk::shred(&paths, method, &progress);
                            if report.failed > 0 {
                                Err(format!(
                                    "{} shredded, {} could not be removed",
                                    plural(report.files, "file"),
                                    report.failed
                                ))
                            } else {
                                Ok(format!("{} shredded.", plural(report.files, "file")))
                            }
                        });
                    }
                    Pending::EraseDrive => {
                        if let Some(root) = erasable.get(self.erase_drive).map(|d| d.root.clone()) {
                            self.typed.clear();
                            self.job_title = "Erasing drive".to_owned();
                            self.job.start(&ctx, "wipe-drive", move |progress: Progress| {
                                disk::wipe_entire_drive(&root, method, &progress)
                                    .map(|r| {
                                        format!(
                                            "Drive erased: {} destroyed, {} of free space overwritten.",
                                            plural(r.contents.deleted.files, "file"),
                                            format_bytes(r.free_space.bytes_wiped)
                                        )
                                    })
                                    .map_err(|e| e.to_string())
                            });
                        }
                    }
                }
            }
            Confirm::No => self.confirm = None,
            Confirm::Pending => {}
        }
    }
}
