//! Windows tools: the repair and maintenance commands built into Windows,
//! started from one place with their output shown as it arrives.

use super::Cx;
use crate::job::Job;
use crate::theme::{Palette, mix, text};
use crate::widgets::{self, Confirm, Tone};
use eframe::egui::{self, Align, Frame, Layout, Margin, ScrollArea, Ui, vec2};
use egui_phosphor::regular as icon;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zest_engine::Settings;
use zest_engine::core::Error;
use zest_engine::core::fmt::format_duration;
use zest_engine::tweaks::tools::{self, Group, TOOLS, Tool, ToolRun, Verdict};

/// What the running tool has written so far.
#[derive(Default)]
struct Live {
    text: String,
    fraction: Option<f32>,
}

/// State of the Windows tools page.
#[derive(Default)]
pub struct ToolsPage {
    job: Job<Result<ToolRun, String>>,
    running: Option<&'static Tool>,
    live: Arc<Mutex<Live>>,
    /// The tool that ran last and how it went.
    last: Option<(&'static Tool, Result<ToolRun, String>)>,
    confirm: Option<&'static Tool>,
    /// Showing a made-up run; nothing is started.
    demo: bool,
}

impl ToolsPage {
    /// True while a tool is running.
    pub fn is_busy(&self) -> bool {
        self.job.is_running()
    }

    /// Nothing to analyse here; tools only run when asked to.
    pub fn start_scan(&mut self, _ctx: &egui::Context, _settings: &Settings) {}

    /// Fills the page with made-up data for documentation screenshots.
    pub fn load_demo(&mut self) {
        self.demo = true;
        let tool = Tool::find("component-store-size");
        let output = "Deployment Image Servicing and Management tool\n\
            Version: 10.0.26100.1\n\n\
            Image Version: 10.0.26100.1\n\n\
            [==========================100.0%==========================]\n\n\
            Component Store (WinSxS) information:\n\n\
            Windows Explorer Reported Size of Component Store : 11.42 GB\n\n\
            Actual Size of Component Store : 10.87 GB\n\n\
            \u{20}   Shared with Windows : 6.12 GB\n\
            \u{20}   Backups and Disabled Features : 3.94 GB\n\
            \u{20}   Cache and Temporary Data : 812.44 MB\n\n\
            Date of Last Cleanup : 2026-09-28 03:14:07\n\n\
            Number of Reclaimable Packages : 4\n\
            Component Store Cleanup Recommended : Yes\n\n\
            The operation completed successfully.";
        self.last = tool.map(|tool| {
            (
                tool,
                Ok(ToolRun {
                    verdict: Verdict::Ok,
                    code: Some(0),
                    summary: "Finished.".to_owned(),
                    output: output.to_owned(),
                    elapsed: Duration::from_secs(96),
                }),
            )
        });
    }

    fn start(&mut self, ctx: &egui::Context, tool: &'static Tool) {
        if let Ok(mut live) = self.live.lock() {
            *live = Live::default();
        }
        self.last = None;
        self.running = Some(tool);
        let live = Arc::clone(&self.live);
        let repaint = ctx.clone();
        self.job.start(ctx, "windows-tool", move |progress| {
            tools::run(tool, &progress, &mut |log| {
                if let Ok(mut live) = live.lock() {
                    live.text = log.text();
                    live.fraction = log.fraction();
                }
                repaint.request_repaint();
            })
            .map_err(|e| e.to_string())
        });
    }

    fn open(&mut self, cx: &mut Cx, tool: &'static Tool) {
        if self.demo {
            return;
        }
        match tools::open(tool) {
            Ok(()) | Err(Error::Cancelled) => {}
            Err(e) => cx.toasts.error(e.to_string()),
        }
    }

    /// Draws the page.
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let ctx = ui.ctx().clone();
        let pal = *cx.pal;
        let elevated = cx.elevated || self.demo;

        if let Some(result) = self.job.poll(&ctx)
            && let Some(tool) = self.running.take()
        {
            let outcome = result.and_then(|r| r);
            match &outcome {
                Ok(run) => {
                    let message = format!("{}: {}", tool.title, finish_line(tool, run));
                    match run.verdict {
                        Verdict::Ok => cx.toasts.good(message),
                        Verdict::RestartNeeded | Verdict::Attention | Verdict::Cancelled => {
                            cx.toasts.info(message);
                        }
                        Verdict::NeedsAdmin | Verdict::Failed => cx.toasts.error(message),
                    }
                }
                Err(e) => cx
                    .toasts
                    .error(format!("{} could not be started: {e}", tool.title)),
            }
            self.last = Some((tool, outcome));
        }

        widgets::page_header(
            ui,
            &pal,
            "Windows tools",
            "Repair and maintenance tools that come with Windows. Zest starts them and shows what they report.",
        );

