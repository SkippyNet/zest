//! The widget set. Everything visible is drawn here so the whole app shares
//! one look; pages compose these and never paint ad hoc.

use crate::theme::{Palette, RADIUS, RADIUS_PANEL, mix, text};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Layout, Margin, Pos2,
    Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, epaint, pos2, vec2,
};
use egui_phosphor::regular as icon;
use std::time::{Duration, Instant};

// ── Text helpers ─────────────────────────────────────────────────────

/// Lays out one line of text that is cut off with an ellipsis at `max_width`.
pub fn elided(
    ui: &Ui,
    s: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(s.to_owned(), font, color);
    job.wrap.max_width = max_width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.painter().layout_job(job)
}

/// Paints one elided line inside `rect`, vertically centred.
pub fn paint_text(ui: &Ui, rect: Rect, s: &str, font: FontId, color: Color32, align: Align) {
    let galley = elided(ui, s, font, color, rect.width());
    let x = match align {
        Align::Min => rect.left(),
        Align::Center => rect.center().x - galley.size().x / 2.0,
        Align::Max => rect.right() - galley.size().x,
    };
    let y = rect.center().y - galley.size().y / 2.0;
    ui.painter()
        .with_clip_rect(rect)
        .galley(pos2(x, y), galley, color);
}

// ── Buttons ──────────────────────────────────────────────────────────

/// Visual weight of a button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    /// The one main action of a view: solid orange.
    Primary,
    /// Everything else: outlined.
    Secondary,
    /// Irreversible actions.
    Danger,
    /// Low-key actions inside rows: text only until hovered.
    Quiet,
}

fn button_impl(
    ui: &mut Ui,
    pal: &Palette,
    glyph: Option<&str>,
    label: &str,
    emphasis: Emphasis,
) -> Response {
    let enabled = ui.is_enabled();
    let (fill, fill_hover, fill_down, fg, outline) = match emphasis {
        Emphasis::Primary => (
            pal.accent,
            pal.accent_hi,
            pal.accent_lo,
            pal.on_accent,
            Color32::TRANSPARENT,
        ),
        Emphasis::Secondary => (
            Color32::TRANSPARENT,
            mix(pal.surface, pal.text, 0.07),
            mix(pal.surface, pal.text, 0.12),
            pal.text,
            mix(pal.line, pal.text, 0.18),
        ),
        Emphasis::Danger => (
            Color32::TRANSPARENT,
            pal.tint(pal.danger, 0.16),
            pal.tint(pal.danger, 0.26),
            pal.danger,
            mix(pal.line, pal.danger, 0.6),
        ),
        Emphasis::Quiet => (
            Color32::TRANSPARENT,
            mix(pal.surface, pal.text, 0.07),
            mix(pal.surface, pal.text, 0.12),
            pal.accent_text,
            Color32::TRANSPARENT,
        ),
    };
    let font = if emphasis == Emphasis::Quiet {
        text::body_font(14.0)
    } else {
        text::semibold_font(14.0)
    };
    let fg = if enabled { fg } else { pal.text_faint };
    let label_galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let icon_galley = glyph.map(|g| {
        ui.painter()
            .layout_no_wrap(g.to_owned(), text::body_font(17.0), fg)
    });
    let gap = if icon_galley.is_some() && !label.is_empty() {
        8.0
    } else {
        0.0
    };
    let pad_x = if emphasis == Emphasis::Quiet {
        10.0
    } else {
        16.0
    };
    let content_w = label_galley.size().x + icon_galley.as_ref().map_or(0.0, |g| g.size().x) + gap;
    let size = vec2((content_w + pad_x * 2.0).max(34.0), 34.0);

    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));

    if ui.is_rect_visible(rect) {
        let bg = if !enabled {
            if emphasis == Emphasis::Primary {
                mix(pal.surface, pal.text, 0.08)
            } else {
                Color32::TRANSPARENT
            }
        } else if response.is_pointer_button_down_on() {
            fill_down
        } else if response.hovered() {
            fill_hover
        } else {
            fill
        };
        let radius = CornerRadius::same(RADIUS);
        ui.painter().rect_filled(rect, radius, bg);
        if outline != Color32::TRANSPARENT {
            let stroke = if enabled { outline } else { pal.line };
            ui.painter()
                .rect_stroke(rect, radius, Stroke::new(1.0, stroke), StrokeKind::Inside);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                CornerRadius::same(RADIUS + 2),
                Stroke::new(2.0, pal.info),
                StrokeKind::Outside,
            );
        }
        let mut x = rect.center().x - content_w / 2.0;
        if let Some(g) = icon_galley {
            let y = rect.center().y - g.size().y / 2.0;
            let w = g.size().x;
            ui.painter().galley(pos2(x, y), g, fg);
            x += w + gap;
        }
        let y = rect.center().y - label_galley.size().y / 2.0;
        ui.painter().galley(pos2(x, y), label_galley, fg);
    }
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// The main action of a view.
pub fn primary_button(ui: &mut Ui, pal: &Palette, label: &str) -> Response {
    button_impl(ui, pal, None, label, Emphasis::Primary)
}

