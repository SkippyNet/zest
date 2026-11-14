//! System info: hardware inventory, drive health and battery condition.

use super::Cx;
use crate::job::Job;
use crate::theme::{DISPLAY, Palette, text};
use crate::widgets::{self, Col, Columns, Segment, Tone};
use eframe::egui::{
    self, Align, FontFamily, FontId, Layout, Margin, Rect, ScrollArea, Sense, Ui, pos2, vec2,
};
use egui_phosphor::regular as icon;
use zest_engine::Settings;
use zest_engine::core::Progress;
use zest_engine::core::fmt::{format_bytes, format_count};
use zest_engine::hardware::{
    self, Attention, Battery, Health, Inventory, Level, PhysicalDrive, Volume, format_capacity,
    format_energy, format_frequency, format_link_speed, format_uptime,
};

/// Height of the content of a summary card, so cards in a row line up.
const CARD_HEIGHT: f32 = 196.0;
/// Width of the label column inside a summary card.
const LABEL_WIDTH: f32 = 96.0;

/// State of the System info page.
#[derive(Default)]
pub struct SysInfoPage {
    loading: Job<Inventory>,
    inventory: Option<Inventory>,
    /// Whether a first look has been started (or demo data loaded).
    started: bool,
}

impl SysInfoPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        self.loading.is_running()
    }

    /// Starts the page's read-only analysis (used by screenshot mode).
    pub fn start_scan(&mut self, ctx: &egui::Context, _settings: &Settings) {
        self.started = true;
        self.loading.start(ctx, "hardware", |progress: Progress| {
            hardware::inventory_with_progress(&progress)
        });
    }

    /// Fills the page with made-up data for documentation screenshots.
    pub fn load_demo(&mut self) {
        self.started = true;
        self.inventory = Some(hardware::sample());
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if let Some(result) = self.loading.poll(&ctx) {
            match result {
                Ok(inventory) => self.inventory = Some(inventory),
                Err(e) => cx
                    .toasts
                    .error(format!("The hardware could not be read: {e}")),
            }
        }
        // Looking is harmless and quick, so the page fills itself in.
        if !self.started {
            self.start_scan(&ctx, cx.settings);
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "System info",
                    "What is inside this PC and how healthy it is. Looking changes nothing.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(self.inventory.is_some(), |ui| {
                    if widgets::icon_text_button(ui, &pal, icon::FLOPPY_DISK, "Save report")
                        .clicked()
                        && let Some(inventory) = &self.inventory
                    {
                        save_report(inventory, cx);
                    }
                    if widgets::icon_text_button(ui, &pal, icon::CLIPBOARD_TEXT, "Copy report")
                        .on_hover_text("The report holds no serial numbers, addresses or names.")
                        .clicked()
                        && let Some(inventory) = &self.inventory
                    {
                        ctx.copy_text(hardware::to_text(inventory));
                        cx.toasts.good("Report copied.");
                    }
                });
                ui.add_enabled_ui(!self.is_busy(), |ui| {
                    if widgets::button(ui, &pal, "Look again").clicked() {
                        self.start_scan(&ctx, cx.settings);
                    }
                });
                if self.is_busy() && self.inventory.is_some() {
                    widgets::spinner(ui, &pal, 20.0);
                }
            });
        });

        let Some(inventory) = &self.inventory else {
            widgets::panel(&pal).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height((ui.available_height() - 40.0).max(160.0));
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.3).clamp(30.0, 180.0));
                    ui.label(text::figure("Looking at the hardware…", 28.0));
                    ui.add_space(4.0);
                    ui.scope(|ui| {
                        ui.set_max_width(420.0);
                        let fraction = self.loading.progress().and_then(|p| p.fraction());
                        widgets::progress_bar(ui, &pal, fraction, 8.0);
                    });
                });
            });
            return;
        };

        let mut elevate = false;
        ScrollArea::vertical()
            .id_salt("sysinfo")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Leave room for the floating scroll bar.
                ui.set_width(ui.available_width() - 12.0);
                attention_panel(ui, &pal, &hardware::attention(inventory));
                ui.add_space(4.0);
                summary_cards(ui, &pal, inventory);
                ui.add_space(4.0);
                if storage_panel(ui, &pal, inventory, cx.elevated) {
                    elevate = true;
                }
                if !inventory.batteries.is_empty() {
                    ui.add_space(4.0);
                    battery_panel(ui, &pal, inventory);
                }
                if !inventory.network.is_empty() {
                    ui.add_space(4.0);
                    network_panel(ui, &pal, inventory);
                }
                ui.add_space(8.0);
            });
        if elevate {
            cx.request_elevation = true;
        }
    }
}

