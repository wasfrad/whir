//! Design tokens and font setup: the single swap point for re_ui-style theming later.
//!
//! Everything visual that isn't egui's own widget styling lives here, so a future
//! vendored design system only has to replace this module.

use egui::Color32;

#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,

    /// Content area background (also the active tab, so they read as one surface).
    pub content_bg: Color32,
    /// Tab strip / title bar background.
    pub strip_bg: Color32,
    pub tab_hover_bg: Color32,
    pub tab_text: Color32,
    pub tab_text_inactive: Color32,
    pub separator: Color32,
    pub accent: Color32,

    pub caption_hover: Color32,
    pub caption_pressed: Color32,
    pub close_hover: Color32,
    pub close_pressed: Color32,

    pub text: Color32,
    pub text_dim: Color32,
    pub error: Color32,
    pub selection_bg: Color32,
    pub selection_stroke: Color32,
    pub highlight_bg: Color32,

    /// Transparency checkerboard behind images.
    pub checker_a: Color32,
    pub checker_b: Color32,
    /// Background behind PDF pages / images.
    pub canvas_bg: Color32,

    pub grid_line: Color32,
    pub header_bg: Color32,
    pub gutter_bg: Color32,

    pub overlay_bg: Color32,

    pub metrics: Metrics,
}

/// Sizes in egui points.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub tab_strip_height: f32,
    pub tab_min_width: f32,
    pub tab_max_width: f32,
    pub tab_gap: f32,
    pub caption_button_width: f32,
    /// Empty strip area always kept draggable to the left of the caption buttons.
    pub drag_area_min_width: f32,
    pub body_text_size: f32,
    pub small_text_size: f32,
    pub corner_radius: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            tab_strip_height: 36.0,
            tab_min_width: 72.0,
            tab_max_width: 232.0,
            tab_gap: 2.0,
            caption_button_width: 46.0,
            drag_area_min_width: 48.0,
            body_text_size: 13.0,
            small_text_size: 11.5,
            corner_radius: 6.0,
        }
    }
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            dark: true,
            content_bg: Color32::from_rgb(0x24, 0x24, 0x28),
            strip_bg: Color32::from_rgb(0x1a, 0x1a, 0x1d),
            tab_hover_bg: Color32::from_rgb(0x2b, 0x2b, 0x30),
            tab_text: Color32::from_rgb(0xec, 0xec, 0xee),
            tab_text_inactive: Color32::from_rgb(0x9c, 0x9c, 0xa4),
            separator: Color32::from_rgb(0x38, 0x38, 0x3e),
            accent: Color32::from_rgb(0x4c, 0x8d, 0xff),
            caption_hover: Color32::from_rgb(0x2e, 0x2e, 0x33),
            caption_pressed: Color32::from_rgb(0x38, 0x38, 0x3e),
            close_hover: Color32::from_rgb(0xc4, 0x2b, 0x1c),
            close_pressed: Color32::from_rgb(0xa8, 0x26, 0x1a),
            text: Color32::from_rgb(0xe6, 0xe6, 0xe8),
            text_dim: Color32::from_rgb(0x9a, 0x9a, 0xa2),
            error: Color32::from_rgb(0xff, 0x6b, 0x6b),
            selection_bg: Color32::from_rgba_premultiplied(0x1d, 0x3a, 0x6e, 0xc0),
            selection_stroke: Color32::from_rgb(0x4c, 0x8d, 0xff),
            highlight_bg: Color32::from_rgba_premultiplied(0x6e, 0x5a, 0x10, 0xc0),
            checker_a: Color32::from_rgb(0x3a, 0x3a, 0x3e),
            checker_b: Color32::from_rgb(0x2e, 0x2e, 0x32),
            canvas_bg: Color32::from_rgb(0x1c, 0x1c, 0x1f),
            grid_line: Color32::from_rgb(0x33, 0x33, 0x39),
            header_bg: Color32::from_rgb(0x2c, 0x2c, 0x31),
            gutter_bg: Color32::from_rgb(0x20, 0x20, 0x24),
            overlay_bg: Color32::from_rgba_premultiplied(0x10, 0x10, 0x12, 0xd8),
            metrics: Metrics::default(),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            content_bg: Color32::from_rgb(0xfb, 0xfb, 0xfc),
            strip_bg: Color32::from_rgb(0xe8, 0xe8, 0xeb),
            tab_hover_bg: Color32::from_rgb(0xdb, 0xdb, 0xdf),
            tab_text: Color32::from_rgb(0x1b, 0x1b, 0x1f),
            tab_text_inactive: Color32::from_rgb(0x5c, 0x5c, 0x64),
            separator: Color32::from_rgb(0xcc, 0xcc, 0xd2),
            accent: Color32::from_rgb(0x25, 0x63, 0xeb),
            caption_hover: Color32::from_rgb(0xd6, 0xd6, 0xdb),
            caption_pressed: Color32::from_rgb(0xc8, 0xc8, 0xce),
            close_hover: Color32::from_rgb(0xc4, 0x2b, 0x1c),
            close_pressed: Color32::from_rgb(0xa8, 0x26, 0x1a),
            text: Color32::from_rgb(0x1b, 0x1b, 0x1f),
            text_dim: Color32::from_rgb(0x5f, 0x5f, 0x66),
            error: Color32::from_rgb(0xc0, 0x1c, 0x28),
            selection_bg: Color32::from_rgba_premultiplied(0xb4, 0xcc, 0xf4, 0xe0),
            selection_stroke: Color32::from_rgb(0x25, 0x63, 0xeb),
            highlight_bg: Color32::from_rgba_premultiplied(0xf0, 0xd8, 0x60, 0xe0),
            checker_a: Color32::from_rgb(0xff, 0xff, 0xff),
            checker_b: Color32::from_rgb(0xe4, 0xe4, 0xe8),
            canvas_bg: Color32::from_rgb(0xe3, 0xe3, 0xe7),
            grid_line: Color32::from_rgb(0xe2, 0xe2, 0xe6),
            header_bg: Color32::from_rgb(0xf0, 0xf0, 0xf3),
            gutter_bg: Color32::from_rgb(0xf4, 0xf4, 0xf6),
            overlay_bg: Color32::from_rgba_premultiplied(0xf8, 0xf8, 0xfa, 0xf0),
            metrics: Metrics::default(),
        }
    }

    /// Apply to egui's own widget styling.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = self.content_bg;
        visuals.window_fill = self.content_bg;
        visuals.extreme_bg_color = self.canvas_bg;
        visuals.selection.bg_fill = self.selection_bg;
        visuals.selection.stroke.color = self.selection_stroke;
        visuals.hyperlink_color = self.accent;
        visuals.override_text_color = Some(self.text);
        ctx.set_visuals_of(
            if self.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            },
            visuals,
        );
        ctx.set_theme(if self.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        let m = self.metrics;
        ctx.all_styles_mut(|style| {
            use egui::{FontFamily, FontId, TextStyle};
            style.text_styles = [
                (
                    TextStyle::Small,
                    FontId::new(m.small_text_size, FontFamily::Proportional),
                ),
                (
                    TextStyle::Body,
                    FontId::new(m.body_text_size, FontFamily::Proportional),
                ),
                (
                    TextStyle::Button,
                    FontId::new(m.body_text_size, FontFamily::Proportional),
                ),
                (
                    TextStyle::Heading,
                    FontId::new(18.0, FontFamily::Proportional),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(m.body_text_size, FontFamily::Monospace),
                ),
            ]
            .into();
            style.interaction.tooltip_delay = 0.35;
        });
    }

    /// The theme the UI thread is currently drawing with.
    pub fn current() -> Theme {
        CURRENT.with(|t| t.borrow().clone())
    }

    pub fn set_current(theme: Theme) {
        CURRENT.with(|t| *t.borrow_mut() = theme);
    }
}