/// A regular action.
pub fn button(ui: &mut Ui, pal: &Palette, label: &str) -> Response {
    button_impl(ui, pal, None, label, Emphasis::Secondary)
}

/// A regular action with a leading icon.
pub fn icon_text_button(ui: &mut Ui, pal: &Palette, glyph: &str, label: &str) -> Response {
    button_impl(ui, pal, Some(glyph), label, Emphasis::Secondary)
}

/// An irreversible action.
pub fn danger_button(ui: &mut Ui, pal: &Palette, label: &str) -> Response {
    button_impl(ui, pal, None, label, Emphasis::Danger)
}

/// A low-key text action.
pub fn quiet_button(ui: &mut Ui, pal: &Palette, label: &str) -> Response {
    button_impl(ui, pal, None, label, Emphasis::Quiet)
}

/// A square icon-only button with a tooltip.
pub fn icon_button(ui: &mut Ui, pal: &Palette, glyph: &str, tooltip: &str) -> Response {
    let enabled = ui.is_enabled();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, tooltip));
    if ui.is_rect_visible(rect) {
        if enabled && response.hovered() {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(RADIUS),
                mix(pal.surface, pal.text, 0.09),
            );
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(RADIUS),
                Stroke::new(2.0, pal.info),
                StrokeKind::Inside,
            );
        }
        let color = if !enabled {
            pal.text_faint
        } else if response.hovered() {
            pal.text
        } else {
            pal.text_muted
        };
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            text::body_font(18.0),
            color,
        );
    }
    let response = if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    };
    response.on_hover_text(tooltip)
}

// ── Checkbox and switch ──────────────────────────────────────────────

/// State of a (possibly partial) checkbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    Off,
    On,
    /// Some but not all children are on.
    Mixed,
}

impl Check {
    /// Summarises a set of booleans.
    pub fn of(mut values: impl Iterator<Item = bool>) -> Check {
        let Some(first) = values.next() else {
            return Check::Off;
        };
        if values.all(|v| v == first) {
            if first { Check::On } else { Check::Off }
        } else {
            Check::Mixed
        }
    }
}

fn paint_check_box(
    ui: &Ui,
    pal: &Palette,
    rect: Rect,
    state: Check,
    hovered: bool,
    focused: bool,
    enabled: bool,
) {
    let radius = CornerRadius::same(4);
    let on = state != Check::Off;
    let fill = match (on, enabled) {
        (true, true) => pal.accent,
        (true, false) => mix(pal.surface, pal.text, 0.2),
        (false, _) => pal.well,
    };
    ui.painter().rect_filled(rect, radius, fill);
    let border = if on && enabled {
        pal.accent
    } else if hovered {
        mix(pal.line, pal.text, 0.45)
    } else {
        mix(pal.line, pal.text, 0.2)
    };
    ui.painter()
        .rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);
    if focused {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(6),
            Stroke::new(2.0, pal.info),
            StrokeKind::Outside,
        );
    }
    let mark = Stroke::new(2.0, if enabled { pal.on_accent } else { pal.surface });
    match state {
        Check::On => {
            let c = rect.center();
            let s = rect.width() / 18.0;
            ui.painter().line(
                vec![
                    pos2(c.x - 4.5 * s, c.y + 0.2 * s),
                    pos2(c.x - 1.4 * s, c.y + 3.3 * s),
                    pos2(c.x + 4.8 * s, c.y - 3.4 * s),
                ],
                mark,
            );
        }
        Check::Mixed => {
            let c = rect.center();
            ui.painter()
                .line_segment([pos2(c.x - 4.5, c.y), pos2(c.x + 4.5, c.y)], mark);
        }
        Check::Off => {}
    }
}

/// A bare tri-state checkbox. Returns the response; `clicked()` means toggle.
pub fn check(ui: &mut Ui, pal: &Palette, state: Check, name: &str) -> Response {
    let enabled = ui.is_enabled();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), sense);
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::Checkbox, enabled, state == Check::On, name)
    });
    if ui.is_rect_visible(rect) {
        paint_check_box(
            ui,
            pal,
            rect,
            state,
            response.hovered(),
            response.has_focus(),
            enabled,
        );
    }
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// A checkbox with a clickable label. Returns true when the value changed.
pub fn checkbox(ui: &mut Ui, pal: &Palette, value: &mut bool, label: &str) -> bool {
    let enabled = ui.is_enabled();
    let color = if enabled { pal.text } else { pal.text_faint };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), text::body_font(14.0), color);
    let size = vec2(18.0 + 9.0 + galley.size().x, 24.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *value, label));
    if ui.is_rect_visible(rect) {
        let box_rect =
            Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(18.0, 18.0));
        let state = if *value { Check::On } else { Check::Off };
        paint_check_box(
            ui,
            pal,
            box_rect,
            state,
            response.hovered(),
            response.has_focus(),
            enabled,
        );
        ui.painter().galley(
            pos2(
                box_rect.right() + 9.0,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            color,
        );
    }
    let clicked = response.clicked();
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
    if clicked {
        *value = !*value;
    }
    clicked
}

