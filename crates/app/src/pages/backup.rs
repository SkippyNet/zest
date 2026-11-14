//! Backup: versioned file backups and full system images.

use super::Cx;
use crate::job::Job;
use crate::theme::{Palette, text};
use crate::widgets::{self, Check, Col, Columns, Confirm, Tone};
use eframe::egui::{self, Align, Layout, ScrollArea, Sense, Ui, vec2};
use egui_phosphor::regular as icon;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zest_engine::Settings;
use zest_engine::backup::image::{self, ImageOutcome, ImageRequest, ImageVersion, TargetFacts};
use zest_engine::backup::{
    self, BackupOptions, BackupReport, BrowseEntry, EntryKind, GcReport, Overwrite, RepoStats,
    Repository, RestoreOptions, RestoreReport, RestoreTarget, Selection, Skipped, Snapshot,
    SnapshotInfo, VerifyReport, VerifyScope,
};
use zest_engine::core::fmt::{format_age, format_bytes, format_count, format_timestamp, plural};
use zest_engine::core::paths::UserDirs;
use zest_engine::core::{Error, Progress};
use zest_engine::disk::{self, DriveInfo, DriveKind};
use zest_engine::settings::BackupSettings;

const TAB_FILES: usize = 0;

/// What the page is busy with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Backup,
    Restore,
    Verify,
    Delete,
}

impl Work {
    fn label(self) -> &'static str {
        match self {
            Work::Backup => "Backing up",
            Work::Restore => "Restoring",
            Work::Verify => "Checking the backup",
            Work::Delete => "Removing",
        }
    }
}

/// Result of a finished job.
enum Done {
    Backup { report: BackupReport, pruned: usize },
    Restore(RestoreReport),
    Verify(VerifyReport),
    Delete(GcReport),
}

/// What the repository at the chosen destination holds.
struct Listing {
    snapshots: Vec<SnapshotInfo>,
    stats: RepoStats,
}

/// Something from the last run the user should look at.
enum Notice {
    /// Items the last backup left out.
    Skipped(Vec<Skipped>),
    /// Damage found by a check.
    Damaged(VerifyReport),
    /// Files a restore did not write.
    NotRestored(RestoreReport),
}

/// An open snapshot being browsed.
struct Browse {
    snapshot: Arc<Snapshot>,
    /// Source being looked at; `None` shows the list of sources.
    root: Option<u32>,
    dir: String,
    rows: Vec<BrowseEntry>,
    picked: BTreeSet<(u32, String)>,
}

impl Browse {
    fn new(snapshot: Arc<Snapshot>) -> Browse {
        let mut browse = Browse {
            snapshot,
            root: None,
            dir: String::new(),
            rows: Vec::new(),
            picked: BTreeSet::new(),
        };
        // A snapshot of a single folder opens straight into it.
        let roots = browse.snapshot.roots();
        if roots.len() == 1 && roots[0].kind == EntryKind::Dir {
            browse.root = Some(0);
        }
        browse.refresh();
        browse
    }

    fn refresh(&mut self) {
        self.rows = match self.root {
            None => self.snapshot.roots(),
            Some(root) => self.snapshot.list(root, &self.dir),
        };
    }

    fn open(&mut self, root: Option<u32>, dir: &str) {
        self.root = root;
        self.dir = dir.to_owned();
        self.refresh();
    }

    /// Whether the folder being shown, or one above it, is ticked.
    fn inherited(&self, root: u32, path: &str) -> bool {
        self.picked.iter().any(|(r, p)| {
            *r == root
                && p.len() < path.len()
                && (p.is_empty()
                    || (path.starts_with(p.as_str()) && path.as_bytes()[p.len()] == b'\\'))
        })
    }

    fn contains_picked(&self, root: u32, path: &str) -> bool {
        self.picked.iter().any(|(r, p)| {
            *r == root
                && p.len() > path.len()
                && (path.is_empty() || (p.starts_with(path) && p.as_bytes()[path.len()] == b'\\'))
        })
    }
}

/// The restore dialog.
struct RestoreDialog {
    snapshot: Arc<Snapshot>,
    selection: Vec<Selection>,
    /// What is being restored, in words.
    what: String,
    to_original: bool,
    folder: Option<PathBuf>,
    overwrite: Overwrite,
}

/// State of the System image tab.
#[derive(Default)]
struct ImagePane {
    drives: Option<Vec<DriveInfo>>,
    target: Option<char>,
    extra: BTreeSet<char>,
    listing: Job<Result<Vec<ImageVersion>, Error>>,
    versions: Option<Result<Vec<ImageVersion>, String>>,
    creating: Job<Result<ImageOutcome, Error>>,
    confirm: bool,
}

/// State of the Backup page.
#[derive(Default)]
pub struct BackupPage {
    tab: usize,
    /// Made-up choices shown instead of the real settings (documentation).
    demo: Option<BackupSettings>,
    exclude_text: Option<String>,
    /// Destination the listing was last requested for; cleared to ask again.
    loaded_for: Option<PathBuf>,
    /// Destination whose backups are on screen.
    shown_for: Option<PathBuf>,
    loading: Job<Result<Listing, Error>>,
    listing: Option<Result<Listing, String>>,
    work: Job<Result<Done, Error>>,
    working_on: Option<Work>,
    notice: Option<Notice>,
    notice_open: bool,
    opening: Job<Result<Snapshot, Error>>,
    browse: Option<Browse>,
    restore: Option<RestoreDialog>,
    confirm_replace: Option<RestoreDialog>,
    confirm_delete: Option<SnapshotInfo>,
    image: ImagePane,
}

fn taken_at(info: &SnapshotInfo) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(info.created.max(0) as u64)
}

fn last_part(path: &str) -> &str {
    let trimmed = path.trim_end_matches('\\');
    trimmed
        .rsplit('\\')
        .next()
        .filter(|p| !p.is_empty())
        .unwrap_or(path)
}

/// Upper-case drive letter of a path such as `C:\Users`.
fn drive_of(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let mut chars = text.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':')).then(|| letter.to_ascii_uppercase())
}

/// What a snapshot holds, in a few words.
fn contents_of(info: &SnapshotInfo) -> String {
    if !info.label.is_empty() {
        return info.label.clone();
    }
    info.sources
        .iter()
        .map(|s| last_part(s))
        .collect::<Vec<_>>()
        .join(", ")
}

fn chip(ui: &mut Ui, pal: &Palette, label: &str, remove_hint: &str) -> bool {
    let mut removed = false;
    egui::Frame::new()
        .fill(pal.well)
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(text::body(label));
            let close = ui.add(
                egui::Label::new(text::icon(icon::X, 13.0).color(pal.text_muted))
                    .sense(Sense::click()),
            );
            if close
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(remove_hint)
                .clicked()
            {
                removed = true;
            }
        });
    removed
}