fn save_report(inventory: &Inventory, cx: &mut Cx) {
    let Some(file) = rfd::FileDialog::new()
        .set_title("Save the system report")
        .set_file_name("zest-system-report.txt")
        .add_filter("Text", &["txt"])
        .add_filter("JSON", &["json"])
        .save_file()
    else {
        return;
    };
    let is_json = file
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
    let body = if is_json {
        hardware::to_json(inventory)
    } else {
        hardware::to_text(inventory)
    };
    match std::fs::write(&file, body) {
        Ok(()) => cx
            .toasts
            .good(format!("Report saved to {}.", file.display())),
        Err(e) => cx
            .toasts
            .error(format!("The report could not be saved: {e}")),
    }
}

fn display_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(DISPLAY.into()))
}

/// What needs attention, or a short all-clear.
fn attention_panel(ui: &mut Ui, pal: &Palette, findings: &[Attention]) {
    if findings.is_empty() {
        widgets::banner(
            ui,
            pal,
            Tone::Good,
            icon::CHECK_CIRCLE,
            "Nothing needs attention. Drives, battery and free space look fine.",
            None,
        );
        return;
    }
    let worst = findings
        .iter()
        .map(|f| f.level)
        .max()
        .unwrap_or(Level::Note);
    let edge = level_tone(worst).color(pal);
    egui::Frame::new()
        .fill(pal.tint(edge, 0.08))
        .stroke(egui::Stroke::new(1.0, pal.tint(edge, 0.38)))
        .corner_radius(egui::CornerRadius::same(crate::theme::RADIUS_PANEL))
        .inner_margin(Margin::symmetric(18, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            for finding in findings {
                let color = level_tone(finding.level).color(pal);
                let glyph = match finding.level {
                    Level::Critical => icon::WARNING_OCTAGON,
                    Level::Warning => icon::WARNING,
                    Level::Note => icon::INFO,
                };
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.label(text::icon(glyph, 20.0).color(color));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(text::strong(&finding.title));
                        ui.add(egui::Label::new(text::muted(&finding.detail, pal)).wrap());
                    });
                });
            }
        });
}

fn level_tone(level: Level) -> Tone {
    match level {
        Level::Critical => Tone::Danger,
        Level::Warning => Tone::Warn,
        Level::Note => Tone::Info,
    }
}

/// One summary card: an icon and title, a headline and labelled facts.
fn card(
    ui: &mut Ui,
    pal: &Palette,
    width: f32,
    glyph: &str,
    title: &str,
    headline: &str,
    add: impl FnOnce(&mut Ui),
) {
    widgets::panel(pal).show(ui, |ui| {
        // The panel adds 18 points of padding and a hairline on each side.
        ui.set_width(width - 38.0);
        ui.set_min_height(CARD_HEIGHT);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(text::icon(glyph, 18.0).color(pal.accent_text));
                ui.label(text::muted(title, pal));
            });
            ui.add_space(4.0);
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
            widgets::paint_text(ui, rect, headline, display_font(21.0), pal.text, Align::Min);
            response.on_hover_text(headline);
            ui.add_space(8.0);
            add(ui);
        });
    });
}

/// A labelled fact inside a summary card.
fn fact_row(ui: &mut Ui, pal: &Palette, label: &str, value: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 25.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let label_rect = Rect::from_min_max(rect.min, pos2(rect.left() + LABEL_WIDTH, rect.bottom()));
    let value_rect = Rect::from_min_max(pos2(label_rect.right() + 8.0, rect.top()), rect.max);
    widgets::paint_text(
        ui,
        label_rect,
        label,
        text::body_font(13.0),
        pal.text_muted,
        Align::Min,
    );
    widgets::paint_text(
        ui,
        value_rect,
        value,
        text::body_font(14.0),
        pal.text,
        Align::Min,
    );
    response.on_hover_text(value);
}

