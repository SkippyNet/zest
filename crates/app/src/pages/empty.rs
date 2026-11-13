//! Empty folders: find and remove folders that hold nothing.

use super::Cx;
use crate::widgets;
use eframe::egui::{self, Ui};
use egui_phosphor::regular as icon;
use zest_engine::Settings;

/// State of the Empty folders page.
#[derive(Default)]
pub struct EmptyPage {}

impl EmptyPage {
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
            "Empty folders",
            "Find folders with nothing in them.",
        );
        widgets::empty_state(ui, &pal, icon::FOLDER_DASHED, "Not built yet", "", None);
    }
}