/// How long a switch knob takes to glide across after a click.
const SWITCH_GLIDE_SECONDS: f64 = 0.12;

/// An on/off switch. Returns true when the value changed.
pub fn switch(ui: &mut Ui, pal: &Palette, value: &mut bool, name: &str) -> bool {
    let enabled = ui.is_enabled();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 22.0), sense);
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, *value, name));
    let clicked = response.clicked();
    if clicked {
        *value = !*value;
    }
    // The knob glides only right after a click on this switch. Rows of
    // virtualised and live-sorted lists hand their widget ids on to other
    // rows as they scroll, so animating every change of value would replay
    // a neighbour's state: switches would seem to flip while scrolling.
    let now = ui.input(|i| i.time);
    let click_id = response.id.with("switch-click");
    if clicked {
        ui.ctx().data_mut(|d| d.insert_temp(click_id, now));
    }
    let mut t = if *value { 1.0 } else { 0.0 };
    if let Some(started) = ui.ctx().data(|d| d.get_temp::<f64>(click_id)) {
        let progress = ((now - started) / SWITCH_GLIDE_SECONDS) as f32;
        if (0.0..1.0).contains(&progress) {
            t = if *value { progress } else { 1.0 - progress };
            ui.ctx().request_repaint();
        } else {
            ui.ctx().data_mut(|d| d.remove::<f64>(click_id));
        }
    }
    if ui.is_rect_visible(rect) {
        let track_off = mix(pal.well, pal.text, 0.12);
        let track = mix(
            track_off,
            if enabled { pal.accent } else { pal.text_faint },
            t,
        );
        ui.painter()
            .rect_filled(rect, CornerRadius::same(11), track);
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                CornerRadius::same(13),
                Stroke::new(2.0, pal.info),
                StrokeKind::Outside,
            );
        }
        let knob_x = egui::lerp((rect.left() + 11.0)..=(rect.right() - 11.0), t);
        let knob = mix(pal.text, pal.on_accent, t);
        ui.painter().circle_filled(
            pos2(knob_x, rect.center().y),
            7.5,
            if enabled { knob } else { pal.surface },
        );
    }
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
    clicked
}

// ── Small pieces ─────────────────────────────────────────────────────

/// Colour role of a status element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Good,
    Info,
    Warn,
    Danger,
    Accent,
}

impl Tone {
    /// Foreground colour of the tone.
    pub fn color(self, pal: &Palette) -> Color32 {
        match self {
            Tone::Neutral => pal.text_muted,
            Tone::Good => pal.fresh,
            Tone::Info => pal.info,
            Tone::Warn => pal.warn,
            Tone::Danger => pal.danger,
            Tone::Accent => pal.accent_text,
        }
    }
}

/// A compact status label on a tinted background.
pub fn pill(ui: &mut Ui, pal: &Palette, label: &str, tone: Tone) -> Response {
    let fg = tone.color(pal);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), text::body_font(12.0), fg);
    let size = vec2(galley.size().x + 16.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(11), pal.tint(fg, 0.16));
        ui.painter().galley(
            pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0),
            galley,
            fg,
        );
    }
    response
}

/// A hairline across the available width.
pub fn hairline(ui: &mut Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, pal.line);
}

/// A panel: the standard container for a block of related content.
pub fn panel(pal: &Palette) -> Frame {
    Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.line))
        .corner_radius(CornerRadius::same(RADIUS_PANEL))
        .inner_margin(Margin::same(18))
}

/// A panel without padding, for lists and tables that draw to the edge.
pub fn list_panel(pal: &Palette) -> Frame {
    panel(pal).inner_margin(Margin::same(0))
}

/// Page title with an optional one-line explanation.
pub fn page_header(ui: &mut Ui, pal: &Palette, title: &str, subtitle: &str) {
    ui.label(text::title(title));
    if !subtitle.is_empty() {
        ui.add_space(-2.0);
        ui.label(text::muted(subtitle, pal));
    }
    ui.add_space(10.0);
}

/// A message bar. Returns true when its action button was clicked.
pub fn banner(
    ui: &mut Ui,
    pal: &Palette,
    tone: Tone,
    glyph: &str,
    message: &str,
    action: Option<&str>,
) -> bool {
    let fg = tone.color(pal);
    let mut clicked = false;
    Frame::new()
        .fill(pal.tint(fg, 0.12))
        .stroke(Stroke::new(1.0, pal.tint(fg, 0.38)))
        .corner_radius(CornerRadius::same(RADIUS + 2))
        .inner_margin(Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(text::icon(glyph, 18.0).color(fg));
                // Place the action first (from the right) so the message
                // wraps inside whatever width is left.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(label) = action
                        && button(ui, pal, label).clicked()
                    {
                        clicked = true;
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(text::body(message))
                                .wrap()
                                .selectable(false),
                        );
                    });
                });
            });
        });
    clicked
}