fn on_off(value: bool) -> &'static str {
    if value { "On" } else { "Off" }
}

fn summary_cards(ui: &mut Ui, pal: &Palette, inventory: &Inventory) {
    let gap = 10.0;
    let width = ((ui.available_width() - gap) / 2.0).floor();
    let system = &inventory.system;

    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let computer = match (&system.manufacturer, &system.model) {
            (Some(maker), Some(model)) => format!("{maker} {model}"),
            (None, Some(model)) => model.clone(),
            (Some(maker), None) => maker.clone(),
            (None, None) => {
                let kind = system.chassis.label();
                kind[..1].to_uppercase() + &kind[1..]
            }
        };
        card(
            ui,
            pal,
            width,
            icon::DESKTOP_TOWER,
            "This PC",
            &computer,
            |ui| {
                let mut windows: Vec<String> = Vec::new();
                windows.extend(system.os_name.clone());
                windows.extend(system.os_release.clone());
                fact_row(ui, pal, "Windows", &windows.join(" "));
                let mut build: Vec<String> = Vec::new();
                build.extend(system.os_build.clone());
                build.extend(system.os_architecture.clone());
                fact_row(ui, pal, "Build", &build.join(", "));
                let mut firmware = vec![system.firmware_kind.label().to_owned()];
                firmware.extend(system.firmware_version.clone());
                if let Some(date) = system.firmware_date {
                    firmware.push(date.to_string());
                }
                fact_row(ui, pal, "Firmware", &firmware.join(", "));
                let mut security = Vec::new();
                if let Some(secure_boot) = system.secure_boot {
                    security.push(format!(
                        "Secure Boot {}",
                        on_off(secure_boot).to_lowercase()
                    ));
                }
                security.push(match &system.tpm_version {
                    Some(version) => format!("TPM {version}"),
                    None => "no TPM".to_owned(),
                });
                fact_row(ui, pal, "Security", &security.join(", "));
                fact_row(
                    ui,
                    pal,
                    "Running for",
                    &format_uptime(system.uptime_seconds),
                );
            },
        );

        let processor = inventory.processors.first();
        let headline = match (processor, inventory.processors.len()) {
            (Some(processor), 1) => processor.name.clone(),
            (Some(processor), count) => format!("{count} × {}", processor.name),
            (None, _) => "Not reported".to_owned(),
        };
        card(ui, pal, width, icon::CPU, "Processor", &headline, |ui| {
            let Some(processor) = processor else {
                return;
            };
            let mut cores = Vec::new();
            if let Some(count) = processor.cores {
                cores.push(format!("{count} cores"));
            }
            if let Some(count) = processor.threads {
                cores.push(format!("{count} threads"));
            }
            fact_row(ui, pal, "Cores", &cores.join(", "));
            if let Some(mhz) = processor.base_mhz {
                fact_row(ui, pal, "Base speed", &format_frequency(mhz));
            }
            let mut cache = Vec::new();
            if let Some(kb) = processor.l2_cache_kb {
                cache.push(format!("{} L2", format_capacity(u64::from(kb) * 1024)));
            }
            if let Some(kb) = processor.l3_cache_kb {
                cache.push(format!("{} L3", format_capacity(u64::from(kb) * 1024)));
            }
            if !cache.is_empty() {
                fact_row(ui, pal, "Cache", &cache.join(", "));
            }
            if let Some(architecture) = &processor.architecture {
                fact_row(ui, pal, "Architecture", architecture);
            }
            if let Some(enabled) = processor.virtualization {
                fact_row(ui, pal, "Virtualisation", on_off(enabled));
            }
        });
    });
    ui.add_space(gap - 8.0);

    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        let memory = &inventory.memory;
        card(
            ui,
            pal,
            width,
            icon::MEMORY,
            "Memory",
            &format_capacity(memory.total_bytes()),
            |ui| {
                let used = memory.usable_bytes.saturating_sub(memory.available_bytes);
                widgets::usage_bar(
                    ui,
                    pal,
                    &[(memory.used_fraction(), Segment::Solid)],
                    12.0,
                    None,
                );
                ui.add_space(4.0);
                fact_row(
                    ui,
                    pal,
                    "Right now",
                    &format!(
                        "{} in use, {} free",
                        format_bytes(used),
                        format_bytes(memory.available_bytes)
                    ),
                );
                if let Some(first) = memory.modules.first() {
                    let mut kind: Vec<String> = Vec::new();
                    kind.extend(first.kind.clone());
                    kind.extend(first.form.clone());
                    let mut line = kind.join(" ");
                    if let Some(mhz) = first.speed_mhz {
                        if !line.is_empty() {
                            line.push_str(", ");
                        }
                        line.push_str(&format!("{mhz} MHz"));
                    }
                    if !line.is_empty() {
                        fact_row(ui, pal, "Type", &line);
                    }
                    fact_row(ui, pal, "Modules", &module_summary(inventory));
                }
                if let (Some(slots), Some(free)) = (memory.slots, memory.free_slots()) {
                    let line = if free == 0 {
                        format!("All {slots} in use")
                    } else {
                        format!("{} of {slots} in use, {free} free", slots - free)
                    };
                    fact_row(ui, pal, "Slots", &line);
                }
            },
        );

        let adapter = inventory.graphics.first();
        let headline = adapter.map_or("Not reported", |a| a.name.as_str());
        card(
            ui,
            pal,
            width,
            icon::GRAPHICS_CARD,
            "Graphics",
            headline,
            |ui| {
                if let Some(adapter) = adapter {
                    // Integrated graphics borrow main memory; a small figure of
                    // their own would only mislead.
                    if let Some(bytes) = adapter
                        .dedicated_memory_bytes
                        .filter(|&bytes| bytes >= 256 << 20)
                    {
                        fact_row(ui, pal, "Memory", &format_capacity(bytes));
                    }
                    if let Some(version) = &adapter.driver_version {
                        let line = match adapter.driver_date {
                            Some(date) => format!("{version}, {date}"),
                            None => version.clone(),
                        };
                        fact_row(ui, pal, "Driver", &line);
                    }
                }
                for other in inventory.graphics.iter().skip(1).take(1) {
                    fact_row(ui, pal, "Also", &other.name);
                }
                for (index, display) in inventory.displays.iter().take(2).enumerate() {
                    let mut line = format!("{} × {}", display.width, display.height);
                    if let Some(hz) = display.refresh_hz {
                        line.push_str(&format!(" at {hz:.0} Hz"));
                    }
                    if !display.built_in {
                        line = format!("{}, {line}", display.name);
                    }
                    let label = if index == 0 { "Display" } else { "" };
                    fact_row(ui, pal, label, &line);
                }
                if inventory.displays.len() > 2 {
                    fact_row(
                        ui,
                        pal,
                        "",
                        &format!("and {} more", inventory.displays.len() - 2),
                    );
                }
            },
        );
    });
}

