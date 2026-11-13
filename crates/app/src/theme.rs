//! Visual identity: palette, typography and the egui style built from them.
//!
//! The look is taken from the product name. Zest is the bright outer peel of
//! an orange: the one prominent action colour is orange, "clean" states use
//! the green of a citrus leaf, and both sit on cool slate surfaces, the
//! complement that lets the orange carry. Headings and figures are set in a
//! DIN-style face so numbers read like instrument dials.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke,
    TextStyle,
};
use std::sync::Arc;

/// Named font family for headings and large figures.
pub const DISPLAY: &str = "display";
/// Named font family for emphasised body text.
pub const SEMIBOLD: &str = "semibold";
/// Named font family for filled icons.
pub const ICON_FILL: &str = "phosphor-fill";

/// Corner radius of controls (buttons, inputs, rows).
pub const RADIUS: u8 = 6;
/// Corner radius of panels.
pub const RADIUS_PANEL: u8 = 10;

/// All colours used by the interface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// Window background.
    pub bg: Color32,
    /// Navigation rail background.
    pub rail: Color32,
    /// Panels and list backgrounds.
    pub surface: Color32,
    /// Hovered rows, raised controls.
    pub surface_hi: Color32,
    /// Recessed areas: input fields, progress tracks.
    pub well: Color32,
    /// Hairlines and control outlines.
    pub line: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    /// Solid accent: the primary action colour.
    pub accent: Color32,
    pub accent_hi: Color32,
    pub accent_lo: Color32,
    /// Text drawn on the accent colour.
    pub on_accent: Color32,
    /// Accent tone that is legible as text on `bg`/`surface`.
    pub accent_text: Color32,
    /// Leaf green: clean, done, healthy.
    pub fresh: Color32,
    /// Dull peel: what can be cleaned.
    pub stale: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub info: Color32,
}

impl Palette {
    /// Orange and leaf green on dark slate.
    pub const DARK: Palette = Palette {
        dark: true,
        bg: Color32::from_rgb(0x1A, 0x20, 0x28),
        rail: Color32::from_rgb(0x14, 0x19, 0x20),
        surface: Color32::from_rgb(0x22, 0x2A, 0x34),
        surface_hi: Color32::from_rgb(0x2B, 0x35, 0x41),
        well: Color32::from_rgb(0x13, 0x18, 0x1E),
        line: Color32::from_rgb(0x34, 0x3F, 0x4D),
        text: Color32::from_rgb(0xE9, 0xED, 0xF2),
        text_muted: Color32::from_rgb(0x9E, 0xAB, 0xBA),
        text_faint: Color32::from_rgb(0x6F, 0x7D, 0x8D),
        accent: Color32::from_rgb(0xF7, 0x93, 0x1E),
        accent_hi: Color32::from_rgb(0xFF, 0xAB, 0x45),
        accent_lo: Color32::from_rgb(0xDB, 0x78, 0x0C),
        on_accent: Color32::from_rgb(0x26, 0x13, 0x02),
        accent_text: Color32::from_rgb(0xFF, 0xA6, 0x4D),
        fresh: Color32::from_rgb(0x7C, 0xC8, 0x5A),
        stale: Color32::from_rgb(0x94, 0x70, 0x4E),
        warn: Color32::from_rgb(0xEF, 0xC4, 0x4B),
        danger: Color32::from_rgb(0xEF, 0x5F, 0x6C),
        info: Color32::from_rgb(0x74, 0xAC, 0xEA),
    };

    /// The same colours on light surfaces.
    pub const LIGHT: Palette = Palette {
        dark: false,
        bg: Color32::from_rgb(0xF1, 0xF3, 0xF5),
        rail: Color32::from_rgb(0xE4, 0xE8, 0xEC),
        surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        surface_hi: Color32::from_rgb(0xEC, 0xF0, 0xF4),
        well: Color32::from_rgb(0xE7, 0xEB, 0xEF),
        line: Color32::from_rgb(0xD0, 0xD7, 0xDF),
        text: Color32::from_rgb(0x1B, 0x23, 0x2D),
        text_muted: Color32::from_rgb(0x55, 0x63, 0x73),
        text_faint: Color32::from_rgb(0x84, 0x91, 0xA0),
        accent: Color32::from_rgb(0xF2, 0x8A, 0x12),
        accent_hi: Color32::from_rgb(0xFB, 0xA0, 0x3A),
        accent_lo: Color32::from_rgb(0xD9, 0x74, 0x00),
        on_accent: Color32::from_rgb(0x2A, 0x15, 0x00),
        accent_text: Color32::from_rgb(0xB0, 0x56, 0x00),
        fresh: Color32::from_rgb(0x3B, 0x8C, 0x2A),
        stale: Color32::from_rgb(0xA8, 0x8C, 0x6C),
        warn: Color32::from_rgb(0x8F, 0x6C, 0x00),
        danger: Color32::from_rgb(0xC8, 0x30, 0x40),
        info: Color32::from_rgb(0x2C, 0x6D, 0xB8),
    };

