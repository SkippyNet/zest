//! Windows privacy: switches for what Windows collects, suggests and advertises.

use super::Cx;
use crate::theme::{Palette, mix, text};
use crate::widgets::{self, Confirm, Tone};
use eframe::egui::{self, Align, Frame, Layout, Margin, ScrollArea, Ui, vec2};
use egui_phosphor::regular as icon;
use zest_engine::Settings;
use zest_engine::core::fmt::plural;
use zest_engine::tweaks::privacy::{
    CATALOG, Category, ItemOutcome, Mechanism, Outcome, Privacy, Scope, State, Tweak, TweakStatus,
};

/// A change waiting for the user's confirmation.
enum Pending {
    /// Switch on the recommended settings that are still off.
    Recommended(Vec<&'static str>),
    /// Put everything that was changed back to how Windows ships it.
    Defaults(Vec<&'static str>),
    /// Switch on one setting that carries a caution.
    Single(&'static str),
}

/// What a click on a row asks for.
enum RowAction {
    On(&'static str),
    Off(&'static str),
}

/// State of the Windows privacy page.
#[derive(Default)]
pub struct PrivacyPage {
    list: Option<Vec<TweakStatus>>,
    /// Showing made-up states; nothing is read from or written to the registry.
    demo: bool,
    pending: Option<Pending>,
}

impl PrivacyPage {
    /// True while a background job of this page is running.
    pub fn is_busy(&self) -> bool {
        false
    }

    /// Reads the current state of every setting (used by screenshot mode).
    pub fn start_scan(&mut self, _ctx: &egui::Context, _settings: &Settings) {
        self.reload();
    }

    /// Fills the page with made-up data for documentation screenshots.
    pub fn load_demo(&mut self) {
        const PRIVATE: [&str; 9] = [
            "diagnostic-data",
            "tailored-experiences",
            "feedback-requests",
            "advertising-id",
            "suggestions-and-tips",
            "third-party-suggestions",
            "activity-history",
            "search-highlights",
            "recall-snapshots",
        ];
        const ABSENT: [&str; 2] = ["news-and-interests", "copilot-panel"];
        self.demo = true;
        self.list = Some(
            CATALOG
                .iter()
                .map(|tweak| TweakStatus {
                    tweak,
                    state: if ABSENT.contains(&tweak.id) {
                        State::NotApplicable
                    } else if PRIVATE.contains(&tweak.id) {
                        State::Private
                    } else {
                        State::Default
                    },
                    needs_admin: false,
                    edition_caveat: false,
                })
                .collect(),
        );
    }

    /// Reads every setting again. Read-only.
    fn reload(&mut self) {
        if !self.demo {
            self.list = Some(Privacy::system().list());
        }
    }

    /// Switches the given settings on (`private`) or back to the default.
    fn change(&mut self, cx: &mut Cx, ids: &[&'static str], private: bool) {
        if self.demo {
            for status in self.list.iter_mut().flatten() {
                if ids.contains(&status.tweak.id) && status.state != State::NotApplicable {
                    status.state = if private {
                        State::Private
                    } else {
                        State::Default
                    };
                }
            }
            return;
        }
        let privacy = Privacy::system();
        let result = if private {
            privacy.apply(ids)
        } else {
            privacy.revert(ids)
        };
        match result {
            Ok(outcome) => report(cx, &outcome, private),
            Err(e) => cx.toasts.error(format!(
                "Nothing was changed, because the backup could not be saved: {e}"
            )),
        }
        self.list = Some(privacy.list());
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        if self.list.is_none() {
            self.reload();
        }
        let list = self.list.clone().unwrap_or_default();

        let applicable: Vec<&TweakStatus> = list
            .iter()
            .filter(|s| s.state != State::NotApplicable)
            .collect();
        let private = applicable
            .iter()
            .filter(|s| s.state == State::Private)
            .count();
        let recommended_off = applicable
            .iter()
            .filter(|s| s.tweak.recommended && s.state != State::Private)
            .count();
        // What the two buttons can do with the rights at hand.
        let can_recommend: Vec<&'static str> = applicable
            .iter()
            .filter(|s| s.tweak.recommended && s.state != State::Private && !s.needs_admin)
            .map(|s| s.tweak.id)
            .collect();
        let can_restore: Vec<&'static str> = applicable
            .iter()
            .filter(|s| s.state != State::Default && !s.needs_admin)
            .map(|s| s.tweak.id)
            .collect();
        let locked = applicable.iter().filter(|s| s.needs_admin).count();
        let caveat = applicable.iter().any(|s| s.edition_caveat);

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                widgets::page_header(
                    ui,
                    &pal,
                    "Windows privacy",
                    "Decide what Windows collects, suggests and advertises. Every change is backed up first.",
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.add_space(2.0);
                ui.add_enabled_ui(!can_recommend.is_empty(), |ui| {
                    if widgets::primary_button(ui, &pal, "Apply recommended").clicked() {
                        self.pending = Some(Pending::Recommended(can_recommend.clone()));
                    }
                });
                ui.add_enabled_ui(!can_restore.is_empty(), |ui| {
                    if widgets::button(ui, &pal, "Restore Windows defaults").clicked() {
                        self.pending = Some(Pending::Defaults(can_restore.clone()));
                    }
                });
            });
        });

        widgets::panel(&pal)
            .inner_margin(Margin::symmetric(18, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(text::strong(format!(
                        "{private} of {} are private",
                        plural(applicable.len() as u64, "setting")
                    )));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let hint = match recommended_off {
                            0 => "Every recommended setting is on".to_owned(),
                            1 => "1 recommended setting is still off".to_owned(),
                            n => format!("{n} recommended settings are still off"),
                        };
                        ui.label(text::muted(hint, &pal));
                    });
                });
                ui.add_space(2.0);
                let share = private as f32 / applicable.len().max(1) as f32;
                widgets::progress_bar(ui, &pal, Some(share), 6.0);
            });
        ui.add_space(2.0);