/// Describes the installed modules, e.g. `2 × 16 GB Kestrel KS5600S16`.
fn module_summary(inventory: &Inventory) -> String {
    let modules = &inventory.memory.modules;
    let Some(first) = modules.first() else {
        return String::new();
    };
    let all_alike = modules
        .iter()
        .all(|m| m.capacity_bytes == first.capacity_bytes && m.part_number == first.part_number);
    if all_alike {
        let mut line = format!(
            "{} × {}",
            modules.len(),
            format_capacity(first.capacity_bytes)
        );
        for part in [&first.manufacturer, &first.part_number]
            .into_iter()
            .flatten()
        {
            line.push(' ');
            line.push_str(part);
        }
        line
    } else {
        modules
            .iter()
            .map(|m| format_capacity(m.capacity_bytes))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

/// A small label above a value, for the figures of a drive or battery.
fn figure(ui: &mut Ui, pal: &Palette, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(text::small(label, pal));
        ui.label(text::strong(value));
    });
}

fn health_tone(health: Health) -> Tone {
    match health {
        Health::Healthy => Tone::Good,
        Health::Warning => Tone::Warn,
        Health::Unhealthy => Tone::Danger,
        Health::Unknown => Tone::Neutral,
    }
}

fn section_title(ui: &mut Ui, pal: &Palette, glyph: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(text::icon(glyph, 18.0).color(pal.accent_text));
        ui.label(text::section(title));
    });
}