/// Centred invitation shown when a view has nothing to list yet.
/// Returns true when the action button was clicked.
pub fn empty_state(
    ui: &mut Ui,
    pal: &Palette,
    glyph: &str,
    title: &str,
    body: &str,
    action: Option<&str>,
) -> bool {
    let mut clicked = false;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.22).clamp(24.0, 140.0));
        ui.label(text::icon(glyph, 46.0).color(pal.text_faint));
        ui.add_space(4.0);
        ui.label(text::section(title));
        ui.scope(|ui| {
            ui.set_max_width(460.0);
            ui.label(text::muted(body, pal));
        });
        if let Some(label) = action {
            ui.add_space(10.0);
            if primary_button(ui, pal, label).clicked() {
                clicked = true;
            }
        }
    });
    clicked
}

/// A spinner glyph that rotates while something is in progress.
pub fn spinner(ui: &mut Ui, pal: &Palette, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let time = ui.input(|i| i.time) as f32;
        let center = rect.center();
        let r = size / 2.0 - 2.0;
        let start = time * 5.0;
        let points: Vec<Pos2> = (0..=24)
            .map(|i| {
                let a = start + i as f32 / 24.0 * std::f32::consts::TAU * 0.72;
                pos2(center.x + r * a.cos(), center.y + r * a.sin())
            })
            .collect();
        ui.painter().line(points, Stroke::new(2.0, pal.accent));
        ui.ctx().request_repaint();
    }
}

/// A progress bar. `None` draws an indeterminate animation.
pub fn progress_bar(ui: &mut Ui, pal: &Palette, fraction: Option<f32>, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let radius = CornerRadius::same((height / 2.0) as u8);
    ui.painter().rect_filled(rect, radius, pal.well);
    match fraction {
        Some(f) => {
            let f = f.clamp(0.0, 1.0);
            if f > 0.0 {
                let mut fill = rect;
                fill.set_width((rect.width() * f).max(height));
                ui.painter().rect_filled(fill, radius, pal.accent);
            }
        }
        None => {
            let time = ui.input(|i| i.time) as f32;
            let span = rect.width() * 0.28;
            let travel = rect.width() + span;
            let x = rect.left() - span + (time * 0.55).fract() * travel;
            let seg = Rect::from_min_max(pos2(x, rect.top()), pos2(x + span, rect.bottom()))
                .intersect(rect);
            if seg.width() > 0.0 {
                ui.painter().rect_filled(seg, radius, pal.accent);
            }
            ui.ctx().request_repaint();
        }
    }
}

/// Underlined tabs. Returns true when the selection changed.
pub fn tabs(ui: &mut Ui, pal: &Palette, selected: &mut usize, labels: &[&str]) -> bool {
    let mut changed = false;
    let row = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, label) in labels.iter().enumerate() {
                let active = *selected == i;
                let font = if active {
                    text::semibold_font(14.0)
                } else {
                    text::body_font(14.0)
                };
                let color = if active { pal.text } else { pal.text_muted };
                let galley = ui
                    .painter()
                    .layout_no_wrap((*label).to_owned(), font, color);
                let (rect, response) =
                    ui.allocate_exact_size(vec2(galley.size().x + 24.0, 36.0), Sense::click());
                response.widget_info(|| {
                    WidgetInfo::selected(WidgetType::RadioButton, true, active, *label)
                });
                if ui.is_rect_visible(rect) {
                    if response.hovered() && !active {
                        ui.painter().rect_filled(
                            rect.shrink2(vec2(0.0, 3.0)),
                            CornerRadius::same(RADIUS),
                            mix(pal.bg, pal.text, 0.06),
                        );
                    }
                    let color = if response.hovered() { pal.text } else { color };
                    ui.painter().galley(
                        pos2(
                            rect.left() + 12.0,
                            rect.center().y - galley.size().y / 2.0 - 1.0,
                        ),
                        galley,
                        color,
                    );
                    if active {
                        let line = Rect::from_min_max(
                            pos2(rect.left() + 12.0, rect.bottom() - 2.5),
                            pos2(rect.right() - 12.0, rect.bottom()),
                        );
                        ui.painter()
                            .rect_filled(line, CornerRadius::same(1), pal.accent);
                    }
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            rect.shrink(2.0),
                            CornerRadius::same(RADIUS),
                            Stroke::new(2.0, pal.info),
                            StrokeKind::Inside,
                        );
                    }
                }
                if response.on_hover_cursor(CursorIcon::PointingHand).clicked() && !active {
                    *selected = i;
                    changed = true;
                }
            }
        })
        .response
        .rect;
    let y = row.bottom();
    let x = ui.max_rect().x_range();
    ui.painter().hline(x, y, Stroke::new(1.0, pal.line));
    changed
}

/// A search field with a leading magnifier. Returns true when the text changed.
pub fn search_field(
    ui: &mut Ui,
    pal: &Palette,
    value: &mut String,
    hint: &str,
    width: f32,
) -> bool {
    let mut changed = false;
    Frame::new()
        .fill(pal.well)
        .stroke(Stroke::new(1.0, pal.line))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(text::icon(icon::MAGNIFYING_GLASS, 16.0).color(pal.text_faint));
                let edit = egui::TextEdit::singleline(value)
                    .hint_text(hint)
                    .frame(Frame::NONE)
                    .desired_width(width - 60.0);
                if ui.add(edit).changed() {
                    changed = true;
                }
                if !value.is_empty() {
                    let clear = ui.add(
                        egui::Label::new(text::icon(icon::X, 14.0).color(pal.text_muted))
                            .sense(Sense::click()),
                    );
                    if clear.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        value.clear();
                        changed = true;
                    }
                }
            });
        });
    changed
}