    /// Picks the palette for a theme setting, consulting Windows for "System".
    pub fn for_setting(theme: zest_engine::settings::Theme) -> Palette {
        use zest_engine::settings::Theme;
        match theme {
            Theme::Dark => Palette::DARK,
            Theme::Light => Palette::LIGHT,
            Theme::System => {
                if windows_prefers_light() {
                    Palette::LIGHT
                } else {
                    Palette::DARK
                }
            }
        }
    }

    /// A translucent version of `color`, for tinted backgrounds.
    pub fn tint(&self, color: Color32, alpha: f32) -> Color32 {
        mix(self.surface, color, alpha)
    }
}

/// Linear blend of two opaque colours.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

fn windows_prefers_light() -> bool {
    zest_engine::core::reg::RegPath::hkcu(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
    )
    .read_u32("AppsUseLightTheme")
        == Some(1)
}

fn load_system_font(file: &str) -> Option<FontData> {
    let path = zest_engine::core::paths::windows_dir()
        .join("Fonts")
        .join(file);
    std::fs::read(path).ok().map(FontData::from_owned)
}

/// Installs fonts: the Windows interface face for text, a DIN-style face for
/// headings and figures, and the icon font. Fonts are read from the Windows
/// font folder at start-up; when one is missing egui's built-in face is used.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    let mut register = |name: &str, file: &str| -> bool {
        match load_system_font(file) {
            Some(data) => {
                fonts.font_data.insert(name.to_owned(), Arc::new(data));
                true
            }
            None => false,
        }
    };
    let has_body = register("segoe", "segoeui.ttf");
    let has_semibold = register("segoe-semibold", "seguisb.ttf");
    let has_display = register("din", "bahnschrift.ttf");
    let has_symbols = register("segoe-symbols", "seguisym.ttf");

    let fallback: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let chain = |first: Option<&str>| -> Vec<String> {
        let mut list: Vec<String> = Vec::new();
        list.extend(first.map(str::to_owned));
        if has_body && first != Some("segoe") {
            list.push("segoe".to_owned());
        }
        list.extend(fallback.iter().cloned());
        if has_symbols {
            list.push("segoe-symbols".to_owned());
        }
        list
    };

    fonts
        .families
        .insert(FontFamily::Proportional, chain(has_body.then_some("segoe")));
    fonts.families.insert(
        FontFamily::Name(SEMIBOLD.into()),
        chain(has_semibold.then_some("segoe-semibold")),
    );
    fonts.families.insert(
        FontFamily::Name(DISPLAY.into()),
        chain(if has_display {
            Some("din")
        } else if has_semibold {
            Some("segoe-semibold")
        } else {
            None
        }),
    );

    // Filled icons live in their own family so they can be chosen explicitly.
    fonts.font_data.insert(
        "phosphor-fill".to_owned(),
        Arc::new(egui_phosphor::Variant::Fill.font_data()),
    );
    let mut fill_names = vec!["phosphor-fill".to_owned()];
    fill_names.extend(fallback.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name(ICON_FILL.into()), fill_names);

    ctx.set_fonts(fonts);
}