/// One volume: name, how full it is, and the figures.
fn volume_row(ui: &mut Ui, pal: &Palette, volume: &Volume) {
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    if !ui.is_rect_visible(row) {
        return;
    }
    let columns = Columns(vec![Col::Fixed(190.0), Col::Flex(1.0), Col::Fixed(196.0)]);
    let cells = columns.cells(row, 14.0);
    widgets::paint_text(
        ui,
        cells[0],
        &volume.display_name(),
        text::body_font(14.0),
        pal.text,
        Align::Min,
    );
    // Nearly full volumes change colour so they stand out in the list.
    let segment = if volume.free_fraction() < 0.05 {
        Segment::Flat(pal.danger)
    } else if volume.free_fraction() < 0.10 {
        Segment::Flat(pal.warn)
    } else {
        Segment::Solid
    };
    widgets::in_cell(ui, cells[1], |ui| {
        widgets::usage_bar(ui, pal, &[(volume.used_fraction(), segment)], 12.0, None);
    });
    widgets::paint_text(
        ui,
        cells[2],
        &format!(
            "{} free of {}",
            format_bytes(volume.free_bytes),
            format_bytes(volume.total_bytes)
        ),
        text::body_font(13.0),
        pal.text_muted,
        Align::Max,
    );
}

fn drive_block(ui: &mut Ui, pal: &Palette, drive: &PhysicalDrive, elevated: bool) -> bool {
    let mut elevate = false;
    ui.horizontal(|ui| {
        ui.label(text::strong(&drive.model));
        ui.label(text::muted(hardware::drive_summary(drive), pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            widgets::pill(ui, pal, drive.health.label(), health_tone(drive.health));
        });
    });

    let counters = &drive.counters;
    let mut figures: Vec<(&str, String)> = Vec::new();
    if let Some(celsius) = counters.temperature_c {
        figures.push(("Temperature", format!("{celsius} °C")));
    }
    if let Some(wear) = counters.wear_percent {
        figures.push(("Rated life used", format!("{wear}%")));
    }
    if let Some(spare) = counters.spare_percent {
        figures.push(("Spare blocks", format!("{spare}%")));
    }
    if let Some(hours) = counters.power_on_hours {
        figures.push(("Powered on", format!("{} hours", format_count(hours))));
    }
    if let Some(written) = counters.bytes_written {
        figures.push(("Written", format_bytes(written)));
    }
    if counters.media_errors.is_some()
        || counters.read_errors.is_some()
        || counters.write_errors.is_some()
    {
        let errors = counters.media_errors.unwrap_or(0)
            + counters.read_errors.unwrap_or(0)
            + counters.write_errors.unwrap_or(0);
        figures.push(("Errors", format_count(errors)));
    }
    if !figures.is_empty() {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(30.0, 6.0);
            for (label, value) in &figures {
                figure(ui, pal, label, value);
            }
        });
    }
    if drive.counters_need_admin {
        ui.horizontal(|ui| {
            ui.label(text::small(
                "Windows shows this drive's wear and error counters to administrators only.",
                pal,
            ));
            if !elevated && widgets::quiet_button(ui, pal, "Restart as administrator").clicked() {
                elevate = true;
            }
        });
    }
    ui.add_space(2.0);
    for volume in &drive.volumes {
        volume_row(ui, pal, volume);
    }
    if drive.hidden_partitions > 0 {
        let count = if drive.hidden_partitions == 1 {
            "1 more partition".to_owned()
        } else {
            format!("{} more partitions", drive.hidden_partitions)
        };
        ui.label(text::small(
            format!("{count} without a drive letter, used for starting and repairing Windows."),
            pal,
        ));
    }
    elevate
}

/// Drives with their health, counters and volumes. Returns true when the
/// user asked to restart as administrator.
fn storage_panel(ui: &mut Ui, pal: &Palette, inventory: &Inventory, elevated: bool) -> bool {
    let mut elevate = false;
    widgets::panel(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_title(ui, pal, icon::HARD_DRIVES, "Storage");
        if inventory.drives.is_empty() && inventory.other_volumes.is_empty() {
            ui.label(text::muted("Windows did not report any drives.", pal));
        }
        for (index, drive) in inventory.drives.iter().enumerate() {
            if index > 0 {
                ui.add_space(4.0);
                widgets::hairline(ui, pal);
            }
            ui.add_space(4.0);
            if drive_block(ui, pal, drive, elevated) {
                elevate = true;
            }
        }
        if !inventory.other_volumes.is_empty() {
            if !inventory.drives.is_empty() {
                ui.add_space(4.0);
                widgets::hairline(ui, pal);
                ui.add_space(4.0);
            }
            ui.label(text::strong("Other volumes"));
            for volume in &inventory.other_volumes {
                volume_row(ui, pal, volume);
            }
        }
    });
    elevate
}

