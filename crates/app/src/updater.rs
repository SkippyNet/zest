//! Zest's own updates in the interface: the daily check, the offer in the
//! navigation rail and the controls on the About tab.
//!
//! All network and file work happens in [`zest_engine::updates::selfupdate`]
//! on background threads. This module only decides when to start it and
//! shows what came of it.

use crate::job::Job;
use crate::theme::{Palette, text};
use crate::widgets::{self, Confirm, Toasts, Tone};
use eframe::egui::{self, Ui};
use egui_phosphor::regular as icon;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zest_engine::core::brand;
use zest_engine::core::fmt::{format_age, format_bytes};
use zest_engine::updates::selfupdate::{
    self, Applied, CheckOutcome, CopyKind, Release, UpdateError,
};
use zest_engine::{Settings, app_update};

/// What the shell must do after [`Updater::tick`].
#[derive(Default)]
pub struct Tick {
    /// Settings were modified and need saving.
    pub settings_changed: bool,
    /// Close the window: the installer or the new version is taking over.
    pub close: bool,
}

/// State of the update check, the download and the offer.
pub struct Updater {
    kind: CopyKind,
    checking: Job<Result<CheckOutcome, UpdateError>>,
    /// The running check was started by the user, who expects an answer.
    asked: bool,
    /// What the latest check in this session found.
    result: Option<Result<CheckOutcome, UpdateError>>,
    installing: Job<Result<Applied, UpdateError>>,
    confirm: bool,
    /// Why the last attempt to install failed.
    failure: Option<String>,
    /// The automatic check of this session has been considered.
    automatic_done: bool,
    /// Showing sample data: the offer stays out of the navigation rail.
    demo: bool,
}

impl Default for Updater {
    fn default() -> Self {
        Updater {
            kind: CopyKind::current(),
            checking: Job::default(),
            asked: false,
            result: None,
            installing: Job::default(),
            confirm: false,
            failure: None,
            automatic_done: false,
            demo: false,
        }
    }
}

impl Updater {
    /// Shows a made-up release for documentation screenshots.
    pub fn load_demo(&mut self) {
        self.automatic_done = true;
        self.demo = true;
        self.kind = CopyKind::Installed;
        if let Some(version) = selfupdate::parse_version("1.4.0") {
            self.result = Some(Ok(CheckOutcome::Available(Box::new(Release {
                tag: format!("v{version}"),
                title: format!("{} {version}", brand::NAME),
                notes: "## What is new\n\n- Disk analyzer reads large drives about twice as fast\n- Backups can be browsed before restoring\n- Windows privacy: nine more settings\n\n## Fixed\n\n- Switches no longer flicker while a list scrolls".to_owned(),
                page_url: format!("{}/releases/latest", brand::REPO_URL),
                published: None,
                assets: vec![selfupdate::Asset {
                    name: format!("{}-{version}-x64-setup.exe", brand::NAME),
                    url: String::new(),
                    size: 9_840_000,
                }],
                version,
            }))));
        }
    }

    /// True while a check or an installation is running.
    pub fn is_busy(&self) -> bool {
        self.checking.is_running() || self.installing.is_running()
    }

    /// Asks the repository for the latest release. `asked` says whether the
    /// user started the check (and so expects to be told the outcome).
    pub fn start_check(&mut self, ctx: &egui::Context, asked: bool) {
        if self.is_busy() {
            return;
        }
        self.asked = asked;
        self.failure = None;
        self.checking
            .start(ctx, "update-check", |_| selfupdate::check());
    }

    /// The version to offer in the navigation rail, if there is one: found
    /// by this session's check, or remembered from an earlier one.
    pub fn offer(&self, settings: &Settings) -> Option<String> {
        if self.demo {
            return None;
        }
        match &self.result {
            Some(Ok(CheckOutcome::Available(release))) => selfupdate::should_offer(
                &release.version,
                &selfupdate::current_version(),
                settings.app_updates.skipped_version.as_deref(),
            )
            .then(|| release.version.to_string()),
            Some(Ok(CheckOutcome::UpToDate { .. })) => None,
            _ => app_update::version_on_offer(settings),
        }
    }