impl BackupPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.loading.is_running()
            || self.work.is_running()
            || self.opening.is_running()
            || self.image.listing.is_running()
            || self.image.creating.is_running()
    }

    /// Starts the page's read-only analysis (used by screenshot mode). The
    /// list of backups loads by itself once a destination is chosen.
    pub fn start_scan(&mut self, _ctx: &egui::Context, _settings: &Settings) {}

    /// Fills the page with made-up data for documentation screenshots.
    pub fn load_demo(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let sources = vec![
            r"C:\Users\Alex\Documents".to_owned(),
            r"C:\Users\Alex\Pictures".to_owned(),
            r"C:\Users\Alex\Desktop".to_owned(),
        ];
        let destination = PathBuf::from(r"E:\Backups");
        self.demo = Some(BackupSettings {
            destination: Some(destination.clone()),
            sources: sources.iter().map(PathBuf::from).collect(),
            ..BackupSettings::default()
        });
        // (hours ago, files, size in MB, new data in MB, stored MB, note)
        let runs: [(i64, u64, u64, u64, u64, &str); 6] = [
            (2, 48_212, 61_840, 312, 201, ""),
            (26, 48_170, 61_530, 1_240, 902, ""),
            (50, 47_951, 60_310, 96, 58, ""),
            (
                170,
                47_902,
                60_220,
                2_870,
                2_105,
                "Before the Windows update",
            ),
            (338, 46_388, 57_400, 640, 433, ""),
            (720, 45_901, 56_790, 56_790, 39_412, "First backup"),
        ];
        let snapshots: Vec<SnapshotInfo> = runs
            .iter()
            .map(
                |&(hours, files, mb, new_mb, stored_mb, note)| SnapshotInfo {
                    id: format!("demo-{hours}"),
                    created: now - hours * 3600,
                    label: note.to_owned(),
                    sources: sources.clone(),
                    files,
                    dirs: files / 14,
                    bytes: mb << 20,
                    new_bytes: new_mb << 20,
                    stored_bytes: stored_mb << 20,
                    reused_files: files.saturating_sub(new_mb.min(files)),
                    skipped: 0,
                },
            )
            .collect();
        let stats = RepoStats {
            snapshots: snapshots.len() as u64,
            latest_bytes: snapshots[0].bytes,
            logical_bytes: snapshots.iter().map(|s| s.bytes).sum(),
            stored_bytes: snapshots.iter().map(|s| s.stored_bytes).sum(),
            objects: 61_204,
            oldest: snapshots.last().map(|s| s.created),
            newest: snapshots.first().map(|s| s.created),
        };
        self.loaded_for = Some(destination.clone());
        self.shown_for = Some(destination);
        self.listing = Some(Ok(Listing { snapshots, stats }));
    }

    fn reload(&mut self) {
        self.loaded_for = None;
    }

    fn start_backup(
        &mut self,
        ctx: &egui::Context,
        cfg: &BackupSettings,
        repair: Option<VerifyReport>,
    ) {
        let Some(destination) = cfg.destination.clone() else {
            return;
        };
        if self.demo.is_some() || self.work.is_running() || cfg.sources.is_empty() {
            return;
        }
        let options = BackupOptions {
            sources: cfg.sources.clone(),
            excludes: cfg.excludes.clone(),
            ..BackupOptions::default()
        };
        let keep = cfg.keep_last as usize;
        self.notice = None;
        self.browse = None;
        self.working_on = Some(Work::Backup);
        self.work.start(ctx, "backup", move |progress: Progress| {
            let repo = Repository::open_or_create(&destination)?;
            if let Some(report) = &repair {
                backup::discard_damaged(&repo, report)?;
            }
            let report = backup::create_snapshot(&repo, &options, &progress)?;
            let mut pruned = 0;
            if keep > 0 {
                match backup::prune(&repo, keep, &Progress::new()) {
                    Ok(outcome) => pruned = outcome.removed.len(),
                    Err(e) => log::warn!("old snapshots were not removed: {e}"),
                }
            }
            Ok(Done::Backup { report, pruned })
        });
    }

    fn start_verify(&mut self, ctx: &egui::Context, destination: &Path, scope: VerifyScope) {
        if self.demo.is_some() || self.work.is_running() {
            return;
        }
        let destination = destination.to_path_buf();
        self.working_on = Some(Work::Verify);
        self.work
            .start(ctx, "backup-verify", move |progress: Progress| {
                let repo = Repository::open(&destination)?;
                Ok(Done::Verify(backup::verify(&repo, &scope, &progress)?))
            });
    }

    fn start_delete(&mut self, ctx: &egui::Context, destination: &Path, id: String) {
        if self.demo.is_some() || self.work.is_running() {
            return;
        }
        let destination = destination.to_path_buf();
        self.working_on = Some(Work::Delete);
        self.work
            .start(ctx, "backup-delete", move |progress: Progress| {
                let repo = Repository::open(&destination)?;
                Ok(Done::Delete(backup::delete_snapshot(
                    &repo, &id, &progress,
                )?))
            });
    }

    fn start_restore(&mut self, ctx: &egui::Context, destination: &Path, dialog: RestoreDialog) {
        if self.demo.is_some() || self.work.is_running() {
            return;
        }
        let target = if dialog.to_original {
            RestoreTarget::Original
        } else if let Some(folder) = dialog.folder.clone() {
            RestoreTarget::Folder(folder)
        } else {
            return;
        };
        let options = RestoreOptions {
            target,
            overwrite: dialog.overwrite,
            selection: dialog.selection,
        };
        let snapshot = dialog.snapshot;
        let destination = destination.to_path_buf();
        self.working_on = Some(Work::Restore);
        self.work
            .start(ctx, "backup-restore", move |progress: Progress| {
                let repo = Repository::open(&destination)?;
                Ok(Done::Restore(backup::restore(
                    &repo, &snapshot, &options, &progress,
                )?))
            });
    }

    fn open_snapshot(&mut self, ctx: &egui::Context, destination: &Path, id: String) {
        if self.demo.is_some() || self.opening.is_running() {
            return;
        }
        let destination = destination.to_path_buf();
        self.opening.start(ctx, "backup-open", move |_| {
            let repo = Repository::open(&destination)?;
            Snapshot::load(&repo, &id)
        });
    }

    fn poll(&mut self, ctx: &egui::Context, cx: &mut Cx, cfg: &BackupSettings) {
        // Load the list of backups whenever the destination changes.
        if self.demo.is_none() && self.loaded_for != cfg.destination && !self.loading.is_running() {
            self.loaded_for = cfg.destination.clone();
            if self.shown_for != cfg.destination {
                // Another place: what is on screen belongs to the old one.
                // A refresh of the same place keeps the list up meanwhile.
                self.shown_for = cfg.destination.clone();
                self.listing = None;
                self.browse = None;
                self.notice = None;
            }
            if let Some(destination) = cfg.destination.clone() {
                self.loading.start(ctx, "backup-list", move |_| {
                    if !Repository::exists(&destination) {
                        return Ok(Listing {
                            snapshots: Vec::new(),
                            stats: RepoStats::default(),
                        });
                    }
                    let repo = Repository::open(&destination)?;
                    Ok(Listing {
                        snapshots: repo.list_snapshots()?,
                        stats: backup::stats(&repo)?,
                    })
                });
            }
        }
        if let Some(result) = self.loading.poll(ctx) {
            self.listing = Some(match result {
                Ok(Ok(listing)) => Ok(listing),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(e),
            });
        }
        if let Some(result) = self.opening.poll(ctx) {
            match result {
                Ok(Ok(snapshot)) => self.browse = Some(Browse::new(Arc::new(snapshot))),
                Ok(Err(e)) => cx
                    .toasts
                    .error(format!("This backup could not be opened: {e}")),
                Err(e) => cx.toasts.error(e),
            }
        }
        if let Some(result) = self.work.poll(ctx) {
            let kind = self.working_on.take();
            self.reload();
            match result {
                Ok(Ok(done)) => self.finished(cx, done),
                Ok(Err(Error::Cancelled)) => cx.toasts.info(match kind {
                    Some(Work::Backup) => "Backup cancelled. No snapshot was recorded.",
                    Some(Work::Restore) => "Restore cancelled. Files restored so far stay.",
                    _ => "Cancelled.",
                }),
                Ok(Err(e)) => cx.toasts.error(match kind {
                    Some(Work::Backup) => format!("The backup did not finish: {e}"),
                    Some(Work::Restore) => format!("The restore did not finish: {e}"),
                    Some(Work::Verify) => format!("The check did not finish: {e}"),
                    _ => e.to_string(),
                }),
                Err(e) => cx.toasts.error(e),
            }
        }
    }

    fn finished(&mut self, cx: &mut Cx, done: Done) {
        match done {
            Done::Backup { report, pruned } => {
                let s = &report.snapshot;
                let mut message = if s.new_bytes == 0 && report.read_files == 0 {
                    format!(
                        "Backup done: nothing changed in {}.",
                        plural(s.files, "file")
                    )
                } else {
                    format!(
                        "Backup done: {}, {} of new data.",
                        plural(s.files, "file"),
                        format_bytes(s.new_bytes)
                    )
                };
                if pruned > 0 {
                    message.push_str(&format!(
                        " {} removed.",
                        plural(pruned as u64, "old snapshot")
                    ));
                }
                cx.toasts.good(message);
                if !report.skipped.is_empty() {
                    self.notice = Some(Notice::Skipped(report.skipped));
                    self.notice_open = false;
                }
            }
            Done::Restore(report) => {
                let mut message = format!(
                    "{} restored, {}.",
                    plural(report.files, "file"),
                    format_bytes(report.bytes)
                );
                if report.unchanged > 0 {
                    message.push_str(&format!(
                        " {} already there.",
                        plural(report.unchanged, "file")
                    ));
                }
                cx.toasts.good(message);
                if !report.kept_existing.is_empty() || !report.failed.is_empty() {
                    self.notice = Some(Notice::NotRestored(report));
                    self.notice_open = false;
                }
            }
            Done::Verify(report) => {
                if report.is_ok() {
                    cx.toasts.good(format!(
                        "Everything is intact: {} of stored data checked.",
                        format_bytes(report.bytes)
                    ));
                    if matches!(self.notice, Some(Notice::Damaged(_))) {
                        self.notice = None;
                    }
                } else {
                    self.notice = Some(Notice::Damaged(report));
                    self.notice_open = false;
                }
            }
            Done::Delete(gc) => {
                cx.toasts.good(format!(
                    "Backup removed, {} freed.",
                    format_bytes(gc.bytes_freed)
                ));
            }
        }
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        let saved = cx.settings.backup.clone();
        let mut cfg = self.demo.clone().unwrap_or_else(|| saved.clone());
        self.poll(&ctx, cx, &cfg);

        let ready = cfg.destination.is_some() && !cfg.sources.is_empty();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Backup",
                    "Keep earlier versions of your files, and a full image of this PC.",
                );
            });
            if self.tab == TAB_FILES {
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    ui.add_space(2.0);
                    ui.add_enabled_ui(ready && !self.work.is_running(), |ui| {
                        if widgets::primary_button(ui, &pal, "Back up now").clicked() {
                            self.start_backup(&ctx, &cfg, None);
                        }
                    });
                });
            }
        });
        widgets::tabs(ui, &pal, &mut self.tab, &["File backup", "System image"]);
        ui.add_space(8.0);

        if self.tab == TAB_FILES {
            self.files_tab(ui, cx, &mut cfg);
        } else {
            self.image_tab(ui, cx);
        }
        self.dialogs(&ctx, cx, &cfg);

        if self.demo.is_none() && cfg != saved {
            cx.settings.backup = cfg;
            cx.settings_changed = true;
        }
    }

    // ── File backup ──────────────────────────────────────────────────

    fn files_tab(&mut self, ui: &mut Ui, cx: &mut Cx, cfg: &mut BackupSettings) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        self.setup_panel(ui, &pal, cfg);
        ui.add_space(6.0);

        if self.notice.is_some() && !self.work.is_running() {
            self.notice_banner(ui, &pal, cfg, &ctx);
            ui.add_space(6.0);
        }

        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - vec2(2.0, 2.0));
            if self.work.is_running() {
                self.progress_view(ui, &pal);
            } else if self.notice_open && self.notice.is_some() {
                self.notice_details(ui, &pal);
            } else if self.browse.is_some() {
                self.browse_view(ui, &pal);
            } else {
                self.snapshot_list(ui, &pal, cfg, &ctx);
            }
        });
    }

    fn setup_panel(&mut self, ui: &mut Ui, pal: &Palette, cfg: &mut BackupSettings) {
        let label_width = 92.0;
        widgets::panel(pal)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                // What to back up.
                let mut remove: Option<usize> = None;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    ui.allocate_ui_with_layout(
                        vec2(label_width, 24.0),
                        Layout::left_to_right(Align::Center),
                        |ui| ui.label(text::strong("Back up")),
                    );
                    for (index, source) in cfg.sources.iter().enumerate() {
                        if chip(ui, pal, &source.display().to_string(), "Stop backing this up") {
                            remove = Some(index);
                        }
                    }
                    if widgets::quiet_button(ui, pal, "Add folder").clicked()
                        && let Some(folder) = rfd::FileDialog::new()
                            .set_title("Choose a folder to back up")
                            .pick_folder()
                        && !cfg.sources.contains(&folder)
                    {
                        cfg.sources.push(folder);
                    }
                    if cfg.sources.is_empty() {
                        let profile = UserDirs::current().profile;
                        for name in ["Documents", "Pictures", "Desktop"] {
                            let folder = profile.join(name);
                            if folder.is_dir() && widgets::quiet_button(ui, pal, name).clicked() {
                                cfg.sources.push(folder);
                            }
                        }
                    }
                });
                if let Some(index) = remove {
                    cfg.sources.remove(index);
                }

                // Where to keep it.
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        vec2(label_width, 24.0),
                        Layout::left_to_right(Align::Center),
                        |ui| ui.label(text::strong("Keep it in")),
                    );
                    match &cfg.destination {
                        Some(destination) => {
                            ui.label(text::body(destination.display().to_string()));
                        }
                        None => {
                            ui.label(text::muted("Not chosen yet", pal));
                        }
                    }
                    let choose = if cfg.destination.is_some() {
                        "Change"
                    } else {
                        "Choose a folder"
                    };
                    if widgets::quiet_button(ui, pal, choose).clicked()
                        && let Some(folder) = rfd::FileDialog::new()
                            .set_title("Choose where to keep the backups")
                            .pick_folder()
                    {
                        cfg.destination = Some(folder);
                    }
                    let same_drive = cfg
                        .destination
                        .as_deref()
                        .and_then(drive_of)
                        .is_some_and(|drive| {
                            cfg.sources.iter().any(|s| drive_of(s) == Some(drive))
                        });
                    if same_drive {
                        ui.label(text::icon(icon::WARNING, 15.0).color(pal.warn));
                        ui.label(egui::RichText::new("Same drive as the files").size(12.0).color(pal.warn))
                            .on_hover_text(
                                "If this drive fails, the files and their backup are lost together. An external drive is safer.",
                            );
                    }
                    if let Some(Ok(listing)) = &self.listing
                        && listing.stats.snapshots > 0
                    {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let stats = &listing.stats;
                            let mut summary = format!(
                                "{} of versions kept in {}",
                                format_bytes(stats.logical_bytes),
                                format_bytes(stats.stored_bytes)
                            );
                            if let Some(ratio) = stats.savings_ratio().filter(|r| *r >= 1.1) {
                                summary.push_str(&format!(", {ratio:.1} times smaller"));
                            }
                            ui.label(text::small(summary, pal)).on_hover_text(
                                "Files that did not change between backups are stored once, and everything is compressed.",
                            );
                        });
                    }
                });

                // What to leave out and how much to keep.
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        vec2(label_width, 24.0),
                        Layout::left_to_right(Align::Center),
                        |ui| ui.label(text::strong("Leave out")),
                    );
                    let buffer = self
                        .exclude_text
                        .get_or_insert_with(|| cfg.excludes.join("; "));
                    let field = ui.add(
                        egui::TextEdit::singleline(buffer)
                            .hint_text("Names or paths, separated by semicolons")
                            .desired_width((ui.available_width() - 250.0).max(160.0)),
                    );
                    if field.changed() {
                        cfg.excludes = backup::split_patterns(buffer);
                    }
                    field.on_hover_text(
                        "Use * for any characters, for example *.tmp or node_modules. A pattern with a backslash is matched against the whole path.",
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(text::muted("backups", pal));
                        ui.add(
                            egui::DragValue::new(&mut cfg.keep_last)
                                .range(0..=1000)
                                .speed(0.1)
                                .custom_formatter(|n, _| {
                                    if n < 1.0 {
                                        "all".to_owned()
                                    } else {
                                        format!("{n:.0}")
                                    }
                                }),
                        )
                        .on_hover_text(
                            "Older backups are removed after each run. Set to 0 to keep them all.",
                        );
                        ui.label(text::muted("Keep the last", pal));
                    });
                });
            });
    }

    fn notice_banner(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        cfg: &BackupSettings,
        ctx: &egui::Context,
    ) {
        let Some(notice) = &self.notice else {
            return;
        };
        let details = if self.notice_open {
            "Hide"
        } else {
            "Show them"
        };
        match notice {
            Notice::Skipped(items) => {
                let message = format!(
                    "{} left out of the last backup because {} could not be read or {} not on this PC.",
                    plural(items.len() as u64, "item"),
                    if items.len() == 1 { "it" } else { "they" },
                    if items.len() == 1 { "is" } else { "are" },
                );
                if widgets::banner(ui, pal, Tone::Warn, icon::WARNING, &message, Some(details)) {
                    self.notice_open = !self.notice_open;
                }
            }
            Notice::NotRestored(report) => {
                let message = match (report.failed.len(), report.kept_existing.len()) {
                    (0, kept) => format!(
                        "{} not restored because a different file is already there.",
                        plural(kept as u64, "file")
                    ),
                    (failed, 0) => {
                        format!("{} could not be restored.", plural(failed as u64, "file"))
                    }
                    (failed, kept) => format!(
                        "{} could not be restored, and {} left alone because a different file is already there.",
                        plural(failed as u64, "file"),
                        kept
                    ),
                };
                if widgets::banner(ui, pal, Tone::Warn, icon::WARNING, &message, Some(details)) {
                    self.notice_open = !self.notice_open;
                }
            }
            Notice::Damaged(report) => {
                let message = if report.damaged_file_count > 0 {
                    format!(
                        "The check found damage: {} in the backup cannot be restored. Repairing stores them again from this PC where they still exist.",
                        plural(report.damaged_file_count, "file")
                    )
                } else {
                    format!(
                        "The check could not read {}.",
                        plural(report.unreadable_snapshots.len() as u64, "backup")
                    )
                };
                let can_repair = report.damaged_file_count > 0
                    && cfg.destination.is_some()
                    && !cfg.sources.is_empty();
                let action = can_repair.then_some("Repair");
                if widgets::banner(
                    ui,
                    pal,
                    Tone::Danger,
                    icon::WARNING_CIRCLE,
                    &message,
                    action,
                ) && let Some(Notice::Damaged(report)) = self.notice.take()
                {
                    self.start_backup(ctx, cfg, Some(report));
                }
            }
        }
    }

    fn notice_details(&mut self, ui: &mut Ui, pal: &Palette) {
        let rows: Vec<(String, String)> = match &self.notice {
            Some(Notice::Skipped(items)) => items
                .iter()
                .map(|item| {
                    let why = if item.detail.is_empty() {
                        item.reason.label().to_owned()
                    } else {
                        format!("{}: {}", item.reason.label(), item.detail)
                    };
                    (item.path.clone(), why)
                })
                .collect(),
            Some(Notice::NotRestored(report)) => report
                .failed
                .iter()
                .map(|f| (f.path.clone(), f.reason.clone()))
                .chain(
                    report
                        .kept_existing
                        .iter()
                        .map(|p| (p.clone(), "A different file is already there".to_owned())),
                )
                .collect(),
            _ => Vec::new(),
        };
        let columns = Columns(vec![Col::Flex(1.6), Col::Flex(1.0)]);
        let mut sort = (usize::MAX, true);
        widgets::list_header(
            ui,
            pal,
            &columns,
            &[("Item", Align::Min, false), ("Why", Align::Min, false)],
            &mut sort,
        );
        let row_height = 36.0;
        ScrollArea::vertical()
            .id_salt("backup-notice")
            .auto_shrink([false, false])
            .show_rows(ui, row_height, rows.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (path, why) in &rows[range] {
                    let (row, response) =
                        widgets::list_row(ui, pal, row_height, false, Sense::hover());
                    let cells = columns.cells(row, 12.0);
                    widgets::paint_text(
                        ui,
                        cells[0],
                        path,
                        text::body_font(13.0),
                        pal.text,
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        cells[1],
                        why,
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Min,
                    );
                    response.on_hover_text(format!("{path}\n{why}"));
                }
            });
    }

    fn progress_view(&mut self, ui: &mut Ui, pal: &Palette) {
        let kind = self.working_on.unwrap_or(Work::Backup);
        let progress = self.work.progress().cloned();
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.22).clamp(30.0, 150.0));
            ui.label(text::figure(format!("{}…", kind.label()), 28.0));
            ui.scope(|ui| {
                ui.set_max_width(440.0);
                widgets::progress_bar(ui, pal, progress.as_ref().and_then(Progress::fraction), 8.0);
            });
            let amount = progress
                .as_ref()
                .filter(|p| p.total() > 0)
                .map(|p| match kind {
                    Work::Backup | Work::Restore => {
                        format!("{} of {}", format_bytes(p.done()), format_bytes(p.total()))
                    }
                    Work::Verify => format!(
                        "{} of {} pieces, {} read",
                        format_count(p.done()),
                        format_count(p.total()),
                        format_bytes(p.bytes())
                    ),
                    Work::Delete => String::new(),
                });
            let message = progress.as_ref().map(Progress::message).unwrap_or_default();
            let line = match amount.filter(|a| !a.is_empty()) {
                Some(amount) => amount,
                None if message.is_empty() => " ".to_owned(),
                None => message,
            };
            ui.label(text::muted(line, pal));
            ui.add_space(6.0);
            if kind != Work::Delete && widgets::button(ui, pal, "Cancel").clicked() {
                self.work.cancel();
            }
        });
    }

    fn snapshot_list(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        cfg: &mut BackupSettings,
        ctx: &egui::Context,
    ) {
        let Some(destination) = cfg.destination.clone() else {
            if widgets::empty_state(
                ui,
                pal,
                icon::VAULT,
                "Choose where to keep your backups",
                "An external drive is the safest place: a backup on the same disk is lost together with it.",
                Some("Choose a folder"),
            ) && let Some(folder) = rfd::FileDialog::new()
                .set_title("Choose where to keep the backups")
                .pick_folder()
            {
                cfg.destination = Some(folder);
            }
            return;
        };
        let listing = match &self.listing {
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    widgets::spinner(ui, pal, 28.0);
                });
                return;
            }
            Some(Err(e)) => {
                let body = format!("{e} Check that the drive is connected.");
                if widgets::empty_state(
                    ui,
                    pal,
                    icon::PLUGS,
                    "The backups cannot be read",
                    &body,
                    Some("Try again"),
                ) {
                    self.reload();
                }
                return;
            }
            Some(Ok(listing)) => listing,
        };
        if listing.snapshots.is_empty() {
            if cfg.sources.is_empty() {
                widgets::empty_state(
                    ui,
                    pal,
                    icon::FOLDER_PLUS,
                    "Choose what to back up",
                    "Add the folders that matter to you above. Documents and Pictures are a good start.",
                    None,
                );
            } else if widgets::empty_state(
                ui,
                pal,
                icon::VAULT,
                "No backups yet",
                "The first backup copies everything. Later ones only add what changed, so they are quick and small.",
                Some("Back up now"),
            ) {
                self.start_backup(ctx, cfg, None);
            }
            return;
        }

        let mut open: Option<String> = None;
        let mut check: Option<VerifyScope> = None;
        let mut delete: Option<SnapshotInfo> = None;

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(text::strong(plural(listing.snapshots.len() as u64, "backup")));
                    ui.label(text::small(
                        format!(
                            "The newest holds {}.",
                            format_bytes(listing.stats.latest_bytes)
                        ),
                        pal,
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::quiet_button(ui, pal, "Check all").on_hover_text(
                            "Reads every stored piece back and compares it with its fingerprint. Changes nothing.",
                        ).clicked() {
                            check = Some(VerifyScope::All);
                        }
                        if self.opening.is_running() {
                            widgets::spinner(ui, pal, 18.0);
                        }
                    });
                });
            });
        widgets::hairline(ui, pal);

        let columns = Columns(vec![
            Col::Fixed(196.0),
            Col::Flex(1.0),
            Col::Fixed(84.0),
            Col::Fixed(88.0),
            Col::Fixed(88.0),
            Col::Fixed(108.0),
        ]);
        let mut sort = (usize::MAX, true);
        widgets::list_header(
            ui,
            pal,
            &columns,
            &[
                ("Taken", Align::Min, false),
                ("Contents", Align::Min, false),
                ("Files", Align::Max, false),
                ("Size", Align::Max, false),
                ("Added", Align::Max, false),
                ("", Align::Min, false),
            ],
            &mut sort,
        );
        let row_height = 46.0;
        ScrollArea::vertical()
            .id_salt("backup-snapshots")
            .auto_shrink([false, false])
            .show_rows(ui, row_height, listing.snapshots.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for info in &listing.snapshots[range] {
                    let (row, response) =
                        widgets::list_row(ui, pal, row_height, false, Sense::click());
                    let cells = columns.cells(row, 12.0);
                    let when = taken_at(info);
                    let upper = egui::Rect::from_min_max(
                        cells[0].min,
                        egui::pos2(cells[0].right(), cells[0].center().y + 2.0),
                    );
                    let lower = egui::Rect::from_min_max(
                        egui::pos2(cells[0].left(), cells[0].center().y - 1.0),
                        cells[0].max,
                    );
                    widgets::paint_text(
                        ui,
                        upper.translate(vec2(0.0, 3.0)),
                        &format_timestamp(when),
                        text::body_font(14.0),
                        pal.text,
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        lower.translate(vec2(0.0, -2.0)),
                        &format_age(when),
                        text::body_font(12.0),
                        pal.text_faint,
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        cells[1],
                        &contents_of(info),
                        text::body_font(13.0),
                        if info.label.is_empty() {
                            pal.text_muted
                        } else {
                            pal.text
                        },
                        Align::Min,
                    );
                    widgets::paint_text(
                        ui,
                        cells[2],
                        &format_count(info.files),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    widgets::paint_text(
                        ui,
                        cells[3],
                        &format_bytes(info.bytes),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    widgets::paint_text(
                        ui,
                        cells[4],
                        &format_bytes(info.stored_bytes),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    widgets::in_cell_right(ui, cells[5], |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if widgets::icon_button(ui, pal, icon::TRASH, "Remove this backup")
                            .clicked()
                        {
                            delete = Some(info.clone());
                        }
                        if widgets::icon_button(ui, pal, icon::SHIELD_CHECK, "Check this backup")
                            .clicked()
                        {
                            check = Some(VerifyScope::Snapshot(info.id.clone()));
                        }
                        if widgets::icon_button(
                            ui,
                            pal,
                            icon::FOLDER_OPEN,
                            "Look inside and restore",
                        )
                        .clicked()
                        {
                            open = Some(info.id.clone());
                        }
                    });
                    let mut tip = format!(
                        "{}\n{} of file contents were new in this backup.",
                        info.sources.join("\n"),
                        format_bytes(info.new_bytes)
                    );
                    if info.skipped > 0 {
                        tip.push_str(&format!("\n{} left out.", plural(info.skipped, "item")));
                    }
                    let response = response.on_hover_text(tip);
                    if response.double_clicked() {
                        open = Some(info.id.clone());
                    }
                }
            });

        if let Some(id) = open {
            self.open_snapshot(ctx, &destination, id);
        }
        if let Some(scope) = check {
            self.start_verify(ctx, &destination, scope);
        }
        if let Some(info) = delete {
            self.confirm_delete = Some(info);
        }
    }

    fn browse_view(&mut self, ui: &mut Ui, pal: &Palette) {
        let Some(browse) = &mut self.browse else {
            return;
        };
        let info = browse.snapshot.info().clone();
        let mut close = false;
        let mut go: Option<(Option<u32>, String)> = None;
        let mut restore: Option<(Vec<Selection>, String)> = None;

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if widgets::icon_button(ui, pal, icon::ARROW_LEFT, "Back to all backups")
                        .clicked()
                    {
                        close = true;
                    }
                    // Breadcrumbs: the backup, the source, then each folder.
                    let many_roots = browse.snapshot.roots().len() > 1;
                    let top = format_timestamp(taken_at(&info));
                    if many_roots || browse.root.is_none() {
                        if widgets::quiet_button(ui, pal, &top).clicked() {
                            go = Some((None, String::new()));
                        }
                    } else {
                        ui.add_space(6.0);
                        ui.label(text::muted(top, pal));
                        ui.add_space(6.0);
                    }
                    if let Some(root) = browse.root {
                        let name = browse
                            .snapshot
                            .roots()
                            .get(root as usize)
                            .map(|r| r.name.clone())
                            .unwrap_or_default();
                        ui.label(text::icon(icon::CARET_RIGHT, 12.0).color(pal.text_faint));
                        if widgets::quiet_button(ui, pal, &name).clicked() {
                            go = Some((Some(root), String::new()));
                        }
                        let mut so_far = String::new();
                        for part in browse.dir.split('\\').filter(|p| !p.is_empty()) {
                            if !so_far.is_empty() {
                                so_far.push('\\');
                            }
                            so_far.push_str(part);
                            ui.label(text::icon(icon::CARET_RIGHT, 12.0).color(pal.text_faint));
                            if widgets::quiet_button(ui, pal, part).clicked() {
                                go = Some((Some(root), so_far.clone()));
                            }
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.add_space(6.0);
                        ui.add_enabled_ui(!browse.picked.is_empty(), |ui| {
                            if widgets::primary_button(ui, pal, "Restore selected").clicked() {
                                let selection: Vec<Selection> = browse
                                    .picked
                                    .iter()
                                    .map(|(root, path)| Selection {
                                        root: *root,
                                        path: path.clone(),
                                    })
                                    .collect();
                                let what = plural(selection.len() as u64, "selected item");
                                restore = Some((selection, what));
                            }
                        });
                        if widgets::button(ui, pal, "Restore everything").clicked() {
                            restore = Some((Vec::new(), "everything in this backup".to_owned()));
                        }
                    });
                });
            });
        widgets::hairline(ui, pal);

        let columns = Columns(vec![
            Col::Fixed(22.0),
            Col::Flex(1.0),
            Col::Fixed(140.0),
            Col::Fixed(90.0),
            Col::Fixed(90.0),
        ]);
        let mut sort = (usize::MAX, true);
        widgets::list_header(
            ui,
            pal,
            &columns,
            &[
                ("", Align::Min, false),
                ("Name", Align::Min, false),
                ("Changed", Align::Min, false),
                ("Files", Align::Max, false),
                ("Size", Align::Max, false),
            ],
            &mut sort,
        );
        if browse.rows.is_empty() {
            widgets::empty_state(ui, pal, icon::FOLDER_OPEN, "This folder is empty", "", None);
        }
        let row_height = 38.0;
        let mut toggle: Option<(u32, String)> = None;
        ScrollArea::vertical()
            .id_salt(("backup-browse", browse.root, browse.dir.clone()))
            .auto_shrink([false, false])
            .show_rows(ui, row_height, browse.rows.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for entry in &browse.rows[range] {
                    let is_dir = entry.kind == EntryKind::Dir;
                    let sense = if is_dir {
                        Sense::click()
                    } else {
                        Sense::hover()
                    };
                    let (row, response) = widgets::list_row(ui, pal, row_height, false, sense);
                    let cells = columns.cells(row, 10.0);
                    let key = (entry.root, entry.path.clone());
                    let inherited =
                        browse.root.is_some() && browse.inherited(entry.root, &entry.path);
                    let state = if inherited || browse.picked.contains(&key) {
                        Check::On
                    } else if is_dir && browse.contains_picked(entry.root, &entry.path) {
                        Check::Mixed
                    } else {
                        Check::Off
                    };
                    widgets::in_cell(ui, cells[0], |ui| {
                        ui.add_enabled_ui(!inherited, |ui| {
                            if widgets::check(ui, pal, state, &entry.name).clicked() {
                                toggle = Some(key.clone());
                            }
                        })
                        .response
                        .on_disabled_hover_text("Included with the folder it is in.");
                    });
                    let glyph = if is_dir { icon::FOLDER } else { icon::FILE };
                    let glyph_rect =
                        egui::Rect::from_min_size(cells[1].min, vec2(22.0, cells[1].height()));
                    ui.painter().text(
                        glyph_rect.left_center() + vec2(8.0, 0.0),
                        egui::Align2::CENTER_CENTER,
                        glyph,
                        text::body_font(17.0),
                        if is_dir {
                            pal.accent_text
                        } else {
                            pal.text_faint
                        },
                    );
                    let mut name_rect = cells[1];
                    name_rect.set_left(cells[1].left() + 26.0);
                    widgets::paint_text(
                        ui,
                        name_rect,
                        &entry.name,
                        text::body_font(14.0),
                        pal.text,
                        Align::Min,
                    );
                    let changed = backup::filetime_to_system(entry.modified)
                        .map(format_timestamp)
                        .unwrap_or_default();
                    widgets::paint_text(
                        ui,
                        cells[2],
                        &changed,
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Min,
                    );
                    let files = if is_dir {
                        format_count(entry.files)
                    } else {
                        String::new()
                    };
                    widgets::paint_text(
                        ui,
                        cells[3],
                        &files,
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    widgets::paint_text(
                        ui,
                        cells[4],
                        &format_bytes(entry.size),
                        text::body_font(13.0),
                        pal.text_muted,
                        Align::Max,
                    );
                    if is_dir
                        && response
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                    {
                        go = Some((Some(entry.root), entry.path.clone()));
                    }
                }
            });

        if let Some(key) = toggle
            && !browse.picked.remove(&key)
        {
            // Ticking a folder covers what was ticked inside it.
            let (root, path) = (key.0, key.1.clone());
            browse.picked.retain(|(r, p)| {
                !(*r == root
                    && p.len() > path.len()
                    && (path.is_empty()
                        || (p.starts_with(path.as_str()) && p.as_bytes()[path.len()] == b'\\')))
            });
            browse.picked.insert(key);
        }
        if let Some((root, dir)) = go {
            browse.open(root, &dir);
        }
        if let Some((selection, what)) = restore {
            self.restore = Some(RestoreDialog {
                snapshot: browse.snapshot.clone(),
                selection,
                what,
                to_original: true,
                folder: None,
                overwrite: Overwrite::Skip,
            });
        }
        if close {
            self.browse = None;
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, cx: &mut Cx, cfg: &BackupSettings) {
        let pal = *cx.pal;
        let Some(destination) = cfg.destination.clone() else {
            return;
        };

        if let Some(info) = self.confirm_delete.clone() {
            let body = format!(
                "The backup of {} is removed for good, along with the versions of files that only it holds. Other backups stay complete.",
                format_timestamp(taken_at(&info))
            );
            match widgets::confirm_dialog(
                ctx,
                &pal,
                "backup-delete",
                "Remove this backup?",
                &body,
                "Remove",
                true,
            ) {
                Confirm::Yes => {
                    self.confirm_delete = None;
                    self.start_delete(ctx, &destination, info.id);
                }
                Confirm::No => self.confirm_delete = None,
                Confirm::Pending => {}
            }
        }

        let mut close = false;
        let mut go = false;
        if let Some(dialog) = &mut self.restore {
            let modal = egui::Modal::new(egui::Id::new("backup-restore"))
                .backdrop_color(egui::Color32::from_black_alpha(if pal.dark {
                    150
                } else {
                    90
                }))
                .show(ctx, |ui| {
                    ui.set_width(500.0);
                    ui.label(text::section("Restore from this backup"));
                    ui.label(text::muted(
                        format!(
                            "Brings back {} as it was on {}.",
                            dialog.what,
                            format_timestamp(taken_at(dialog.snapshot.info()))
                        ),
                        &pal,
                    ));
                    ui.add_space(10.0);

                    ui.label(text::strong("Where to"));
                    let mut place = usize::from(!dialog.to_original);
                    widgets::tabs(
                        ui,
                        &pal,
                        &mut place,
                        &["Where it came from", "Another folder"],
                    );
                    dialog.to_original = place == 0;
                    ui.add_space(4.0);
                    if dialog.to_original {
                        let sources = &dialog.snapshot.info().sources;
                        ui.add(egui::Label::new(text::small(sources.join(", "), &pal)).truncate());
                    } else {
                        ui.horizontal(|ui| {
                            match &dialog.folder {
                                Some(folder) => ui.label(text::body(folder.display().to_string())),
                                None => ui.label(text::muted("No folder chosen", &pal)),
                            };
                            if widgets::quiet_button(ui, &pal, "Choose").clicked()
                                && let Some(folder) = rfd::FileDialog::new()
                                    .set_title("Choose a folder to restore into")
                                    .pick_folder()
                            {
                                dialog.folder = Some(folder);
                            }
                        });
                    }
                    ui.add_space(10.0);

                    ui.label(text::strong("When a different file is already there"));
                    let choices = [
                        (Overwrite::Skip, "Leave it alone and tell me"),
                        (
                            Overwrite::KeepBoth,
                            "Keep both: restore under a new name beside it",
                        ),
                        (Overwrite::Replace, "Replace it with the backed-up version"),
                    ];
                    let current = choices
                        .iter()
                        .find(|(value, _)| *value == dialog.overwrite)
                        .map_or("", |(_, label)| *label);
                    egui::ComboBox::from_id_salt("backup-restore-overwrite")
                        .selected_text(current)
                        .width(360.0)
                        .show_ui(ui, |ui| {
                            for (value, label) in choices {
                                ui.selectable_value(&mut dialog.overwrite, value, label);
                            }
                        });
                    ui.label(text::small(
                        "Files that are already identical are never touched.",
                        &pal,
                    ));
                    ui.add_space(14.0);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let ready = dialog.to_original || dialog.folder.is_some();
                        ui.add_enabled_ui(ready, |ui| {
                            if widgets::primary_button(ui, &pal, "Restore").clicked() {
                                go = true;
                            }
                        });
                        if widgets::button(ui, &pal, "Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            if modal.should_close() {
                close = true;
            }
        }
        if go && let Some(dialog) = self.restore.take() {
            if dialog.overwrite == Overwrite::Replace {
                // Overwriting is the one step here that cannot be taken back.
                self.confirm_replace = Some(dialog);
            } else {
                self.start_restore(ctx, &destination, dialog);
            }
        } else if close {
            self.restore = None;
        }

        if let Some(dialog) = &self.confirm_replace {
            let place = if dialog.to_original {
                "their original folders".to_owned()
            } else {
                dialog
                    .folder
                    .as_ref()
                    .map(|f| f.display().to_string())
                    .unwrap_or_default()
            };
            let body = format!(
                "Files in {place} that differ from the backup are overwritten with the version from {}. The versions that are there now are lost.",
                format_timestamp(taken_at(dialog.snapshot.info()))
            );
            match widgets::confirm_dialog(
                ctx,
                &pal,
                "backup-replace",
                "Replace the files that are there now?",
                &body,
                "Replace",
                true,
            ) {
                Confirm::Yes => {
                    if let Some(dialog) = self.confirm_replace.take() {
                        self.start_restore(ctx, &destination, dialog);
                    }
                }
                Confirm::No => self.confirm_replace = None,
                Confirm::Pending => {}
            }
        }
    }

    // ── System image ─────────────────────────────────────────────────

    fn image_tab(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        let pane = &mut self.image;

        if let Some(result) = pane.listing.poll(&ctx) {
            pane.versions = Some(match result {
                Ok(Ok(versions)) => Ok(versions),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(e),
            });
        }
        if let Some(result) = pane.creating.poll(&ctx) {
            match result {
                Ok(Ok(outcome)) => {
                    cx.toasts.good(format!(
                        "System image saved in {}.",
                        outcome.folder.display()
                    ));
                    pane.versions = None;
                }
                Ok(Err(Error::Cancelled)) => cx.toasts.info("The system image was cancelled."),
                Ok(Err(e)) => cx
                    .toasts
                    .error(format!("The system image was not created: {e}")),
                Err(e) => cx.toasts.error(e),
            }
        }

        ui.label(text::muted(
            "A system image is a copy of the whole Windows drive: Windows itself, programs, settings and files. Windows makes it with its own imaging engine, and its recovery environment can put it back even when the PC no longer starts.",
            &pal,
        ));
        ui.add_space(8.0);

        if !image::is_available() {
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_min_size(ui.available_size() - vec2(38.0, 38.0));
                widgets::empty_state(
                    ui,
                    &pal,
                    icon::HARD_DRIVES,
                    "Not available on this PC",
                    "This edition of Windows does not include the system image tool. File backup works everywhere.",
                    None,
                );
            });
            return;
        }

        ScrollArea::vertical()
            .id_salt("backup-image")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if cx.elevated {
                    Self::image_create_panel(ui, &pal, pane);
                    ui.add_space(6.0);
                    Self::image_list_panel(ui, &pal, pane, &ctx);
                } else {
                    widgets::panel(&pal).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(text::icon(icon::SHIELD, 30.0).color(pal.text_faint));
                            ui.vertical(|ui| {
                                ui.label(text::section("Administrator rights needed"));
                                ui.add_space(-4.0);
                                ui.label(text::muted(
                                    "Windows only lets administrators create and list system images.",
                                    &pal,
                                ));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::primary_button(ui, &pal, "Restart as administrator")
                                    .clicked()
                                {
                                    cx.request_elevation = true;
                                }
                            });
                        });
                        let drives = pane.drives.get_or_insert_with(disk::list_local_drives);
                        let with_images: Vec<String> = drives
                            .iter()
                            .filter(|d| d.is_ready && image::image_folder_on(&d.root).is_some())
                            .map(DriveInfo::display_name)
                            .collect();
                        if !with_images.is_empty() {
                            ui.add_space(6.0);
                            ui.label(text::small(
                                format!("Image folder found on {}.", with_images.join(", ")),
                                &pal,
                            ));
                        }
                    });
                }
                ui.add_space(6.0);
                Self::image_restore_panel(ui, &pal, cx);
            });

        if pane.confirm {
            let target = pane.target.unwrap_or('?');
            let body = format!(
                "Windows copies the system drive and everything needed to start this PC to drive {target}:. This can take an hour or more. You can keep working meanwhile."
            );
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "backup-image",
                "Create a system image?",
                &body,
                "Create image",
                false,
            ) {
                Confirm::Yes => {
                    pane.confirm = false;
                    let request = ImageRequest {
                        target: format!("{target}:"),
                        include: pane.extra.iter().map(|l| format!("{l}:")).collect(),
                        all_critical: true,
                    };
                    pane.creating
                        .start(&ctx, "system-image", move |progress: Progress| {
                            image::create_image(&request, &progress)
                        });
                }
                Confirm::No => pane.confirm = false,
                Confirm::Pending => {}
            }
        }
    }

    fn image_create_panel(ui: &mut Ui, pal: &Palette, pane: &mut ImagePane) {
        widgets::panel(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(text::section("Create an image"));

            if pane.creating.is_running() {
                let progress = pane.creating.progress().cloned();
                ui.add_space(4.0);
                widgets::progress_bar(ui, pal, progress.as_ref().and_then(Progress::fraction), 8.0);
                let message = progress.as_ref().map(Progress::message).unwrap_or_default();
                ui.label(text::muted(
                    if message.is_empty() { "Starting…".to_owned() } else { message },
                    pal,
                ));
                if widgets::button(ui, pal, "Cancel").clicked() {
                    pane.creating.cancel();
                }
                return;
            }

            let drives = pane.drives.get_or_insert_with(disk::list_local_drives).clone();
            let system: Vec<&DriveInfo> = drives.iter().filter(|d| d.is_system).collect();
            let others: Vec<&DriveInfo> = drives
                .iter()
                .filter(|d| !d.is_system && d.is_ready && d.kind == DriveKind::Fixed)
                .collect();

            // What goes into the image.
            let system_name = system
                .first()
                .map(|d| d.display_name())
                .unwrap_or_else(|| "The Windows drive".to_owned());
            ui.label(text::body(format!(
                "{system_name} and the hidden partitions Windows starts from are always included."
            )));
            for drive in &others {
                if Some(drive.letter) == pane.target {
                    pane.extra.remove(&drive.letter);
                    continue;
                }
                let mut on = pane.extra.contains(&drive.letter);
                let label = format!(
                    "Also include {} ({} in use)",
                    drive.display_name(),
                    format_bytes(drive.used_bytes())
                );
                if widgets::checkbox(ui, pal, &mut on, &label) {
                    if on {
                        pane.extra.insert(drive.letter);
                    } else {
                        pane.extra.remove(&drive.letter);
                    }
                }
            }
            let imaged: Vec<String> = system
                .iter()
                .map(|d| d.letter)
                .chain(pane.extra.iter().copied())
                .map(|l| format!("{l}:"))
                .collect();
            let needed: u64 = drives
                .iter()
                .filter(|d| d.is_system || pane.extra.contains(&d.letter))
                .map(DriveInfo::used_bytes)
                .sum();

            // Where it is saved.
            ui.add_space(6.0);
            let candidates: Vec<&DriveInfo> = drives
                .iter()
                .filter(|d| !d.is_system && !pane.extra.contains(&d.letter) && d.kind != DriveKind::Network)
                .collect();
            if candidates.is_empty() {
                if widgets::banner(
                    ui,
                    pal,
                    Tone::Info,
                    icon::INFO,
                    "Connect another drive to save the image to. It cannot be saved on the drive it is a copy of.",
                    Some("Look again"),
                ) {
                    pane.drives = None;
                }
                return;
            }
            let mut problem = None;
            ui.horizontal(|ui| {
                ui.label(text::strong("Save to"));
                let chosen = candidates.iter().find(|d| Some(d.letter) == pane.target);
                egui::ComboBox::from_id_salt("backup-image-target")
                    .selected_text(
                        chosen.map_or("Choose a drive".to_owned(), |d| d.display_name()),
                    )
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for drive in &candidates {
                            let label = format!(
                                "{}, {} free",
                                drive.display_name(),
                                format_bytes(drive.free_bytes)
                            );
                            ui.selectable_value(&mut pane.target, Some(drive.letter), label);
                        }
                    });
                if widgets::icon_button(ui, pal, icon::ARROWS_CLOCKWISE, "Look for drives again").clicked() {
                    pane.drives = None;
                }
                ui.label(text::small(
                    format!("The image needs about {}.", format_bytes(needed)),
                    pal,
                ));
                if let Some(drive) = chosen {
                    problem = image::target_problem(
                        &TargetFacts {
                            volume: format!("{}:", drive.letter),
                            file_system: drive.file_system.clone(),
                            free_bytes: drive.free_bytes,
                            is_ready: drive.is_ready,
                            is_optical: drive.kind == DriveKind::Optical,
                        },
                        &imaged,
                        needed,
                    );
                }
            });
            if let Some(problem) = &problem {
                ui.add_space(4.0);
                widgets::banner(ui, pal, Tone::Warn, icon::WARNING, problem, None);
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(pane.target.is_some() && problem.is_none(), |ui| {
                if widgets::primary_button(ui, pal, "Create system image").clicked() {
                    pane.confirm = true;
                }
            });
        });
    }

    fn image_list_panel(ui: &mut Ui, pal: &Palette, pane: &mut ImagePane, ctx: &egui::Context) {
        if pane.versions.is_none() && !pane.listing.is_running() && !pane.creating.is_running() {
            pane.listing
                .start(ctx, "system-image-list", |_| image::list_images(None));
        }
        widgets::panel(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(text::section("Images of this PC"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if pane.listing.is_running() {
                        widgets::spinner(ui, pal, 18.0);
                    } else if widgets::icon_button(ui, pal, icon::ARROWS_CLOCKWISE, "Look again")
                        .clicked()
                    {
                        pane.versions = None;
                    }
                });
            });
            match &pane.versions {
                None => {
                    ui.label(text::muted("Asking Windows…", pal));
                }
                Some(Err(e)) => {
                    ui.label(text::muted(
                        format!("Windows did not list any images: {e}"),
                        pal,
                    ));
                }
                Some(Ok(versions)) if versions.is_empty() => {
                    ui.label(text::muted(
                        "Windows knows of no system image of this PC yet.",
                        pal,
                    ));
                }
                Some(Ok(versions)) => {
                    for version in versions.iter().rev() {
                        ui.horizontal(|ui| {
                            ui.label(text::icon(icon::HARD_DRIVES, 18.0).color(pal.accent_text));
                            ui.label(text::strong(&version.time));
                            ui.label(text::muted(&version.target, pal));
                        })
                        .response
                        .on_hover_text(version.details.join("\n"));
                    }
                }
            }
        });
    }

    fn image_restore_panel(ui: &mut Ui, pal: &Palette, cx: &mut Cx) {
        widgets::panel(pal).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(text::section("Getting an image back"));
            ui.add_space(2.0);
            for (index, step) in image::RESTORE_STEPS.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        vec2(22.0, 20.0),
                        Layout::left_to_right(Align::Min),
                        |ui| {
                            // Same width for every number, so the steps line up.
                            ui.set_min_width(18.0);
                            ui.label(
                                text::figure(format!("{}", index + 1), 16.0).color(pal.accent_text),
                            );
                        },
                    );
                    ui.add(egui::Label::new(text::body(*step)).wrap());
                });
            }
            ui.add_space(6.0);
            if widgets::button(ui, pal, "Open recovery settings").clicked()
                && let Err(e) = image::open_recovery_settings()
            {
                cx.toasts
                    .error(format!("Windows Settings did not open: {e}"));
            }
        });
    }
}