fn battery_block(ui: &mut Ui, pal: &Palette, battery: &Battery) {
    ui.horizontal(|ui| {
        ui.label(text::strong(hardware::battery_summary(battery)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(wear) = battery.wear_percent() {
                let (label, tone) = if wear >= 45.0 {
                    ("Worn", Tone::Warn)
                } else if wear >= 25.0 {
                    ("Ageing", Tone::Info)
                } else {
                    ("Good", Tone::Good)
                };
                widgets::pill(ui, pal, label, tone);
            }
        });
    });
    if let (Some(health), Some(full), Some(design)) = (
        battery.health_percent(),
        battery.full_charge_mwh,
        battery.design_mwh,
    ) {
        ui.add_space(2.0);
        widgets::usage_bar(ui, pal, &[(health / 100.0, Segment::Solid)], 12.0, None);
        ui.label(text::muted(
            format!(
                "A full charge holds {} of the {} it held when new.",
                format_energy(full),
                format_energy(design)
            ),
            pal,
        ));
    }
    let mut figures: Vec<(&str, String)> = Vec::new();
    if let Some(health) = battery.health_percent() {
        figures.push(("Capacity left", format!("{health:.0}%")));
    }
    if let Some(cycles) = battery.cycle_count {
        figures.push(("Charge cycles", format_count(u64::from(cycles))));
    }
    if let Some(rate) = battery.rate_mw.filter(|&rate| rate != 0) {
        let watts = f64::from(rate.unsigned_abs()) / 1000.0;
        figures.push(if rate > 0 {
            ("Charging at", format!("{watts:.1} W"))
        } else {
            ("Drawing", format!("{watts:.1} W"))
        });
    }
    if let Some(millivolts) = battery.voltage_mv {
        figures.push((
            "Voltage",
            format!("{:.1} V", f64::from(millivolts) / 1000.0),
        ));
    }
    if !figures.is_empty() {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(30.0, 6.0);
            for (label, value) in &figures {
                figure(ui, pal, label, value);
            }
        });
    }
}

fn battery_panel(ui: &mut Ui, pal: &Palette, inventory: &Inventory) {
    widgets::panel(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_title(ui, pal, icon::BATTERY_HIGH, "Battery");
        for (index, battery) in inventory.batteries.iter().enumerate() {
            if index > 0 {
                ui.add_space(4.0);
                widgets::hairline(ui, pal);
            }
            ui.add_space(4.0);
            battery_block(ui, pal, battery);
        }
    });
}

fn network_panel(ui: &mut Ui, pal: &Palette, inventory: &Inventory) {
    widgets::panel(pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        section_title(ui, pal, icon::NETWORK, "Network adapters");
        ui.add_space(2.0);
        for adapter in &inventory.network {
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
            if !ui.is_rect_visible(row) {
                continue;
            }
            let columns = Columns(vec![Col::Flex(1.0), Col::Fixed(110.0), Col::Fixed(196.0)]);
            let cells = columns.cells(row, 14.0);
            widgets::paint_text(
                ui,
                cells[0],
                &adapter.name,
                text::body_font(14.0),
                pal.text,
                Align::Min,
            );
            widgets::paint_text(
                ui,
                cells[1],
                adapter.kind.label(),
                text::body_font(13.0),
                pal.text_muted,
                Align::Min,
            );
            let (state, color) = match (adapter.connected, adapter.link_bps) {
                (true, Some(bps)) => (format!("Connected at {}", format_link_speed(bps)), pal.text),
                (true, None) => ("Connected".to_owned(), pal.text),
                (false, _) => ("Not connected".to_owned(), pal.text_faint),
            };
            widgets::paint_text(
                ui,
                cells[2],
                &state,
                text::body_font(13.0),
                color,
                Align::Max,
            );
        }
    });
}
