//! System info: hardware inventory, drive health and battery condition.

use super::Cx;
use crate::widgets;
use eframe::egui::{self, Ui};
use egui_phosphor::regular as icon;
use zest_engine::Settings;

/// State of the System info page.
#[derive(Default)]
pub struct SysInfoPage {}

impl SysInfoPage {
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
            "System info",
            "What is inside this PC and how healthy it is.",
        );
        widgets::empty_state(ui, &pal, icon::DESKTOP_TOWER, "Not built yet", "", None);
    }
}
