//! The application shell: window chrome, navigation and page hosting.

use crate::pages::{
    Cx, NAV_GROUPS, Page, apps::AppsPage, backup::BackupPage, clean::CleanPage, disk::DiskPage,
    drivers::DriversPage, duplicates::DuplicatesPage, empty::EmptyPage, extensions::ExtensionsPage,
    health::HealthPage, options::OptionsPage, performance::PerformancePage, privacy::PrivacyPage,
    registry::RegistryPage, restore::RestorePage, startup::StartupPage, sysinfo::SysInfoPage,
    tools::ToolsPage, updates::UpdatesPage, wiper::WiperPage,
};
use crate::theme::{self, Palette, mix, text};
use crate::widgets::{self, Toasts};
use eframe::egui::{self, Align, Layout, Margin, ScrollArea, Ui};
use egui_phosphor::regular as icon;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zest_engine::Settings;
use zest_engine::core::{brand, elevation};

/// Options decided on the command line.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// Page to open first.
    pub page: Option<Page>,
    /// Folder to save one screenshot per page into, then exit.
    pub screenshots: Option<PathBuf>,
    /// In screenshot mode, run each page's read-only analysis first.
    pub screenshot_scan: bool,
    /// Force a theme regardless of the saved setting.
    pub theme: Option<zest_engine::settings::Theme>,
    /// Show made-up data instead of scanning this PC (for documentation).
    pub demo: bool,
}

struct Shots {
    dir: PathBuf,
    pages: Vec<Page>,
    index: usize,
    frames: u32,
    started: bool,
    requested: bool,
    scan: bool,
}

/// The whole application state.
pub struct App {
    settings: Settings,
    pal: Palette,
    page: Page,
    toasts: Toasts,
    elevated: bool,
    theme_override: Option<zest_engine::settings::Theme>,
    save_at: Option<Instant>,
    shots: Option<Shots>,
    clean: CleanPage,
    health: HealthPage,
    registry: RegistryPage,
    startup: StartupPage,
    apps: AppsPage,
    updates: UpdatesPage,
    drivers: DriversPage,
    performance: PerformancePage,
    extensions: ExtensionsPage,
    disk: DiskPage,
    duplicates: DuplicatesPage,
    wiper: WiperPage,
    restore: RestorePage,
    privacy: PrivacyPage,
    tools: ToolsPage,
    backup: BackupPage,
    sysinfo: SysInfoPage,
    empty: EmptyPage,
    options: OptionsPage,
}

impl App {
    /// Creates the app, installing fonts and the style.
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> Self {
        let mut settings = Settings::load();
        if let Some(theme) = launch.theme {
            settings.appearance.theme = theme;
        }
        let pal = Palette::for_setting(settings.appearance.theme);
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, &pal);
        cc.egui_ctx.set_zoom_factor(settings.appearance.zoom);