// ── The signature element: the usage bar ─────────────────────────────

/// What a section of the [`usage_bar`] stands for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    /// Space in use: solid orange.
    Solid,
    /// Space that can be reclaimed: dull and hatched.
    Hatched,
    /// A plain colour (used by breakdown charts).
    Flat(Color32),
}

fn rounded_points(rect: Rect, radii: [f32; 4]) -> Vec<Pos2> {
    // radii: top-left, top-right, bottom-right, bottom-left
    let max = rect.width().min(rect.height()) / 2.0;
    let corners = [
        (pos2(rect.left(), rect.top()), radii[0].min(max), 180.0f32),
        (pos2(rect.right(), rect.top()), radii[1].min(max), 270.0),
        (pos2(rect.right(), rect.bottom()), radii[2].min(max), 0.0),
        (pos2(rect.left(), rect.bottom()), radii[3].min(max), 90.0),
    ];
    let mut points = Vec::with_capacity(40);
    for (i, (corner, r, start)) in corners.into_iter().enumerate() {
        let centre = match i {
            0 => pos2(corner.x + r, corner.y + r),
            1 => pos2(corner.x - r, corner.y + r),
            2 => pos2(corner.x - r, corner.y - r),
            _ => pos2(corner.x + r, corner.y - r),
        };
        if r < 0.5 {
            points.push(corner);
            continue;
        }
        for step in 0..=8 {
            let a = (start + step as f32 * 90.0 / 8.0).to_radians();
            points.push(pos2(centre.x + r * a.cos(), centre.y + r * a.sin()));
        }
    }
    points
}

/// Fills a rounded shape with a vertical gradient.
fn gradient_fill(ui: &Ui, rect: Rect, radii: [f32; 4], top: Color32, bottom: Color32) {
    let points = rounded_points(rect, radii);
    let mut mesh = epaint::Mesh::default();
    let shade = |p: Pos2| {
        mix(
            top,
            bottom,
            ((p.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0),
        )
    };
    let centre = rect.center();
    mesh.colored_vertex(centre, shade(centre));
    for p in &points {
        mesh.colored_vertex(*p, shade(*p));
    }
    let n = points.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    ui.painter().add(mesh);
}

/// A glossy bar split into sections; the widths are fractions of
/// the whole and whatever remains is shown as an empty track.
///
/// `sheen` (0..1) sweeps a highlight across the bar: the "polish" moment
/// played once after a clean.
pub fn usage_bar(
    ui: &mut Ui,
    pal: &Palette,
    sections: &[(f32, Segment)],
    height: f32,
    sheen: Option<f32>,
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let r = (height / 2.0).min(7.0);
    ui.painter()
        .rect_filled(rect, CornerRadius::same(r as u8), pal.well);

    let mut x = rect.left();
    let total = sections.len();
    for (index, (fraction, segment)) in sections.iter().enumerate() {
        let w = rect.width() * fraction.clamp(0.0, 1.0);
        if w < 0.5 {
            continue;
        }
        let seg = Rect::from_min_max(
            pos2(x, rect.top()),
            pos2((x + w).min(rect.right()), rect.bottom()),
        );
        let first = (x - rect.left()).abs() < 0.5;
        let last = index + 1 == total && (seg.right() - rect.right()).abs() < 0.5;
        let radii = [
            if first { r } else { 0.0 },
            if last { r } else { 0.0 },
            if last { r } else { 0.0 },
            if first { r } else { 0.0 },
        ];
        match segment {
            Segment::Solid => {
                gradient_fill(
                    ui,
                    seg,
                    radii,
                    mix(pal.accent_hi, Color32::WHITE, 0.18),
                    pal.accent_lo,
                );
                // A bright lip along the top edge gives the bar its gloss.
                let lip = Rect::from_min_max(
                    pos2(seg.left() + radii[0], seg.top() + 1.0),
                    pos2(seg.right() - radii[1], seg.top() + 2.0),
                );
                if lip.width() > 0.0 {
                    ui.painter().rect_filled(
                        lip,
                        0.0,
                        Color32::from_white_alpha(if pal.dark { 70 } else { 120 }),
                    );
                }
            }
            Segment::Hatched => {
                let base = mix(pal.stale, pal.well, 0.35);
                gradient_fill(
                    ui,
                    seg,
                    radii,
                    mix(base, Color32::WHITE, 0.06),
                    mix(base, Color32::BLACK, 0.12),
                );
                let clipped = ui.painter().with_clip_rect(seg.shrink(1.0));
                let stroke = Stroke::new(1.5, mix(pal.stale, pal.accent, 0.35));
                let step = 7.0;
                let mut hx = seg.left() - seg.height();
                while hx < seg.right() {
                    clipped.line_segment(
                        [pos2(hx, seg.bottom()), pos2(hx + seg.height(), seg.top())],
                        stroke,
                    );
                    hx += step;
                }
            }
            Segment::Flat(color) => gradient_fill(ui, seg, radii, *color, *color),
        }
        x += w;
        // A thin gap between sections keeps them readable.
        if x < rect.right() - 1.0 {
            ui.painter()
                .vline(x, rect.y_range(), Stroke::new(1.0, pal.well));
        }
    }

    if let Some(t) = sheen {
        let t = t.clamp(0.0, 1.0);
        let band = 90.0;
        let cx = rect.left() - band + (rect.width() + band * 2.0) * t;
        let skew = rect.height() * 0.9;
        let clipped = ui.painter().with_clip_rect(rect);
        let mut mesh = epaint::Mesh::default();
        let clear = Color32::from_white_alpha(0);
        let bright = Color32::from_white_alpha(if pal.dark { 150 } else { 200 });
        let xs = [cx - band / 2.0, cx, cx + band / 2.0];
        let colors = [clear, bright, clear];
        for (px, color) in xs.iter().zip(colors) {
            mesh.colored_vertex(pos2(px + skew, rect.top()), color);
            mesh.colored_vertex(pos2(px - skew, rect.bottom()), color);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 2, 3);
        mesh.add_triangle(2, 3, 4);
        mesh.add_triangle(3, 4, 5);
        clipped.add(mesh);
    }
    // An antialiased outline tidies the mesh edges.
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(r as u8),
        Stroke::new(1.0, mix(pal.line, pal.bg, 0.3)),
        StrokeKind::Inside,
    );
    response
}