    /// Call once per frame. Starts the daily check when it is due (only if
    /// `automatic` is set), and picks up finished background work.
    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        settings: &mut Settings,
        toasts: &mut Toasts,
        automatic: bool,
    ) -> Tick {
        let mut tick = Tick::default();
        if automatic && !self.automatic_done {
            self.automatic_done = true;
            if app_update::automatic_check_due(settings, app_update::now()) {
                self.start_check(ctx, false);
            }
        }

        if let Some(finished) = self.checking.poll(ctx) {
            let outcome = finished.unwrap_or_else(|panic| Err(UpdateError::Malformed(panic)));
            tick.settings_changed |=
                app_update::record_check(settings, &outcome, app_update::now());
            match &outcome {
                Ok(CheckOutcome::Available(release)) => {
                    let wanted = selfupdate::should_offer(
                        &release.version,
                        &selfupdate::current_version(),
                        settings.app_updates.skipped_version.as_deref(),
                    );
                    if self.asked || wanted {
                        toasts.info(format!("{} {} is available", brand::NAME, release.version));
                    }
                }
                Ok(CheckOutcome::UpToDate { .. }) if self.asked => {
                    toasts.good(format!("{} is up to date", brand::NAME));
                }
                Err(error) if self.asked => toasts.error(error.to_string()),
                // A failed automatic check is not worth interrupting anyone.
                _ => {}
            }
            self.result = Some(outcome);
        }

        if let Some(finished) = self.installing.poll(ctx) {
            match finished.unwrap_or_else(|panic| Err(UpdateError::Io(panic))) {
                Ok(Applied::InstallerStarted) => tick.close = true,
                Ok(Applied::Replaced { restart }) => {
                    match std::process::Command::new(&restart)
                        .arg(crate::RESTARTED_ARG)
                        .spawn()
                    {
                        Ok(_) => tick.close = true,
                        Err(error) => {
                            log::error!("could not start {}: {error}", restart.display());
                            toasts.good(format!(
                                "{} was updated. Close and reopen it to use the new version.",
                                brand::NAME
                            ));
                        }
                    }
                }
                Err(UpdateError::Cancelled) => {}
                Err(error) => {
                    toasts.error(error.to_string());
                    self.failure = Some(error.to_string());
                }
            }
        }
        tick
    }

    fn start_install(&mut self, ctx: &egui::Context, release: Release) {
        if self.is_busy() {
            return;
        }
        self.failure = None;
        self.installing
            .start(ctx, "update-install", move |progress| {
                let download = selfupdate::download(&release, &progress)?;
                progress.set_message("Installing…");
                selfupdate::apply(&download)
            });
    }

    /// The "Updates" block of the About tab. Returns true when it changed
    /// the settings.
    pub fn about_ui(&mut self, ui: &mut Ui, pal: &Palette, settings: &mut Settings) -> bool {
        let ctx = ui.ctx().clone();
        let mut changed = false;
        ui.label(text::section("Updates"));

        let available = match &self.result {
            Some(Ok(CheckOutcome::Available(release))) => Some((**release).clone()),
            _ => None,
        };

        if self.installing.is_running() {
            self.installing_ui(ui, pal);
        } else if self.checking.is_running() {
            ui.horizontal(|ui| {
                widgets::spinner(ui, pal, 18.0);
                ui.label(text::muted("Checking for a newer version…", pal));
            });
        } else if let Some(release) = &available {
            changed |= self.release_ui(ui, pal, settings, release);
        } else {
            self.status_ui(ui, pal, settings);
            ui.add_space(4.0);
            if widgets::button(ui, pal, "Check for updates").clicked() {
                self.start_check(&ctx, true);
            }
        }
        if let Some(failure) = &self.failure {
            ui.add_space(6.0);
            widgets::banner(ui, pal, Tone::Danger, icon::WARNING_CIRCLE, failure, None);
        }

        if self.confirm {
            if let Some(release) = available {
                let size = release
                    .package_for(self.kind)
                    .map(|package| format!(" ({})", format_bytes(package.size)))
                    .unwrap_or_default();
                let body = format!(
                    "Version {}{size} will be downloaded from the project's releases and checked against its published checksum. {} then closes and reopens with the new version.",
                    release.version,
                    brand::NAME
                );
                match widgets::confirm_dialog(
                    &ctx,
                    pal,
                    "app-update",
                    &format!("Update {}?", brand::NAME),
                    &body,
                    "Update",
                    false,
                ) {
                    Confirm::Yes => {
                        self.confirm = false;
                        self.start_install(&ctx, release);
                    }
                    Confirm::No => self.confirm = false,
                    Confirm::Pending => {}
                }
            } else {
                self.confirm = false;
            }
        }
        changed
    }

    fn installing_ui(&mut self, ui: &mut Ui, pal: &Palette) {
        let progress = self.installing.progress();
        let (done, total) = progress.map_or((0, 0), |p| (p.done(), p.total()));
        let message = progress.map(|p| p.message()).unwrap_or_default();
        ui.label(text::muted(
            if message.is_empty() {
                "Preparing the update…".to_owned()
            } else {
                message
            },
            pal,
        ));
        ui.scope(|ui| {
            ui.set_max_width(420.0);
            widgets::progress_bar(ui, pal, progress.and_then(|p| p.fraction()), 8.0);
        });
        ui.horizontal(|ui| {
            if total > 0 {
                ui.label(text::small(
                    format!("{} of {}", format_bytes(done), format_bytes(total)),
                    pal,
                ));
            }
            if widgets::quiet_button(ui, pal, "Cancel").clicked() {
                self.installing.cancel();
            }
        });
    }

    /// A newer release: what is in it and what can be done with it.
    fn release_ui(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        settings: &mut Settings,
        release: &Release,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(text::icon(icon::ARROW_CIRCLE_UP, 20.0).color(pal.accent_text));
            ui.label(text::strong(format!(
                "{} {} is available",
                brand::NAME,
                release.version
            )));
            ui.label(text::muted(format!("You have {}", brand::VERSION), pal));
        });
        let notes = release.notes_plain();
        if !notes.is_empty() {
            let shown: Vec<&str> = notes.lines().take(14).collect();
            let more = notes.lines().count() > shown.len();
            egui::Frame::new()
                .fill(pal.well)
                .corner_radius(egui::CornerRadius::same(crate::theme::RADIUS))
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width().min(620.0));
                    ui.add(egui::Label::new(text::muted(shown.join("\n"), pal)).wrap());
                    if more {
                        ui.label(text::small("More on the release page.", pal));
                    }
                });
        }
        ui.add_space(4.0);
        let skipped = settings
            .app_updates
            .skipped_version
            .as_deref()
            .and_then(selfupdate::parse_version)
            .is_some_and(|skipped| skipped == release.version);
        ui.horizontal(|ui| {
            if self.kind == CopyKind::Unmanaged {
                if widgets::primary_button(ui, pal, "Open the download page").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&release.page_url));
                }
            } else {
                if widgets::primary_button(ui, pal, "Update now").clicked() {
                    self.confirm = true;
                }
                if widgets::button(ui, pal, "Release page").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&release.page_url));
                }
            }
            if skipped {
                ui.label(text::small("You chose to skip this version.", pal));
            } else if widgets::quiet_button(ui, pal, "Skip this version")
                .on_hover_text("Stop offering this version. The next one is offered again.")
                .clicked()
            {
                settings.app_updates.skipped_version = Some(release.version.to_string());
                changed = true;
            }
        });
        if self.kind == CopyKind::Unmanaged {
            ui.label(text::small(
                format!(
                    "This copy was not set up by the installer or unpacked from the portable package, so {} cannot replace it for you.",
                    brand::NAME
                ),
                pal,
            ));
        }
        changed
    }

    /// No newer release known from this session: say what is known.
    fn status_ui(&self, ui: &mut Ui, pal: &Palette, settings: &Settings) {
        match &self.result {
            Some(Ok(CheckOutcome::UpToDate { latest: Some(_) })) => {
                ui.horizontal(|ui| {
                    ui.label(text::icon(icon::CHECK_CIRCLE, 18.0).color(pal.fresh));
                    ui.label(text::body(format!(
                        "{} {} is the latest version.",
                        brand::NAME,
                        brand::VERSION
                    )));
                });
            }
            Some(Ok(CheckOutcome::UpToDate { latest: None })) => {
                ui.label(text::muted("No release has been published yet.", pal));
            }
            Some(Err(error)) => {
                widgets::banner(
                    ui,
                    pal,
                    Tone::Warn,
                    icon::WIFI_SLASH,
                    &error.to_string(),
                    None,
                );
            }
            _ => {
                if let Some(version) = app_update::version_on_offer(settings) {
                    ui.label(text::body(format!(
                        "Version {version} was available at the last check."
                    )));
                }
                let checked = settings
                    .app_updates
                    .last_checked
                    .and_then(|secs| u64::try_from(secs).ok())
                    .map(|secs| UNIX_EPOCH + Duration::from_secs(secs))
                    .filter(|time| *time <= SystemTime::now());
                ui.label(text::muted(
                    match checked {
                        Some(time) => format!("Last checked {}.", format_age(time)),
                        None => "Not checked yet.".to_owned(),
                    },
                    pal,
                ));
            }
        }
    }
}