        let shots = launch.screenshots.map(|dir| Shots {
            dir,
            pages: launch.page.map_or_else(|| Page::ALL.to_vec(), |p| vec![p]),
            index: 0,
            frames: 0,
            started: false,
            requested: false,
            scan: launch.screenshot_scan && !launch.demo,
        });
        let page = match &shots {
            Some(shots) => shots.pages[0],
            None => launch.page.unwrap_or(Page::Health),
        };
        let demo = launch.demo;
        let mut app = App {
            settings,
            pal,
            page,
            toasts: Toasts::default(),
            elevated: elevation::is_elevated(),
            theme_override: launch.theme,
            save_at: None,
            shots,
            clean: CleanPage::default(),
            health: HealthPage::default(),
            registry: RegistryPage::default(),
            startup: StartupPage::default(),
            apps: AppsPage::default(),
            updates: UpdatesPage::default(),
            drivers: DriversPage::default(),
            performance: PerformancePage::default(),
            extensions: ExtensionsPage::default(),
            disk: DiskPage::default(),
            duplicates: DuplicatesPage::default(),
            wiper: WiperPage::default(),
            restore: RestorePage::default(),
            privacy: PrivacyPage::default(),
            tools: ToolsPage::default(),
            backup: BackupPage::default(),
            sysinfo: SysInfoPage::default(),
            empty: EmptyPage::default(),
            options: OptionsPage::default(),
        };
        if demo {
            app.health.load_demo(crate::demo::health_report());
            app.clean.load_demo(crate::demo::clean_analysis());
            app.disk
                .load_demo(crate::demo::drives(), crate::demo::disk_analysis());
            app.privacy.load_demo();
            app.tools.load_demo();
            app.backup.load_demo();
            app.sysinfo.load_demo();
            app.empty.load_demo();
        }
        app
    }

    fn page_busy(&self) -> bool {
        match self.page {
            Page::Clean => self.clean.is_busy(),
            Page::Health => self.health.is_busy(),
            Page::Registry => self.registry.is_busy(),
            Page::Startup => self.startup.is_busy(),
            Page::Apps => self.apps.is_busy(),
            Page::Updates => self.updates.is_busy(),
            Page::Drivers => self.drivers.is_busy(),
            Page::Performance => self.performance.is_busy(),
            Page::Extensions => self.extensions.is_busy(),
            Page::Disk => self.disk.is_busy(),
            Page::Duplicates => self.duplicates.is_busy(),
            Page::Wiper => self.wiper.is_busy(),
            Page::Restore => self.restore.is_busy(),
            Page::Privacy => self.privacy.is_busy(),
            Page::Tools => self.tools.is_busy(),
            Page::Backup => self.backup.is_busy(),
            Page::SysInfo => self.sysinfo.is_busy(),
            Page::Empty => self.empty.is_busy(),
            Page::Options => self.options.is_busy(),
        }
    }

    /// Starts the read-only analysis of the current page (screenshot mode).
    fn start_page_scan(&mut self, ctx: &egui::Context) {
        match self.page {
            Page::Clean => self.clean.start_analysis(ctx, &self.settings),
            Page::Health => self.health.start_check(ctx, &self.settings),
            Page::Registry => self.registry.start_scan(ctx, &self.settings),
            Page::Updates => self.updates.start_check(ctx, &self.settings),
            Page::Disk => self.disk.start(ctx),
            Page::Duplicates => self.duplicates.start(ctx),
            Page::Privacy => self.privacy.start_scan(ctx, &self.settings),
            Page::Tools => self.tools.start_scan(ctx, &self.settings),
            Page::Backup => self.backup.start_scan(ctx, &self.settings),
            Page::SysInfo => self.sysinfo.start_scan(ctx, &self.settings),
            Page::Empty => self.empty.start_scan(ctx, &self.settings),
            _ => {}
        }
    }

    fn rail(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            widgets::brand_mark(ui, 26.0);
            ui.add_space(2.0);
            ui.label(text::figure(brand::NAME, 23.0));
        });
        ui.add_space(14.0);

        let mut goto = None;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 96.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (heading, pages) in NAV_GROUPS {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(heading)
                                .size(12.0)
                                .color(pal.text_faint),
                        );
                    });
                    ui.add_space(2.0);
                    for page in pages {
                        if widgets::nav_item(
                            ui,
                            &pal,
                            page.icon(),
                            page.label(),
                            self.page == *page,
                            None,
                        ) {
                            goto = Some(*page);
                        }
                    }
                }
            });

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let status = if self.elevated {
                    format!("{}  Administrator", icon::SHIELD_CHECK)
                } else {
                    format!("Version {}", brand::VERSION)
                };
                ui.label(egui::RichText::new(status).size(12.0).color(pal.text_faint));
            });
            ui.add_space(4.0);
            if widgets::nav_item(
                ui,
                &pal,
                Page::Options.icon(),
                Page::Options.label(),
                self.page == Page::Options,
                None,
            ) {
                goto = Some(Page::Options);
            }
        });
        if let Some(page) = goto {
            self.page = page;
        }
    }

    fn page_ui(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let mut cx = Cx {
            pal: &pal,
            settings: &mut self.settings,
            toasts: &mut self.toasts,
            elevated: self.elevated,
            goto: None,
            settings_changed: false,
            request_elevation: false,
        };
        // Sleep rules keep working whichever page is open.
        self.performance
            .tick(ui.ctx(), &mut cx, self.page == Page::Performance);
        match self.page {
            Page::Clean => self.clean.ui(ui, &mut cx),
            Page::Health => self.health.ui(ui, &mut cx),
            Page::Registry => self.registry.ui(ui, &mut cx),
            Page::Startup => self.startup.ui(ui, &mut cx),
            Page::Apps => self.apps.ui(ui, &mut cx),
            Page::Updates => self.updates.ui(ui, &mut cx),
            Page::Drivers => self.drivers.ui(ui, &mut cx),
            Page::Performance => self.performance.ui(ui, &mut cx),
            Page::Extensions => self.extensions.ui(ui, &mut cx),
            Page::Disk => self.disk.ui(ui, &mut cx),
            Page::Duplicates => self.duplicates.ui(ui, &mut cx),
            Page::Wiper => self.wiper.ui(ui, &mut cx),
            Page::Restore => self.restore.ui(ui, &mut cx),
            Page::Privacy => self.privacy.ui(ui, &mut cx),
            Page::Tools => self.tools.ui(ui, &mut cx),
            Page::Backup => self.backup.ui(ui, &mut cx),
            Page::SysInfo => self.sysinfo.ui(ui, &mut cx),
            Page::Empty => self.empty.ui(ui, &mut cx),
            Page::Options => self.options.ui(ui, &mut cx),
        }
        let (goto, changed, elevate) = (cx.goto, cx.settings_changed, cx.request_elevation);
        if let Some(page) = goto {
            // "Choose what to keep" on the Health page means the cookie list.
            if page == Page::Options && self.page == Page::Health {
                self.options.tab = 2;
            }
            // From Performance it means background watching.
            if page == Page::Options && self.page == Page::Performance {
                self.options.tab = 5;
            }
            self.page = page;
        }
        if changed {
            self.settings_changed(ui.ctx());
        }
        if elevate {
            self.restart_elevated(ui.ctx());
        }
    }

    /// Re-applies appearance and schedules a save shortly after the last change.
    fn settings_changed(&mut self, ctx: &egui::Context) {
        let theme = self
            .theme_override
            .unwrap_or(self.settings.appearance.theme);
        let pal = Palette::for_setting(theme);
        if pal != self.pal {
            self.pal = pal;
            theme::apply(ctx, &pal);
        }
        ctx.set_zoom_factor(self.settings.appearance.zoom);
        self.save_at = Some(Instant::now() + Duration::from_millis(400));
        ctx.request_repaint_after(Duration::from_millis(450));
    }

    /// Keeps the saved window size in step with the real window.
    fn remember_window(&mut self, ctx: &egui::Context) {
        let (size, maximized) = ctx.input(|i| {
            let viewport = i.viewport();
            (
                viewport.inner_rect.map(|r| r.size()),
                viewport.maximized.unwrap_or(false),
            )
        });
        let window = &mut self.settings.window;
        let mut changed = window.maximized != maximized;
        window.maximized = maximized;
        if let Some(size) = size
            && !maximized
            && size.x >= 900.0
            && size.y >= 600.0
            && ((window.width - size.x).abs() > 1.0 || (window.height - size.y).abs() > 1.0)
        {
            window.width = size.x;
            window.height = size.y;
            changed = true;
        }
        if changed && self.save_at.is_none() {
            // Saved together with the next settings write, or on exit.
            self.save_at = Some(Instant::now() + Duration::from_secs(30));
        }
    }

    fn save_if_due(&mut self, force: bool) {
        // Screenshot runs must never touch the user's saved settings.
        if self.shots.is_some() {
            self.save_at = None;
            return;
        }
        if let Some(at) = self.save_at
            && (force || Instant::now() >= at)
        {
            self.save_at = None;
            if let Err(e) = self.settings.save() {
                log::error!("could not save settings: {e}");
                self.toasts
                    .error(format!("Settings could not be saved: {e}"));
            }
        }
    }

    fn restart_elevated(&mut self, ctx: &egui::Context) {
        self.save_if_due(true);
        let args = vec![
            "--page".to_owned(),
            self.page.slug().to_owned(),
            crate::RESTARTED_ARG.to_owned(),
        ];
        match elevation::relaunch_elevated(&args) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(zest_engine::core::Error::Cancelled) => {}
            Err(e) => self
                .toasts
                .error(format!("Could not restart as administrator: {e}")),
        }
    }

    /// Drives screenshot mode: visit each page, wait until it is idle, save
    /// a PNG, move on, and close the window after the last page.
    fn drive_screenshots(&mut self, ctx: &egui::Context) {
        if self.shots.is_none() {
            return;
        }
        ctx.request_repaint();
        let busy = self.page_busy();

        // Did the image we asked for arrive?
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let mut start_scan = false;
        let mut next_page = None;
        let mut finished = false;
        if let Some(shots) = &mut self.shots {
            shots.frames += 1;
            if let Some(image) = image {
                let file = shots
                    .dir
                    .join(format!("{}.png", shots.pages[shots.index].slug()));
                let saved = std::fs::create_dir_all(&shots.dir).is_ok()
                    && image::save_buffer(
                        &file,
                        image.as_raw(),
                        image.width() as u32,
                        image.height() as u32,
                        image::ColorType::Rgba8,
                    )
                    .is_ok();
                if !saved {
                    log::error!("could not save screenshot {}", file.display());
                }
                shots.index += 1;
                shots.frames = 0;
                shots.started = false;
                shots.requested = false;
                match shots.pages.get(shots.index) {
                    Some(page) => next_page = Some(*page),
                    None => finished = true,
                }
            } else if !shots.requested {
                if shots.scan && !shots.started && shots.frames > 3 {
                    shots.started = true;
                    start_scan = true;
                } else if shots.frames > 20 && (!busy || shots.frames > 3600) {
                    shots.requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                }
            }
        }
        if start_scan {
            self.start_page_scan(ctx);
        }
        if let Some(page) = next_page {
            self.page = page;
        }
        if finished {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let pal = self.pal;
        self.drive_screenshots(&ctx);

        egui::Panel::left("rail")
            .exact_size(214.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.rail)
                    .inner_margin(Margin::symmetric(10, 0)),
            )
            .show(ui, |ui| self.rail(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg).inner_margin(Margin {
                left: 28,
                right: 28,
                top: 22,
                bottom: 24,
            }))
            .show(ui, |ui| {
                // A hairline separates the rail from the page.
                let edge = ui.max_rect().left() - 28.0;
                ui.painter().vline(
                    edge,
                    ui.ctx().content_rect().y_range(),
                    egui::Stroke::new(1.0, mix(pal.rail, pal.line, 0.7)),
                );
                self.page_ui(ui);
            });

        self.toasts.show(&ctx, &pal);
        self.remember_window(&ctx);
        self.save_if_due(false);
    }

    fn on_exit(&mut self) {
        self.save_if_due(true);
    }
}