/// A coloured dot with a label and a value, for chart legends.
pub fn legend(ui: &mut Ui, pal: &Palette, color: Color32, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(3), color);
        ui.label(text::strong(value));
        ui.label(text::muted(label, pal));
    });
}

/// A row of legend entries `(colour, label, value)` that wraps onto as many
/// lines as the available width needs.
pub fn legends(ui: &mut Ui, pal: &Palette, items: &[(Color32, &str, String)]) {
    let width = ui.available_width();
    let line_height = 24.0;
    let gap = 20.0;
    let laid: Vec<_> = items
        .iter()
        .map(|(color, label, value)| {
            let value =
                ui.painter()
                    .layout_no_wrap(value.clone(), text::semibold_font(14.0), pal.text);
            let label = ui.painter().layout_no_wrap(
                (*label).to_owned(),
                text::body_font(14.0),
                pal.text_muted,
            );
            let w = 10.0 + 6.0 + value.size().x + 6.0 + label.size().x;
            (*color, value, label, w)
        })
        .collect();
    // First pass: assign positions.
    let mut positions = Vec::with_capacity(laid.len());
    let (mut x, mut line) = (0.0f32, 0usize);
    for (_, _, _, w) in &laid {
        if x > 0.0 && x + w > width {
            x = 0.0;
            line += 1;
        }
        positions.push((x, line));
        x += w + gap;
    }
    let lines = line + 1;
    let (rect, _) = ui.allocate_exact_size(vec2(width, lines as f32 * line_height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    for ((color, value, label, _), (x, line)) in laid.into_iter().zip(positions) {
        let cy = rect.top() + line as f32 * line_height + line_height / 2.0;
        let mut px = rect.left() + x;
        let dot = Rect::from_center_size(pos2(px + 5.0, cy), vec2(10.0, 10.0));
        ui.painter().rect_filled(dot, CornerRadius::same(3), color);
        px += 16.0;
        let value_w = value.size().x;
        ui.painter()
            .galley(pos2(px, cy - value.size().y / 2.0), value, pal.text);
        px += value_w + 6.0;
        ui.painter()
            .galley(pos2(px, cy - label.size().y / 2.0), label, pal.text_muted);
    }
}

// ── Navigation rail ──────────────────────────────────────────────────

/// Height of one entry of the navigation rail.
pub const NAV_ITEM_HEIGHT: f32 = 32.0;

/// One entry of the navigation rail. Returns true when clicked.
pub fn nav_item(
    ui: &mut Ui,
    pal: &Palette,
    glyph: &str,
    label: &str,
    selected: bool,
    badge: Option<&str>,
) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), NAV_ITEM_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, label));
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same(RADIUS);
        if selected {
            ui.painter()
                .rect_filled(rect, radius, mix(pal.rail, pal.text, 0.09));
            let marker = Rect::from_min_max(
                pos2(rect.left(), rect.top() + 8.0),
                pos2(rect.left() + 3.0, rect.bottom() - 8.0),
            );
            ui.painter()
                .rect_filled(marker, CornerRadius::same(2), pal.accent);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, radius, mix(pal.rail, pal.text, 0.05));
        }
        if response.has_focus() {
            ui.painter()
                .rect_stroke(rect, radius, Stroke::new(2.0, pal.info), StrokeKind::Inside);
        }
        let fg = if selected || response.hovered() {
            pal.text
        } else {
            pal.text_muted
        };
        let icon_color = if selected { pal.accent_text } else { fg };
        ui.painter().text(
            pos2(rect.left() + 24.0, rect.center().y),
            Align2::CENTER_CENTER,
            glyph,
            text::body_font(18.0),
            icon_color,
        );
        let font = if selected {
            text::semibold_font(14.0)
        } else {
            text::body_font(14.0)
        };
        let label_rect = Rect::from_min_max(
            pos2(rect.left() + 44.0, rect.top()),
            pos2(rect.right() - 8.0, rect.bottom()),
        );
        paint_text(ui, label_rect, label, font, fg, Align::Min);
        if let Some(badge) = badge {
            let galley = ui.painter().layout_no_wrap(
                badge.to_owned(),
                text::semibold_font(11.0),
                pal.on_accent,
            );
            let w = (galley.size().x + 10.0).max(18.0);
            let badge_rect = Rect::from_center_size(
                pos2(rect.right() - 10.0 - w / 2.0, rect.center().y),
                vec2(w, 18.0),
            );
            ui.painter()
                .rect_filled(badge_rect, CornerRadius::same(9), pal.accent);
            ui.painter().galley(
                pos2(
                    badge_rect.center().x - galley.size().x / 2.0,
                    badge_rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                pal.on_accent,
            );
        }
    }
    response.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