        if locked > 0 {
            let message = if locked == 1 {
                "1 setting is a Windows policy. Windows lets only administrators change it."
                    .to_owned()
            } else {
                format!(
                    "{locked} settings are Windows policies. Windows lets only administrators change those."
                )
            };
            if widgets::banner(
                ui,
                &pal,
                Tone::Info,
                icon::SHIELD,
                &message,
                Some("Restart as administrator"),
            ) {
                cx.request_elevation = true;
            }
            ui.add_space(2.0);
        }

        let mut action: Option<RowAction> = None;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(ui.available_size() - vec2(2.0, 2.0));
            ScrollArea::vertical()
                .id_salt("privacy-settings")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for category in Category::ALL {
                        let rows: Vec<&TweakStatus> = list
                            .iter()
                            .filter(|s| s.tweak.category == category)
                            .collect();
                        if rows.is_empty() {
                            continue;
                        }
                        Frame::new()
                            .inner_margin(Margin {
                                left: 16,
                                right: 16,
                                top: 16,
                                bottom: 6,
                            })
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(text::section(category.label()));
                            });
                        for status in rows {
                            row(ui, &pal, status, &mut action);
                        }
                    }
                    Frame::new()
                        .inner_margin(Margin::symmetric(16, 14))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let mut foot = "Some changes show only after you sign out or restart. Windows marks settings that a policy controls as “managed by your organization”.".to_owned();
                            if caveat {
                                foot.push_str(" Microsoft documents policies for Windows Pro and above; Windows Home usually follows them, but that is not promised.");
                            }
                            ui.add(egui::Label::new(text::small(foot, &pal)).wrap());
                        });
                });
        });

        match action {
            Some(RowAction::On(id)) => {
                if Tweak::find(id).is_some_and(|t| !t.caution.is_empty()) {
                    self.pending = Some(Pending::Single(id));
                } else {
                    self.change(cx, &[id], true);
                }
            }
            Some(RowAction::Off(id)) => self.change(cx, &[id], false),
            None => {}
        }

        if let Some(pending) = &self.pending {
            let (title, body, label) = match pending {
                Pending::Recommended(ids) => (
                    "Apply the recommended settings?".to_owned(),
                    format!(
                        "{}\n\nNone of these takes a feature away, and each switch can be turned off again.",
                        bullet_list(ids)
                    ),
                    "Apply",
                ),
                Pending::Defaults(ids) => (
                    "Restore the Windows defaults?".to_owned(),
                    format!(
                        "These go back to how Windows ships them:\n\n{}",
                        bullet_list(ids)
                    ),
                    "Restore defaults",
                ),
                Pending::Single(id) => {
                    let tweak = Tweak::find(id);
                    (
                        format!("{}?", tweak.map_or("Switch this on", |t| t.title)),
                        tweak.map_or(String::new(), |t| t.caution.to_owned()),
                        "Switch on",
                    )
                }
            };
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "privacy-confirm",
                &title,
                &body,
                label,
                false,
            ) {
                Confirm::Yes => match self.pending.take() {
                    Some(Pending::Recommended(ids)) => self.change(cx, &ids, true),
                    Some(Pending::Defaults(ids)) => self.change(cx, &ids, false),
                    Some(Pending::Single(id)) => self.change(cx, &[id], true),
                    None => {}
                },
                Confirm::No => self.pending = None,
                Confirm::Pending => {}
            }
        }
    }
}

