//! Options: appearance, cleaning behaviour, cookies, include/exclude,
//! scheduling, smart cleaning and information about the app.

use super::Cx;
use crate::job::Job;
use crate::theme::{Palette, text};
use crate::widgets::{self, Confirm, Tone};
use eframe::egui::{self, Align, Layout, ScrollArea, Ui};
use egui_phosphor::regular as icon;
use zest_engine::cleaner::cookies::{self, CookieInventory};
use zest_engine::cleaner::{Exclusion, Include, IncludeMode};
use zest_engine::core::delete::WipeMethod;
use zest_engine::core::fmt::{format_age, format_bytes, format_count};
use zest_engine::core::{brand, paths};
use zest_engine::settings::{Frequency, SmartAction, Theme};
use zest_engine::{History, Settings, autostart, ops, schedule};

const TABS: [&str; 7] = [
    "General",
    "Cleaning",
    "Cookies",
    "Include and exclude",
    "Scheduling",
    "Smart cleaning",
    "About",
];
/// Index of the About tab, where the app's own updates are shown.
pub const ABOUT_TAB: usize = TABS.len() - 1;
const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// State of the Options page.
#[derive(Default)]
pub struct OptionsPage {
    /// Tab to show; other pages may set it before navigating here.
    pub tab: usize,
    inventory_job: Job<CookieInventory>,
    inventory: Option<CookieInventory>,
    cookie_search: String,
    new_domain: String,
    new_registry_exclusion: String,
    history: Option<History>,
    schedule_registered: Option<bool>,
    confirm_reset: bool,
}