        if !elevated {
            if widgets::banner(
                ui,
                &pal,
                Tone::Info,
                icon::SHIELD,
                "Most of the repair tools only run with administrator rights.",
                Some("Restart as administrator"),
            ) {
                cx.request_elevation = true;
            }
            ui.add_space(2.0);
        }

        let show_output = self.running.is_some() || self.last.is_some();
        let gap = 8.0;
        let output_height = if show_output {
            (ui.available_height() * 0.44).clamp(200.0, 340.0)
        } else {
            0.0
        };
        let list_height = (ui.available_height() - output_height - gap).max(160.0);

        let busy = self.is_busy();
        let mut run: Option<&'static Tool> = None;
        let mut open: Option<&'static Tool> = None;
        widgets::list_panel(&pal).show(ui, |ui| {
            ui.set_min_size(vec2(ui.available_width() - 2.0, list_height - 2.0));
            ui.set_max_height(list_height - 2.0);
            ScrollArea::vertical()
                .id_salt("windows-tools")
                .auto_shrink([false, false])
                .max_height(list_height - 2.0)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for group in Group::ALL {
                        let entries: Vec<&'static Tool> = TOOLS
                            .iter()
                            .filter(|t| t.group == group && (self.demo || t.is_available()))
                            .collect();
                        if entries.is_empty() {
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
                                ui.label(text::section(group.label()));
                            });
                        for tool in entries {
                            let is_running = self.running.is_some_and(|t| t.id == tool.id);
                            match tool_row(ui, &pal, tool, elevated, busy, is_running) {
                                Some(Clicked::Run) => run = Some(tool),
                                Some(Clicked::Open) => open = Some(tool),
                                None => {}
                            }
                        }
                    }
                    ui.add_space(8.0);
                });
        });

        if show_output {
            ui.add_space(gap - 2.0);
            self.output_panel(ui, &pal, output_height);
        }

        if let Some(tool) = open {
            self.open(cx, tool);
        }
        if let Some(tool) = run
            && !self.demo
        {
            if tool.changes_system || tool.needs_restart {
                self.confirm = Some(tool);
            } else {
                self.start(&ctx, tool);
            }
        }
        if let Some(tool) = self.confirm {
            let mut body = format!("{}\n\n{}", tool.description, tool.changes);
            if tool.needs_restart {
                body.push_str(" The PC has to be restarted afterwards.");
            }
            if !tool.duration.is_empty() {
                body.push_str(&format!(
                    "\n\nUsually takes {}.",
                    lower_first(tool.duration)
                ));
            }
            match widgets::confirm_dialog(
                &ctx,
                &pal,
                "tools-run",
                &format!("Run “{}”?", tool.title),
                &body,
                "Run",
                false,
            ) {
                Confirm::Yes => {
                    self.confirm = None;
                    self.start(&ctx, tool);
                }
                Confirm::No => self.confirm = None,
                Confirm::Pending => {}
            }
        }
    }

    /// The log of the running or last tool.
    fn output_panel(&mut self, ui: &mut Ui, pal: &Palette, height: f32) {
        let (text, fraction) = match self.live.lock() {
            Ok(live) => (live.text.clone(), live.fraction),
            Err(_) => (String::new(), None),
        };
        widgets::panel(pal)
            .inner_margin(Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_min_size(vec2(ui.available_width(), height - 26.0));
                ui.set_max_height(height - 26.0);
                let mut cancel = false;
                let mut copy: Option<String> = None;
                let shown: String;
                if let Some(tool) = self.running {
                    ui.horizontal(|ui| {
                        widgets::spinner(ui, pal, 18.0);
                        ui.label(text::strong(tool.title));
                        ui.label(text::muted(
                            format!("running for {}", format_duration(self.job.elapsed())),
                            pal,
                        ));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::button(ui, pal, "Stop").clicked() {
                                cancel = true;
                            }
                        });
                    });
                    widgets::progress_bar(ui, pal, fraction, 4.0);
                    shown = if text.is_empty() {
                        "Waiting for the tool to report…".to_owned()
                    } else {
                        text
                    };
                } else if let Some((tool, outcome)) = &self.last {
                    let (tone, label, line, output) = match outcome {
                        Ok(run) => {
                            let (tone, label) = verdict_badge(run.verdict);
                            (
                                tone,
                                label,
                                // The badge already says "Finished".
                                format!(
                                    "{} Took {}.",
                                    finish_line(tool, run).trim_start_matches("Finished."),
                                    format_duration(run.elapsed)
                                )
                                .trim()
                                .to_owned(),
                                run.output.clone(),
                            )
                        }
                        Err(e) => (Tone::Danger, "Not started", e.clone(), String::new()),
                    };
                    ui.horizontal(|ui| {
                        ui.label(text::strong(tool.title));
                        widgets::pill(ui, pal, label, tone);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if !output.is_empty()
                                && widgets::icon_text_button(ui, pal, icon::COPY, "Copy output")
                                    .clicked()
                            {
                                copy = Some(output.clone());
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(text::muted(line, pal)).truncate());
                            });
                        });
                    });
                    widgets::hairline(ui, pal);
                    shown = if output.is_empty() {
                        "The tool wrote no output.".to_owned()
                    } else {
                        output
                    };
                } else {
                    return;
                }
                ui.add_space(2.0);
                ScrollArea::vertical()
                    .id_salt("tool-output")
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.running.is_some())
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(shown).monospace().size(12.5).color(mix(
                                    pal.text,
                                    pal.text_muted,
                                    0.45,
                                )),
                            )
                            .wrap(),
                        );
                    });
                if cancel {
                    self.job.cancel();
                }
                if let Some(output) = copy {
                    ui.ctx().copy_text(output);
                }
            });
    }
}