/// Titles of the given settings, one per line.
fn bullet_list(ids: &[&'static str]) -> String {
    const SHOWN: usize = 14;
    let mut lines: Vec<String> = ids
        .iter()
        .take(SHOWN)
        .filter_map(|id| Tweak::find(id))
        .map(|t| format!("•  {}", t.title))
        .collect();
    if ids.len() > SHOWN {
        lines.push(format!("•  and {} more", ids.len() - SHOWN));
    }
    lines.join("\n")
}

/// Tells the user how a run went.
fn report(cx: &mut Cx, outcome: &Outcome, private: bool) {
    let changed = outcome.changed() as u64;
    let locked = outcome.needs_admin() as u64;
    for item in &outcome.items {
        if let ItemOutcome::Failed(reason) = &item.outcome {
            cx.toasts
                .error(format!("“{}” was not changed: {reason}", item.title));
            break;
        }
    }
    if changed > 0 {
        let what = if private {
            "switched on"
        } else {
            "put back to the Windows default"
        };
        cx.toasts.good(format!(
            "{} {what}. A backup was saved.",
            plural(changed, "setting")
        ));
    }
    if locked > 0 {
        cx.toasts.error(format!(
            "{} left as {}: administrator rights are needed.",
            plural(locked, "setting"),
            if locked == 1 { "it was" } else { "they were" }
        ));
    }
}

/// Everything worth knowing about a setting that does not fit on its row.
fn details(status: &TweakStatus) -> String {
    let tweak = status.tweak;
    let mut lines: Vec<String> = Vec::new();
    if !tweak.note.is_empty() {
        lines.push(tweak.note.to_owned());
    }
    match status.state {
        State::Custom => lines.push(
            "Partly applied, or set to a value Zest does not use. Switch it on to apply it, or reset it to the Windows default.".to_owned(),
        ),
        State::NotApplicable => {
            lines.push("This version of Windows does not have this setting.".to_owned());
        }
        State::Private | State::Default => {}
    }
    if tweak.mechanism == Mechanism::Policy {
        lines.push("Set through a Windows policy.".to_owned());
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    let hive = match tweak.scope {
        Scope::User => "HKCU",
        Scope::Machine => "HKLM",
    };
    for write in tweak.writes {
        lines.push(format!(
            "{hive}\\{}\\{} = {}",
            write.key, write.name, write.private
        ));
    }
    lines.push(format!(
        "Source: {}",
        tweak.source.trim_start_matches("https://")
    ));
    lines.join("\n")
}

/// One setting: switch, title and explanation, state.
fn row(ui: &mut Ui, pal: &Palette, status: &TweakStatus, action: &mut Option<RowAction>) {
    let tweak = status.tweak;
    let applies = status.state != State::NotApplicable;
    let response = Frame::new()
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    vec2(50.0, 24.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        if applies {
                            ui.add_enabled_ui(!status.needs_admin, |ui| {
                                let mut on = status.state == State::Private;
                                if widgets::switch(ui, pal, &mut on, tweak.title) {
                                    *action = Some(if on {
                                        RowAction::On(tweak.id)
                                    } else {
                                        RowAction::Off(tweak.id)
                                    });
                                }
                            })
                            .response
                            .on_disabled_hover_text(
                                "A Windows policy. Restart as administrator to change it.",
                            );
                        } else {
                            ui.add_space(11.0);
                            ui.label(text::icon(icon::MINUS_CIRCLE, 18.0).color(pal.text_faint));
                        }
                    },
                );
                // The state goes on the right first, so the text wraps in
                // whatever width is left.
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    match status.state {
                        State::Private => {
                            widgets::pill(ui, pal, "Private", Tone::Good);
                        }
                        State::Custom => {
                            if !status.needs_admin
                                && widgets::quiet_button(ui, pal, "Reset")
                                    .on_hover_text("Put this setting back to the Windows default")
                                    .clicked()
                            {
                                *action = Some(RowAction::Off(tweak.id));
                            }
                            widgets::pill(ui, pal, "Custom", Tone::Warn);
                        }
                        State::NotApplicable => {
                            widgets::pill(ui, pal, "Not on this Windows", Tone::Neutral);
                        }
                        State::Default => {
                            if tweak.recommended {
                                widgets::pill(ui, pal, "Recommended", Tone::Accent);
                            }
                        }
                    }
                    if applies && status.needs_admin {
                        ui.label(text::icon(icon::SHIELD, 16.0).color(pal.text_faint))
                            .on_hover_text("Needs administrator rights");
                    }
                    ui.add_space(6.0);
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        let title = text::strong(tweak.title);
                        ui.label(if applies {
                            title
                        } else {
                            title.color(pal.text_muted)
                        });
                        ui.add_space(1.0);
                        ui.add(egui::Label::new(text::small(tweak.description, pal)).wrap());
                        if !applies && !tweak.note.is_empty() {
                            ui.add_space(1.0);
                            ui.add(egui::Label::new(text::small(tweak.note, pal)).wrap());
                        }
                    });
                });
            });
        })
        .response;
    response.on_hover_text(details(status));
    let edge = ui.max_rect();
    ui.painter().hline(
        (edge.left() + 16.0)..=(edge.right() - 16.0),
        ui.cursor().top(),
        egui::Stroke::new(1.0, mix(pal.surface, pal.line, 0.6)),
    );
}