thread_local! {
    static CURRENT: std::cell::RefCell<Theme> = std::cell::RefCell::new(Theme::dark());
}

/// Which egui family a font file feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontRole {
    Proportional,
    Monospace,
    /// Appended to both families as a glyph fallback.
    Fallback,
}

/// A font the shell loads from `%WINDIR%\Fonts` at startup.
/// Described as data so a vendored design system can swap the list wholesale.
#[derive(Clone, Copy, Debug)]
pub struct FontSpec {
    pub name: &'static str,
    pub file: &'static str,
    /// Face index inside a `.ttc` collection.
    pub index: u32,
    pub role: FontRole,
}

/// Loaded in order; the first of each role is the primary face.
/// Ships no font bytes in the binary: every Windows 10/11 install has these.
pub const FONTS: &[FontSpec] = &[
    FontSpec {
        name: "Segoe UI",
        file: "segoeui.ttf",
        index: 0,
        role: FontRole::Proportional,
    },
    FontSpec {
        name: "Consolas",
        file: "consola.ttf",
        index: 0,
        role: FontRole::Monospace,
    },
];

/// Loaded on demand the first time text needs glyphs the primary fonts lack
/// (symbols, CJK, emoji) — see [`request_fallback_fonts`]. Never loaded at startup.
pub const LAZY_FALLBACK_FONTS: &[FontSpec] = &[
    FontSpec {
        name: "Segoe UI Symbol",
        file: "seguisym.ttf",
        index: 0,
        role: FontRole::Fallback,
    },
    FontSpec {
        name: "Microsoft YaHei",
        file: "msyh.ttc",
        index: 0,
        role: FontRole::Fallback,
    },
    FontSpec {
        name: "Yu Gothic",
        file: "YuGothM.ttc",
        index: 0,
        role: FontRole::Fallback,
    },
    FontSpec {
        name: "Malgun Gothic",
        file: "malgun.ttf",
        index: 0,
        role: FontRole::Fallback,
    },
    FontSpec {
        name: "Nirmala UI",
        file: "Nirmala.ttc",
        index: 0,
        role: FontRole::Fallback,
    },
    FontSpec {
        name: "Segoe UI Emoji",
        file: "seguiemj.ttf",
        index: 0,
        role: FontRole::Fallback,
    },
];

static FALLBACK_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Segoe UI covers Latin, Greek, Cyrillic, Hebrew and Arabic; most scripts
/// beyond that (Indic, Thai, CJK, emoji, many symbols) need the lazy fallbacks.
pub fn needs_fallback_font(text: &str) -> bool {
    text.chars().any(|c| {
        (c as u32) >= 0x0900
            && !matches!(c, '\u{2000}'..='\u{206F}' | '\u{20A0}'..='\u{20CF}' | '\u{2100}'..='\u{214F}')
    })
}

/// Call with any user-provided text about to be displayed; cheap when ASCII.
#[inline]
pub fn note_text(text: &str) {
    if !text.is_ascii()
        && !FALLBACK_REQUESTED.load(std::sync::atomic::Ordering::Relaxed)
        && needs_fallback_font(text)
    {
        request_fallback_fonts();
    }
}

pub fn request_fallback_fonts() {
    FALLBACK_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The shell polls this once per frame.
pub fn fallback_fonts_requested() -> bool {
    FALLBACK_REQUESTED.load(std::sync::atomic::Ordering::Relaxed)
}
