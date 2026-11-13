//! Windows privacy: switches for what Windows collects, suggests and advertises.

use super::Cx;
use crate::widgets;
use eframe::egui::{self, Ui};
use egui_phosphor::regular as icon;
use zest_engine::Settings;

/// State of the Windows privacy page.
#[derive(Default)]
pub struct PrivacyPage {}

impl PrivacyPage {
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
            "Windows privacy",
            "Decide what Windows collects and shows you.",
        );
        widgets::empty_state(ui, &pal, icon::SHIELD_CHECK, "Not built yet", "", None);
    }
}