/// The sentence that sums a finished run up.
fn finish_line(tool: &Tool, run: &ToolRun) -> String {
    if tool.needs_restart && run.verdict == Verdict::Ok {
        "Finished. Restart the PC for it to take effect.".to_owned()
    } else {
        run.summary.clone()
    }
}

fn verdict_badge(verdict: Verdict) -> (Tone, &'static str) {
    match verdict {
        Verdict::Ok => (Tone::Good, "Finished"),
        Verdict::RestartNeeded => (Tone::Warn, "Restart needed"),
        Verdict::Attention => (Tone::Warn, "Needs a look"),
        Verdict::NeedsAdmin => (Tone::Danger, "Administrator needed"),
        Verdict::Failed => (Tone::Danger, "Failed"),
        Verdict::Cancelled => (Tone::Neutral, "Stopped"),
    }
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

enum Clicked {
    Run,
    Open,
}

/// One tool: what it does, what it changes, and the button that starts it.
fn tool_row(
    ui: &mut Ui,
    pal: &Palette,
    tool: &'static Tool,
    elevated: bool,
    busy: bool,
    is_running: bool,
) -> Option<Clicked> {
    let mut clicked = None;
    Frame::new()
        .inner_margin(Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    // A fixed cell keeps the buttons in one column.
                    ui.allocate_ui_with_layout(
                        vec2(92.0, 34.0),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            if !tool.is_command() {
                                if widgets::icon_text_button(
                                    ui,
                                    pal,
                                    icon::ARROW_SQUARE_OUT,
                                    "Open",
                                )
                                .clicked()
                                {
                                    clicked = Some(Clicked::Open);
                                }
                            } else if is_running {
                                widgets::spinner(ui, pal, 20.0);
                            } else {
                                let locked = tool.needs_admin && !elevated;
                                let reason = if locked {
                                    "Needs administrator rights"
                                } else {
                                    "Another tool is still running"
                                };
                                ui.add_enabled_ui(!locked && !busy, |ui| {
                                    if widgets::button(ui, pal, "Run").clicked() {
                                        clicked = Some(Clicked::Run);
                                    }
                                })
                                .response
                                .on_disabled_hover_text(reason);
                            }
                        },
                    );
                    if tool.needs_restart {
                        widgets::pill(ui, pal, "Restart needed", Tone::Warn);
                    }
                    if tool.needs_admin && !elevated {
                        widgets::pill(ui, pal, "Administrator", Tone::Info);
                    }
                    if !tool.duration.is_empty() {
                        widgets::pill(ui, pal, tool.duration, Tone::Neutral);
                    }
                    ui.add_space(6.0);
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.label(text::strong(tool.title))
                            .on_hover_text(tool.command_line());
                        ui.add_space(1.0);
                        ui.add(egui::Label::new(text::small(tool.description, pal)).wrap());
                        if tool.is_command() {
                            ui.add_space(4.0);
                            // One label with two colours keeps the line as
                            // low as plain text.
                            let (glyph, color) = if tool.changes_system {
                                (icon::PENCIL_SIMPLE_LINE, pal.warn)
                            } else {
                                (icon::CHECK, pal.fresh)
                            };
                            let mut line = egui::text::LayoutJob::default();
                            line.append(
                                glyph,
                                0.0,
                                egui::TextFormat::simple(text::body_font(13.0), color),
                            );
                            line.append(
                                tool.changes,
                                6.0,
                                egui::TextFormat::simple(
                                    text::body_font(12.0),
                                    mix(pal.text, pal.text_muted, 0.6),
                                ),
                            );
                            ui.add(egui::Label::new(line).wrap());
                        }
                    });
                });
            });
        });
    let edge = ui.max_rect();
    ui.painter().hline(
        (edge.left() + 16.0)..=(edge.right() - 16.0),
        ui.cursor().top(),
        egui::Stroke::new(1.0, mix(pal.surface, pal.line, 0.6)),
    );
    clicked
}
