//! Backup: versioned file backups and full system images.

use super::Cx;
use crate::widgets;
use eframe::egui::{self, Ui};
use egui_phosphor::regular as icon;
use zest_engine::Settings;

/// State of the Backup page.
#[derive(Default)]
pub struct BackupPage {}

impl BackupPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        false
    }

    /// Starts the page's read-only analysis (used by screenshot mode).
    pub fn start_scan(&mut self, _ctx: &egui::Context, _settings: &Settings) {}

    /// Fills the page with made-up data for documentation screenshots.
    pub fn load_demo(&mut self) {}

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let pal = *cx.pal;
        widgets::page_header(
            ui,
            &pal,
            "Backup",
            "Keep copies of your files and a full image of this PC.",
        );
        widgets::empty_state(ui, &pal, icon::VAULT, "Not built yet", "", None);
    }
}