/// One labelled setting: title and explanation on the left, control on the right.
fn setting(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    description: &str,
    control: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 300.0).max(260.0));
            ui.label(text::strong(title));
            if !description.is_empty() {
                ui.add_space(-6.0);
                ui.add(egui::Label::new(text::small(description, pal)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
    ui.add_space(4.0);
    widgets::hairline(ui, pal);
    ui.add_space(4.0);
}

fn combo<T: PartialEq + Copy>(
    ui: &mut Ui,
    id: &str,
    value: &mut T,
    options: &[(T, &str)],
    width: f32,
) -> bool {
    let current = options
        .iter()
        .find(|(v, _)| v == value)
        .map_or("", |(_, label)| *label);
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(width)
        .show_ui(ui, |ui| {
            for (option, label) in options {
                if ui.selectable_label(*value == *option, *label).clicked() && *value != *option {
                    *value = *option;
                    changed = true;
                }
            }
        });
    changed
}

fn current_exe() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_default()
}

impl OptionsPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.inventory_job.is_running()
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.inventory_job.poll(&ctx) {
            self.inventory = Some(result.unwrap_or_default());
        }
        widgets::page_header(ui, &pal, "Options", "");
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - egui::vec2(2.0, 2.0));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                widgets::tabs(ui, &pal, &mut self.tab, &TABS);
            });
            ScrollArea::vertical()
                .id_salt(("options", self.tab))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(22, 16))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            match self.tab {
                                0 => self.general(ui, cx),
                                1 => self.cleaning(ui, cx),
                                2 => self.cookies(ui, cx),
                                3 => self.include_exclude(ui, cx),
                                4 => self.scheduling(ui, cx),
                                5 => self.smart(ui, cx),
                                _ => self.about(ui, cx),
                            }
                        });
                });
        });

        if self.confirm_reset {
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "reset-settings",
                "Reset all settings?",
                "Your rule selection, cookie keep list, includes, exclusions and schedule return to their defaults. History and backups are kept.",
                "Reset",
                true,
            ) {
                Confirm::Yes => {
                    self.confirm_reset = false;
                    let _ = schedule::unregister();
                    let _ = autostart::set_enabled(&current_exe(), false);
                    *cx.settings = Settings::default();
                    cx.settings_changed = true;
                    self.schedule_registered = None;
                    cx.toasts.info("Settings were reset");
                }
                Confirm::No => self.confirm_reset = false,
                Confirm::Pending => {}
            }
        }
    }

    fn general(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        setting(
            ui,
            &pal,
            "Theme",
            "Follow Windows, or always use one look.",
            |ui| {
                if combo(
                    ui,
                    "theme",
                    &mut cx.settings.appearance.theme,
                    &[
                        (Theme::System, "Same as Windows"),
                        (Theme::Dark, "Dark"),
                        (Theme::Light, "Light"),
                    ],
                    170.0,
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        setting(ui, &pal, "Text size", "Scales the whole interface.", |ui| {
            let mut percent = (cx.settings.appearance.zoom * 100.0).round() as u32;
            if combo(
                ui,
                "zoom",
                &mut percent,
                &[
                    (90, "90 %"),
                    (100, "100 %"),
                    (110, "110 %"),
                    (125, "125 %"),
                    (150, "150 %"),
                ],
                170.0,
            ) {
                cx.settings.appearance.zoom = percent as f32 / 100.0;
                cx.settings_changed = true;
            }
        });
        setting(
            ui,
            &pal,
            "Use the Recycle Bin for tools",
            "Duplicates, large files and uninstall leftovers go to the Recycle Bin instead of being deleted for good.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.general.tools_use_recycle_bin,
                    "Use the Recycle Bin",
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        setting(
            ui,
            &pal,
            "Show Windows entries",
            "List Microsoft's own startup items, services and tasks alongside everyone else's.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.general.show_system_entries,
                    "Show Windows entries",
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        ui.label(text::strong("Ignored software updates"));
        if cx.settings.general.ignored_updates.is_empty() {
            ui.label(text::small(
                "None. Use “Ignore” on the Software updates page to hide a program's updates.",
                &pal,
            ));
        }
        let mut remove: Option<usize> = None;
        for (index, id) in cx.settings.general.ignored_updates.iter().enumerate() {
            ui.horizontal(|ui| {
                if widgets::icon_button(ui, &pal, icon::X, "Offer updates again").clicked() {
                    remove = Some(index);
                }
                ui.label(text::body(id));
            });
        }
        if let Some(index) = remove {
            cx.settings.general.ignored_updates.remove(index);
            cx.settings_changed = true;
        }
    }

    fn cleaning(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        setting(
            ui,
            &pal,
            "Keep recent temporary files",
            "Programs that are running may still need what they wrote a moment ago.",
            |ui| {
                if combo(
                    ui,
                    "min-age",
                    &mut cx.settings.cleaner.min_age_hours,
                    &[
                        (0, "Remove all"),
                        (24, "Keep the last 24 hours"),
                        (48, "Keep the last 48 hours"),
                        (168, "Keep the last week"),
                    ],
                    220.0,
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        setting(
            ui,
            &pal,
            "Secure deletion",
            "Overwrite file contents before deleting. Slower, and no guarantee on SSDs, which move data around internally.",
            |ui| {
                let options: Vec<(WipeMethod, &str)> =
                    WipeMethod::ALL.iter().map(|m| (*m, m.label())).collect();
                if combo(ui, "wipe", &mut cx.settings.cleaner.wipe, &options, 270.0) {
                    cx.settings_changed = true;
                }
            },
        );
        setting(
            ui,
            &pal,
            "Ask before cleaning",
            "Show a confirmation with the totals first.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.cleaner.confirm_before_clean,
                    "Ask before cleaning",
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        setting(
            ui,
            &pal,
            "Clean all user accounts",
            "Applies your selection to every Windows account on this PC. Only works when Zest runs as administrator.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.cleaner.all_users,
                    "Clean all user accounts",
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        ui.add_space(6.0);
        ui.label(text::section("Health check"));
        ui.add_space(4.0);
        setting(
            ui,
            &pal,
            "Remove trackers",
            "Health check clears browser cookies, except for sites on your keep list.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.health.clean_cookies,
                    "Remove trackers",
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        setting(
            ui,
            &pal,
            "Remove browsing history",
            "Health check also clears visited pages and download records.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.health.clean_history,
                    "Remove browsing history",
                ) {
                    cx.settings_changed = true;
                }
            },
        );
    }

    fn cookies(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let ctx = ui.ctx().clone();
        if self.inventory.is_none() && !self.inventory_job.is_running() {
            let users = ops::users(cx.settings);
            self.inventory_job
                .start(&ctx, "cookie-inventory", move |_| {
                    cookies::inventory(&users)
                });
        }
        ui.label(text::muted(
            "Cookies for sites on the keep list survive cleaning, so you stay signed in there. Sub-domains are included.",
            &pal,
        ));
        ui.add_space(8.0);
        ui.columns(2, |columns| {
            // Keep list.
            let ui = &mut columns[0];
            ui.label(text::section("Keep"));
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_domain)
                        .hint_text("example.com")
                        .desired_width(200.0),
                );
                if widgets::button(ui, &pal, "Add").clicked() {
                    match cookies::normalize_domain(&self.new_domain) {
                        Some(domain) => {
                            if !cx.settings.cleaner.cookie_keep.contains(&domain) {
                                cx.settings.cleaner.cookie_keep.push(domain);
                                cx.settings.cleaner.cookie_keep.sort();
                                cx.settings_changed = true;
                            }
                            self.new_domain.clear();
                        }
                        None => cx.toasts.error("That does not look like a site name."),
                    }
                }
            });
            if cx.settings.cleaner.cookie_keep.is_empty() {
                ui.label(text::small(
                    "Nothing is kept: every cookie is removed when cookies are cleaned.",
                    &pal,
                ));
            }
            let mut remove: Option<usize> = None;
            ScrollArea::vertical()
                .id_salt("keep-list")
                .max_height(360.0)
                .show(ui, |ui| {
                    for (index, domain) in cx.settings.cleaner.cookie_keep.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if widgets::icon_button(ui, &pal, icon::X, "Stop keeping").clicked() {
                                remove = Some(index);
                            }
                            ui.label(text::body(domain));
                        });
                    }
                });
            if let Some(index) = remove {
                cx.settings.cleaner.cookie_keep.remove(index);
                cx.settings_changed = true;
            }

            // What is on the PC.
            let ui = &mut columns[1];
            ui.label(text::section("Cookies on this PC"));
            let Some(inventory) = &self.inventory else {
                widgets::spinner(ui, &pal, 22.0);
                return;
            };
            if !inventory.unreadable.is_empty() {
                widgets::banner(
                    ui,
                    &pal,
                    Tone::Warn,
                    icon::WARNING,
                    &format!(
                        "Close {} to list its cookies.",
                        inventory.unreadable.join(" and ")
                    ),
                    None,
                );
            }
            ui.horizontal(|ui| {
                widgets::search_field(ui, &pal, &mut self.cookie_search, "Search sites", 220.0);
                let suggestions = cookies::suggest_keep(&inventory.hosts);
                let missing: Vec<String> = suggestions
                    .into_iter()
                    .filter(|d| !cx.settings.cleaner.cookie_keep.contains(d))
                    .collect();
                if !missing.is_empty()
                    && widgets::quiet_button(ui, &pal, "Keep common sign-ins")
                        .on_hover_text(missing.join(", "))
                        .clicked()
                {
                    cx.settings.cleaner.cookie_keep.extend(missing);
                    cx.settings.cleaner.cookie_keep.sort();
                    cx.settings_changed = true;
                }
            });
            let needle = self.cookie_search.trim().to_lowercase();
            let keep = cookies::KeepList::new(&cx.settings.cleaner.cookie_keep);
            let mut add: Option<String> = None;
            ScrollArea::vertical()
                .id_salt("cookie-hosts")
                .max_height(360.0)
                .show(ui, |ui| {
                    for host in inventory
                        .hosts
                        .iter()
                        .filter(|h| needle.is_empty() || h.host.contains(&needle))
                    {
                        ui.horizontal(|ui| {
                            if keep.keeps(&host.host) {
                                ui.label(text::icon(icon::CHECK, 16.0).color(pal.fresh))
                                    .on_hover_text("On the keep list");
                            } else if widgets::icon_button(
                                ui,
                                &pal,
                                icon::PLUS,
                                "Keep this site's cookies",
                            )
                            .clicked()
                            {
                                add = Some(host.host.clone());
                            }
                            ui.label(text::body(&host.host));
                            ui.label(text::small(format_count(host.count), &pal));
                        });
                    }
                });
            if let Some(host) = add
                && let Some(domain) = cookies::normalize_domain(&host)
            {
                cx.settings.cleaner.cookie_keep.push(domain);
                cx.settings.cleaner.cookie_keep.sort();
                cx.settings_changed = true;
            }
        });
    }

    fn include_exclude(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        ui.label(text::section("Also clean"));
        ui.label(text::small(
            "Your own files and folders, shown under “Custom” in Custom clean. Protected system locations are refused.",
            &pal,
        ));
        ui.add_space(4.0);
        let mut remove: Option<usize> = None;
        let mut changed = false;
        for (index, include) in cx.settings.cleaner.includes.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                if widgets::icon_button(ui, &pal, icon::X, "Remove").clicked() {
                    remove = Some(index);
                }
                ui.add(egui::Label::new(text::body(include.path.display().to_string())).truncate());
                if include.path.is_dir() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if combo(
                            ui,
                            &format!("include-mode-{index}"),
                            &mut include.mode,
                            &[
                                (IncludeMode::FilesOnly, "Files in this folder"),
                                (IncludeMode::FilesAndSubfolders, "Files and sub-folders"),
                                (IncludeMode::WholeFolder, "The whole folder"),
                            ],
                            190.0,
                        ) {
                            changed = true;
                        }
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut include.pattern)
                                .hint_text("*.tmp;*.log")
                                .desired_width(120.0),
                        );
                        if edit.changed() {
                            changed = true;
                        }
                    });
                }
            });
        }
        if let Some(index) = remove {
            cx.settings.cleaner.includes.remove(index);
            changed = true;
        }
        ui.horizontal(|ui| {
            if widgets::icon_text_button(ui, &pal, icon::FOLDER_PLUS, "Add folder").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a folder to clean")
                    .pick_folder()
            {
                cx.settings.cleaner.includes.push(Include {
                    path,
                    pattern: String::new(),
                    mode: IncludeMode::FilesOnly,
                });
                changed = true;
            }
            if widgets::icon_text_button(ui, &pal, icon::FILE_PLUS, "Add file").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a file to clean")
                    .pick_file()
            {
                cx.settings.cleaner.includes.push(Include {
                    path,
                    pattern: String::new(),
                    mode: IncludeMode::FilesOnly,
                });
                changed = true;
            }
        });

        ui.add_space(16.0);
        ui.label(text::section("Never clean"));
        ui.label(text::small(
            "Files, folders and registry keys that are always left alone.",
            &pal,
        ));
        ui.add_space(4.0);
        let mut remove: Option<usize> = None;
        for (index, exclusion) in cx.settings.cleaner.exclusions.iter().enumerate() {
            ui.horizontal(|ui| {
                if widgets::icon_button(ui, &pal, icon::X, "Remove").clicked() {
                    remove = Some(index);
                }
                let label = match exclusion {
                    Exclusion::Path(path) => path.display().to_string(),
                    Exclusion::Pattern { dir, pattern } => {
                        format!("{}  ({pattern})", dir.display())
                    }
                    Exclusion::Registry(key) => key.clone(),
                };
                ui.add(egui::Label::new(text::body(label)).truncate());
            });
        }
        if let Some(index) = remove {
            cx.settings.cleaner.exclusions.remove(index);
            changed = true;
        }
        ui.horizontal(|ui| {
            if widgets::icon_text_button(ui, &pal, icon::FOLDER_PLUS, "Add folder").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a folder to protect")
                    .pick_folder()
            {
                cx.settings.cleaner.exclusions.push(Exclusion::Path(path));
                changed = true;
            }
            if widgets::icon_text_button(ui, &pal, icon::FILE_PLUS, "Add file").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_title("Choose a file to protect")
                    .pick_file()
            {
                cx.settings.cleaner.exclusions.push(Exclusion::Path(path));
                changed = true;
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.new_registry_exclusion)
                    .hint_text("HKCU\\Software\\Example")
                    .desired_width(230.0),
            );
            if widgets::button(ui, &pal, "Add registry key").clicked() {
                let key = self.new_registry_exclusion.trim().to_owned();
                if zest_engine::core::reg::RegPath::parse(&key).is_some() {
                    cx.settings
                        .cleaner
                        .exclusions
                        .push(Exclusion::Registry(key.clone()));
                    if !cx.settings.registry.excluded.contains(&key) {
                        cx.settings.registry.excluded.push(key);
                    }
                    self.new_registry_exclusion.clear();
                    changed = true;
                } else {
                    cx.toasts
                        .error("Start the key with HKCU, HKLM, HKCR or HKU.");
                }
            }
        });
        if changed {
            cx.settings_changed = true;
        }
    }

    fn scheduling(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let registered = *self
            .schedule_registered
            .get_or_insert_with(schedule::is_registered);
        let mut changed = false;
        setting(
            ui,
            &pal,
            "Clean on a schedule",
            "Windows starts Zest at the chosen time, cleans your Custom clean selection without opening a window, and closes it again.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.schedule.enabled,
                    "Clean on a schedule",
                ) {
                    changed = true;
                }
            },
        );
        ui.add_enabled_ui(cx.settings.schedule.enabled, |ui| {
            setting(ui, &pal, "How often", "", |ui| {
                if combo(
                    ui,
                    "frequency",
                    &mut cx.settings.schedule.frequency,
                    &[
                        (Frequency::Daily, "Every day"),
                        (Frequency::Weekly, "Every week"),
                        (Frequency::Monthly, "Every month"),
                        (Frequency::AtLogon, "When I sign in"),
                    ],
                    190.0,
                ) {
                    changed = true;
                }
            });
            match cx.settings.schedule.frequency {
                Frequency::Weekly => setting(ui, &pal, "Day", "", |ui| {
                    let options: Vec<(u8, &str)> = WEEKDAYS
                        .iter()
                        .enumerate()
                        .map(|(i, d)| (i as u8, *d))
                        .collect();
                    if combo(
                        ui,
                        "weekday",
                        &mut cx.settings.schedule.weekday,
                        &options,
                        190.0,
                    ) {
                        changed = true;
                    }
                }),
                Frequency::Monthly => setting(ui, &pal, "Day of the month", "", |ui| {
                    let labels: Vec<String> = (1..=28).map(|d| d.to_string()).collect();
                    let options: Vec<(u8, &str)> = labels
                        .iter()
                        .enumerate()
                        .map(|(i, l)| (i as u8 + 1, l.as_str()))
                        .collect();
                    if combo(
                        ui,
                        "monthday",
                        &mut cx.settings.schedule.day_of_month,
                        &options,
                        190.0,
                    ) {
                        changed = true;
                    }
                }),
                _ => {}
            }
            if cx.settings.schedule.frequency != Frequency::AtLogon {
                setting(
                    ui,
                    &pal,
                    "Time",
                    "If the PC is off at that time, the clean runs when it is next on.",
                    |ui| {
                        let minutes: Vec<String> =
                            (0..12).map(|m| format!("{:02}", m * 5)).collect();
                        let minute_options: Vec<(u8, &str)> = minutes
                            .iter()
                            .enumerate()
                            .map(|(i, l)| (i as u8 * 5, l.as_str()))
                            .collect();
                        let mut minute = cx.settings.schedule.minute / 5 * 5;
                        if combo(ui, "minute", &mut minute, &minute_options, 64.0) {
                            cx.settings.schedule.minute = minute;
                            changed = true;
                        }
                        let hours: Vec<String> = (0..24).map(|h| format!("{h:02}")).collect();
                        let hour_options: Vec<(u8, &str)> = hours
                            .iter()
                            .enumerate()
                            .map(|(i, l)| (i as u8, l.as_str()))
                            .collect();
                        if combo(
                            ui,
                            "hour",
                            &mut cx.settings.schedule.hour,
                            &hour_options,
                            64.0,
                        ) {
                            changed = true;
                        }
                    },
                );
            }
        });
        if changed {
            cx.settings_changed = true;
            match schedule::apply(&cx.settings.schedule, &current_exe()) {
                Ok(()) => self.schedule_registered = Some(cx.settings.schedule.enabled),
                Err(e) => {
                    cx.toasts.error(format!(
                        "The schedule could not be saved in Task Scheduler: {e}"
                    ));
                    self.schedule_registered = None;
                }
            }
        }
        let status = if cx.settings.schedule.enabled && registered {
            format!("{}.", schedule::describe(&cx.settings.schedule))
        } else if cx.settings.schedule.enabled {
            "The schedule is switched on but not registered with Windows yet. Change a setting to register it.".to_owned()
        } else {
            "No scheduled clean.".to_owned()
        };
        ui.label(text::muted(status, &pal));
    }

    fn smart(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let mut toggled = false;
        setting(
            ui,
            &pal,
            "Watch in the background",
            "Keeps a small icon in the notification area and starts with Windows. It only reads until it has something to tell you.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.smart.enabled,
                    "Watch in the background",
                ) {
                    toggled = true;
                }
            },
        );
        if toggled {
            cx.settings_changed = true;
            let exe = current_exe();
            if let Err(e) = autostart::set_enabled(&exe, cx.settings.smart.enabled) {
                cx.toasts
                    .error(format!("Could not change the start-up entry: {e}"));
            }
            if cx.settings.smart.enabled {
                // Save first so the agent starts with the new settings.
                let _ = cx.settings.save();
                let _ = zest_engine::core::process::hidden_command(&exe)
                    .arg(autostart::AGENT_ARG)
                    .spawn();
            }
        }
        ui.add_enabled_ui(cx.settings.smart.enabled, |ui| {
            let actions = [
                (SmartAction::Off, "Do nothing"),
                (SmartAction::Notify, "Tell me"),
                (SmartAction::Clean, "Clean automatically"),
            ];
            setting(ui, &pal, "When junk builds up", "Only junk rules are used; cookies and history are never cleaned in the background.", |ui| {
                if combo(ui, "junk-action", &mut cx.settings.smart.junk_action, &actions, 190.0) {
                    cx.settings_changed = true;
                }
                if combo(
                    ui,
                    "junk-threshold",
                    &mut cx.settings.smart.junk_threshold_mb,
                    &[(512, "Over 500 MB"), (1024, "Over 1 GB"), (2048, "Over 2 GB"), (5120, "Over 5 GB"), (10240, "Over 10 GB")],
                    130.0,
                ) {
                    cx.settings_changed = true;
                }
            });
            setting(ui, &pal, "When I close a browser", "Cleans that browser with the rules you selected for it.", |ui| {
                if combo(ui, "browser-action", &mut cx.settings.smart.browser_action, &actions, 190.0) {
                    cx.settings_changed = true;
                }
            });
            setting(ui, &pal, "Check for junk", "", |ui| {
                if combo(
                    ui,
                    "check-interval",
                    &mut cx.settings.smart.check_interval_minutes,
                    &[(15, "Every 15 minutes"), (30, "Every 30 minutes"), (60, "Every hour"), (180, "Every 3 hours")],
                    190.0,
                ) {
                    cx.settings_changed = true;
                }
            });
        });
    }

    fn about(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        let history = self.history.get_or_insert_with(History::load).clone();
        ui.horizontal(|ui| {
            widgets::brand_mark(ui, 44.0);
            ui.vertical(|ui| {
                ui.label(text::figure(brand::NAME, 28.0));
                ui.add_space(-8.0);
                ui.label(text::muted(
                    format!("Version {}. {}.", brand::VERSION, brand::TAGLINE),
                    &pal,
                ));
            });
        });
        ui.add_space(8.0);
        ui.add(
            egui::Label::new(text::muted(
                "Zest has no account and collects nothing. It goes online by itself in two cases only: once a day it asks its public repository for the number of the latest release, and on PCs without the Windows Package Manager it downloads Microsoft's public software catalog when you check for software updates. Both requests are anonymous and both can be switched off on this page. Software and driver updates themselves are downloaded by Windows' own components.",
                &pal,
            ))
            .wrap(),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, &pal, "Project page").clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(brand::REPO_URL));
            }
            if widgets::button(ui, &pal, "Open data folder").clicked() {
                let dir = paths::data_dir();
                let _ = paths::ensure_dir(&dir);
                let _ = zest_engine::core::process::hidden_command("explorer.exe")
                    .arg(dir)
                    .spawn();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::danger_button(ui, &pal, "Reset all settings").clicked() {
                    self.confirm_reset = true;
                }
            });
        });
        ui.add_space(16.0);
        if cx.updater.about_ui(ui, &pal, cx.settings) {
            cx.settings_changed = true;
        }
        ui.add_space(12.0);
        setting(
            ui,
            &pal,
            "Check for updates automatically",
            "Once a day Zest asks its public repository for the number of the latest release. Nothing about you or this PC is sent.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.app_updates.check_automatically,
                    "Check for updates automatically",
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        setting(
            ui,
            &pal,
            "Software catalog fallback",
            "On PCs where the Windows Package Manager cannot be used, checking for software updates downloads Microsoft's public catalog instead (about 4 MB, at most twice a day). PCs with a working package manager never need it.",
            |ui| {
                if widgets::switch(
                    ui,
                    &pal,
                    &mut cx.settings.general.catalog_fallback,
                    "Software catalog fallback",
                ) {
                    cx.settings_changed = true;
                }
            },
        );
        ui.add_space(16.0);
        ui.label(text::section("History"));
        if history.runs == 0 {
            ui.label(text::muted("Nothing cleaned yet.", &pal));
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 28.0;
            for (value, label) in [
                (format_bytes(history.total_bytes), "freed in total"),
                (format_count(history.total_items), "items removed"),
                (format_count(history.runs), "runs"),
            ] {
                ui.vertical(|ui| {
                    ui.label(text::figure(value, 26.0));
                    ui.add_space(-6.0);
                    ui.label(text::small(label, &pal));
                });
            }
        });
        ui.add_space(8.0);
        for entry in history.entries.iter().rev().take(12) {
            ui.horizontal(|ui| {
                ui.label(text::body(entry.kind.label()));
                ui.label(text::muted(format_bytes(entry.bytes), &pal));
                ui.label(text::small(format_age(entry.system_time()), &pal));
            });
        }
    }
}