/// Applies the palette to egui's global style.
pub fn apply(ctx: &egui::Context, pal: &Palette) {
    ctx.set_theme(if pal.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (
                TextStyle::Small,
                FontId::new(12.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(14.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Heading,
                FontId::new(22.0, FontFamily::Name(DISPLAY.into())),
            ),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(8.0, 8.0);
        s.button_padding = egui::vec2(14.0, 7.0);
        s.interact_size = egui::vec2(40.0, 32.0);
        s.indent = 20.0;
        s.icon_width = 18.0;
        s.icon_width_inner = 10.0;
        s.icon_spacing = 8.0;
        s.combo_height = 320.0;
        s.scroll.bar_width = 10.0;
        s.scroll.floating = true;
        s.menu_margin = egui::Margin::same(6);
        s.window_margin = egui::Margin::same(20);

        let v = &mut style.visuals;
        v.dark_mode = pal.dark;
        v.override_text_color = Some(pal.text);
        v.panel_fill = pal.bg;
        v.window_fill = pal.surface;
        v.window_stroke = Stroke::new(1.0, pal.line);
        v.window_corner_radius = CornerRadius::same(RADIUS_PANEL + 2);
        v.menu_corner_radius = CornerRadius::same(RADIUS + 2);
        v.extreme_bg_color = pal.well;
        v.text_edit_bg_color = Some(pal.well);
        v.faint_bg_color = mix(pal.surface, pal.surface_hi, 0.45);
        v.code_bg_color = pal.well;
        v.hyperlink_color = pal.accent_text;
        v.warn_fg_color = pal.warn;
        v.error_fg_color = pal.danger;
        v.selection.bg_fill = mix(pal.surface, pal.accent, 0.35);
        v.selection.stroke = Stroke::new(1.0, pal.accent);
        v.window_shadow = egui::Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(if pal.dark { 110 } else { 40 }),
        };
        v.popup_shadow = egui::Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(if pal.dark { 100 } else { 36 }),
        };
        v.striped = false;
        v.indent_has_left_vline = false;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;

        let radius = CornerRadius::same(RADIUS);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = pal.surface;
        w.noninteractive.weak_bg_fill = pal.surface;
        w.noninteractive.bg_stroke = Stroke::new(1.0, pal.line);
        w.noninteractive.fg_stroke = Stroke::new(1.0, pal.text);
        w.noninteractive.corner_radius = radius;

        w.inactive.bg_fill = pal.surface_hi;
        w.inactive.weak_bg_fill = pal.surface_hi;
        w.inactive.bg_stroke = Stroke::new(1.0, pal.line);
        w.inactive.fg_stroke = Stroke::new(1.0, pal.text);
        w.inactive.corner_radius = radius;
        w.inactive.expansion = 0.0;

        w.hovered.bg_fill = mix(pal.surface_hi, pal.text, 0.08);
        w.hovered.weak_bg_fill = mix(pal.surface_hi, pal.text, 0.08);
        w.hovered.bg_stroke = Stroke::new(1.0, mix(pal.line, pal.text, 0.25));
        w.hovered.fg_stroke = Stroke::new(1.0, pal.text);
        w.hovered.corner_radius = radius;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = mix(pal.surface_hi, pal.text, 0.14);
        w.active.weak_bg_fill = mix(pal.surface_hi, pal.text, 0.14);
        w.active.bg_stroke = Stroke::new(1.0, pal.accent);
        w.active.fg_stroke = Stroke::new(1.0, pal.text);
        w.active.corner_radius = radius;
        w.active.expansion = 0.0;

        w.open.bg_fill = pal.surface_hi;
        w.open.weak_bg_fill = pal.surface_hi;
        w.open.bg_stroke = Stroke::new(1.0, pal.accent);
        w.open.fg_stroke = Stroke::new(1.0, pal.text);
        w.open.corner_radius = radius;
    });
}

/// Typography helpers. Sizes follow one scale: 12 / 14 / 16 / 22 / 30 / 44.
pub mod text {
    use super::*;

    /// Regular body text.
    pub fn body(s: impl Into<String>) -> RichText {
        RichText::new(s).size(14.0)
    }

    /// Emphasised body text.
    pub fn strong(s: impl Into<String>) -> RichText {
        RichText::new(s)
            .size(14.0)
            .family(FontFamily::Name(SEMIBOLD.into()))
    }

    /// Secondary text.
    pub fn muted(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s).size(14.0).color(pal.text_muted)
    }

    /// Small secondary text.
    pub fn small(s: impl Into<String>, pal: &Palette) -> RichText {
        RichText::new(s).size(12.0).color(pal.text_muted)
    }

    /// Section heading inside a page.
    pub fn section(s: impl Into<String>) -> RichText {
        RichText::new(s)
            .size(16.0)
            .family(FontFamily::Name(SEMIBOLD.into()))
    }

    /// Page title.
    pub fn title(s: impl Into<String>) -> RichText {
        RichText::new(s)
            .size(26.0)
            .family(FontFamily::Name(DISPLAY.into()))
    }

    /// A figure in the display face at the given size.
    pub fn figure(s: impl Into<String>, size: f32) -> RichText {
        RichText::new(s)
            .size(size)
            .family(FontFamily::Name(DISPLAY.into()))
    }

    /// An icon glyph at the given size.
    pub fn icon(glyph: &str, size: f32) -> RichText {
        RichText::new(glyph).size(size)
    }

    /// The semibold font at `size`.
    pub fn semibold_font(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
    }

    /// The body font at `size`.
    pub fn body_font(size: f32) -> FontId {
        FontId::new(size, FontFamily::Proportional)
    }
}