// ── Lists ────────────────────────────────────────────────────────────

/// Width of one column of a [`Columns`] layout.
#[derive(Clone, Copy, Debug)]
pub enum Col {
    /// Exactly this many points.
    Fixed(f32),
    /// A share of the remaining width.
    Flex(f32),
}

/// A column layout shared by a list's header and its rows.
#[derive(Clone, Debug)]
pub struct Columns(pub Vec<Col>);

impl Columns {
    /// Splits `row` into cell rectangles with `pad` points between them.
    pub fn cells(&self, row: Rect, pad: f32) -> Vec<Rect> {
        let fixed: f32 = self
            .0
            .iter()
            .map(|c| match c {
                Col::Fixed(w) => *w,
                Col::Flex(_) => 0.0,
            })
            .sum();
        let flex: f32 = self
            .0
            .iter()
            .map(|c| match c {
                Col::Flex(f) => *f,
                Col::Fixed(_) => 0.0,
            })
            .sum();
        let gaps = pad * (self.0.len().saturating_sub(1)) as f32;
        let free = (row.width() - fixed - gaps).max(0.0);
        let mut x = row.left();
        self.0
            .iter()
            .map(|c| {
                let w = match c {
                    Col::Fixed(w) => *w,
                    Col::Flex(f) => {
                        if flex > 0.0 {
                            free * f / flex
                        } else {
                            0.0
                        }
                    }
                };
                let cell = Rect::from_min_max(pos2(x, row.top()), pos2(x + w, row.bottom()));
                x += w + pad;
                cell
            })
            .collect()
    }
}

/// Horizontal padding inside list rows.
pub const ROW_PAD: f32 = 16.0;

/// Draws a list header. `sort` is `(column, ascending)`; clicking a sortable
/// title updates it. Returns true when the sort order changed.
pub fn list_header(
    ui: &mut Ui,
    pal: &Palette,
    columns: &Columns,
    titles: &[(&str, Align, bool)],
    sort: &mut (usize, bool),
) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    let inner = rect.shrink2(vec2(ROW_PAD, 0.0));
    let mut changed = false;
    for (i, (cell, (title, align, sortable))) in columns
        .cells(inner, 12.0)
        .into_iter()
        .zip(titles)
        .enumerate()
    {
        if title.is_empty() {
            continue;
        }
        let active = *sortable && sort.0 == i;
        let color = if active { pal.text } else { pal.text_muted };
        let mut label = (*title).to_owned();
        if active {
            label.push(' ');
            label.push_str(if sort.1 {
                icon::CARET_UP
            } else {
                icon::CARET_DOWN
            });
        }
        if *sortable {
            let response = ui.interact(cell, ui.id().with(("header", i)), Sense::click());
            let color = if response.hovered() { pal.text } else { color };
            paint_text(ui, cell, &label, text::body_font(12.5), color, *align);
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                *sort = if sort.0 == i { (i, !sort.1) } else { (i, true) };
                changed = true;
            }
        } else {
            paint_text(ui, cell, &label, text::body_font(12.5), color, *align);
        }
    }
    ui.painter()
        .hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, pal.line));
    changed
}

/// Allocates one list row and paints its hover/selection background.
/// Returns the padded row rectangle and the row's response.
pub fn list_row(
    ui: &mut Ui,
    pal: &Palette,
    height: f32,
    selected: bool,
    sense: Sense,
) -> (Rect, Response) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
    if ui.is_rect_visible(rect) {
        if selected {
            ui.painter()
                .rect_filled(rect, 0.0, pal.tint(pal.accent, 0.13));
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, 0.0, mix(pal.surface, pal.text, 0.04));
        }
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            Stroke::new(1.0, mix(pal.surface, pal.line, 0.6)),
        );
    }
    (rect.shrink2(vec2(ROW_PAD, 0.0)), response)
}

/// A row without a separator line, for tree-like lists.
pub fn tree_row(ui: &mut Ui, pal: &Palette, height: f32, sense: Sense) -> (Rect, Response) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), sense);
    if ui.is_rect_visible(rect) && response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, mix(pal.surface, pal.text, 0.04));
    }
    (rect.shrink2(vec2(ROW_PAD, 0.0)), response)
}

/// Runs `add` in a child ui confined to `rect`, laid out left-to-right and
/// vertically centred. Used to put real widgets into list cells.
pub fn in_cell<R>(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    add(&mut child)
}

/// Like [`in_cell`] but laid out right-to-left.
pub fn in_cell_right<R>(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    add(&mut child)
}

// ── Dialogs and toasts ───────────────────────────────────────────────

/// Outcome of a confirmation dialog for the current frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Still open.
    Pending,
    /// The user confirmed.
    Yes,
    /// The user cancelled or dismissed the dialog.
    No,
}

/// A modal confirmation. Call every frame while it should be visible.
pub fn confirm_dialog(
    ctx: &egui::Context,
    pal: &Palette,
    id: &str,
    title: &str,
    body: &str,
    confirm_label: &str,
    danger: bool,
) -> Confirm {
    let mut outcome = Confirm::Pending;
    let modal = egui::Modal::new(egui::Id::new(("confirm", id)))
        .backdrop_color(Color32::from_black_alpha(if pal.dark { 150 } else { 90 }))
        .show(ctx, |ui| {
            ui.set_width(430.0);
            ui.label(text::section(title));
            ui.add_space(2.0);
            ui.add(egui::Label::new(text::muted(body, pal)).wrap());
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let confirmed = if danger {
                    danger_button(ui, pal, confirm_label).clicked()
                } else {
                    primary_button(ui, pal, confirm_label).clicked()
                };
                if confirmed {
                    outcome = Confirm::Yes;
                }
                if button(ui, pal, "Cancel").clicked() {
                    outcome = Confirm::No;
                }
            });
        });
    if modal.should_close() && outcome == Confirm::Pending {
        outcome = Confirm::No;
    }
    outcome
}

struct Toast {
    message: String,
    tone: Tone,
    born: Instant,
}

/// Short-lived notifications in the corner of the window.
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    const LIFETIME: Duration = Duration::from_millis(4500);

    /// Shows a success message.
    pub fn good(&mut self, message: impl Into<String>) {
        self.push(message, Tone::Good);
    }

    /// Shows a neutral message.
    pub fn info(&mut self, message: impl Into<String>) {
        self.push(message, Tone::Info);
    }

    /// Shows an error message.
    pub fn error(&mut self, message: impl Into<String>) {
        self.push(message, Tone::Danger);
    }

    fn push(&mut self, message: impl Into<String>, tone: Tone) {
        self.items.push(Toast {
            message: message.into(),
            tone,
            born: Instant::now(),
        });
        if self.items.len() > 4 {
            self.items.remove(0);
        }
    }

    /// Draws the toasts; call once per frame.
    pub fn show(&mut self, ctx: &egui::Context, pal: &Palette) {
        self.items.retain(|t| t.born.elapsed() < Self::LIFETIME);
        if self.items.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -20.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                    for toast in self.items.iter().rev() {
                        let fg = toast.tone.color(pal);
                        Frame::new()
                            .fill(mix(pal.surface_hi, pal.bg, 0.2))
                            .stroke(Stroke::new(1.0, pal.tint(fg, 0.5)))
                            .corner_radius(CornerRadius::same(RADIUS + 2))
                            .inner_margin(Margin::symmetric(14, 10))
                            .shadow(ctx.global_style().visuals.popup_shadow)
                            .show(ui, |ui| {
                                ui.set_max_width(380.0);
                                ui.horizontal(|ui| {
                                    let glyph = match toast.tone {
                                        Tone::Good => icon::CHECK_CIRCLE,
                                        Tone::Danger => icon::WARNING_CIRCLE,
                                        _ => icon::INFO,
                                    };
                                    ui.label(text::icon(glyph, 18.0).color(fg));
                                    ui.add(egui::Label::new(text::body(&toast.message)).wrap());
                                });
                            });
                    }
                });
            });
    }
}

/// Draws the application mark: an orange slice with one segment set apart.
///
/// The pixels come from [`crate::brand_icon`], the same renderer that makes
/// the window icon, so the mark looks identical everywhere.
pub fn brand_mark(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let pixels = (size * ui.ctx().pixels_per_point())
        .ceil()
        .clamp(16.0, 512.0) as u32;
    let id = egui::Id::new(("brand-mark", pixels));
    let cached = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(id));
    let texture = cached.unwrap_or_else(|| {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [pixels as usize; 2],
            &crate::brand_icon::render(pixels),
        );
        let handle = ui.ctx().load_texture(
            format!("brand-mark-{pixels}"),
            image,
            egui::TextureOptions::LINEAR,
        );
        ui.ctx().data_mut(|d| d.insert_temp(id, handle.clone()));
        handle
    });
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
}
