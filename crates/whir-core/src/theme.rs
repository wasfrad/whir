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

    /// On-paper colours: PDF pages are white in both themes.
    pub page_selection: Color32,
    pub page_match: Color32,
    pub page_match_current: Color32,
    pub page_match_outline: Color32,

    /// Unsaved edits in a table.
    pub edited_bg: Color32,
    pub edited_mark: Color32,

    /// Markdown styling in notes.
    pub md_syntax: Color32,
    pub md_list_marker: Color32,
    pub md_code_bg: Color32,
    pub md_code_block_bg: Color32,
    pub md_quote_bar: Color32,
    pub md_quote_text: Color32,
    pub md_link: Color32,
    pub md_task_done: Color32,
    pub md_rule: Color32,

    /// The notes drawer.
    pub drawer_bg: Color32,
    pub drawer_border: Color32,
    pub drawer_shadow: Color32,
    pub drawer_item_hover: Color32,
    pub drawer_item_active: Color32,
    pub drawer_grip: Color32,

    /// The PDF minimap: its column, and the slider over what's on screen
    /// (tinted, so it shows over white pages).
    pub minimap_bg: Color32,
    pub minimap_slider: Color32,
    pub minimap_slider_active: Color32,
    pub minimap_slider_stroke: Color32,

    pub modal_backdrop: Color32,
    pub danger: Color32,
    /// Tab / drawer glyph for notes.
    pub note_glyph: Color32,

    pub metrics: Metrics,
}

/// Sizes in egui points.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub tab_strip_height: f32,
    /// The strip of tools between the tabs and the document.
    pub toolbar_height: f32,
    pub tab_min_width: f32,
    pub tab_max_width: f32,
    pub tab_gap: f32,
    pub caption_button_width: f32,
    /// Empty strip area always kept draggable to the left of the caption buttons.
    pub drag_area_min_width: f32,
    pub body_text_size: f32,
    pub small_text_size: f32,
    pub corner_radius: f32,

    /// Notes: widest text column, side/top padding, type sizes.
    pub note_column_max: f32,
    pub note_side_pad: f32,
    pub note_top_pad: f32,
    pub note_title_size: f32,
    pub note_body_size: f32,
    /// Line height as a multiple of the body size.
    pub note_line_factor: f32,
    pub note_code_size: f32,

    /// Drawer: open width, peek strip width, edge trigger band, list row height.
    pub drawer_width: f32,
    pub drawer_strip: f32,
    pub drawer_trigger: f32,
    pub drawer_row_h: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            tab_strip_height: 36.0,
            toolbar_height: 32.0,
            tab_min_width: 72.0,
            tab_max_width: 232.0,
            tab_gap: 2.0,
            caption_button_width: 46.0,
            drag_area_min_width: 48.0,
            body_text_size: 13.0,
            small_text_size: 11.5,
            corner_radius: 6.0,
            note_column_max: 720.0,
            note_side_pad: 32.0,
            note_top_pad: 44.0,
            note_title_size: 26.0,
            note_body_size: 15.0,
            note_line_factor: 1.55,
            note_code_size: 13.5,
            drawer_width: 300.0,
            drawer_strip: 14.0,
            drawer_trigger: 6.0,
            drawer_row_h: 52.0,
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
            page_selection: Color32::from_rgba_unmultiplied(0x3b, 0x82, 0xf6, 0x5c),
            page_match: Color32::from_rgba_unmultiplied(0xff, 0xd4, 0x00, 0x66),
            page_match_current: Color32::from_rgba_unmultiplied(0xff, 0x8a, 0x00, 0x8c),
            page_match_outline: Color32::from_rgb(0xd9, 0x6c, 0x00),
            edited_bg: Color32::from_rgba_unmultiplied(0x4c, 0x8d, 0xff, 0x22),
            edited_mark: Color32::from_rgb(0x4c, 0x8d, 0xff),
            md_syntax: Color32::from_rgb(0x6c, 0x6c, 0x76),
            md_list_marker: Color32::from_rgb(0x7f, 0xa6, 0xf0),
            md_code_bg: Color32::from_rgb(0x30, 0x30, 0x36),
            md_code_block_bg: Color32::from_rgb(0x1e, 0x1e, 0x22),
            md_quote_bar: Color32::from_rgb(0x4a, 0x4a, 0x52),
            md_quote_text: Color32::from_rgb(0xb4, 0xb4, 0xbc),
            md_link: Color32::from_rgb(0x6f, 0xa3, 0xff),
            md_task_done: Color32::from_rgb(0x80, 0x80, 0x88),
            md_rule: Color32::from_rgb(0x3e, 0x3e, 0x46),
            drawer_bg: Color32::from_rgb(0x1f, 0x1f, 0x23),
            drawer_border: Color32::from_rgb(0x38, 0x38, 0x3e),
            drawer_shadow: Color32::from_black_alpha(0x70),
            drawer_item_hover: Color32::from_rgb(0x2a, 0x2a, 0x30),
            drawer_item_active: Color32::from_rgba_unmultiplied(0x4c, 0x8d, 0xff, 0x2e),
            drawer_grip: Color32::from_rgb(0x70, 0x70, 0x7a),
            minimap_bg: Color32::from_rgb(0x21, 0x21, 0x25),
            minimap_slider: Color32::from_rgba_unmultiplied(0x4c, 0x8d, 0xff, 0x2c),
            minimap_slider_active: Color32::from_rgba_unmultiplied(0x4c, 0x8d, 0xff, 0x48),
            minimap_slider_stroke: Color32::from_rgba_unmultiplied(0x4c, 0x8d, 0xff, 0xb0),
            modal_backdrop: Color32::from_black_alpha(0x80),
            danger: Color32::from_rgb(0xff, 0x6b, 0x6b),
            note_glyph: Color32::from_rgb(0xe8, 0xa3, 0x3d),
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
            page_selection: Color32::from_rgba_unmultiplied(0x3b, 0x82, 0xf6, 0x5c),
            page_match: Color32::from_rgba_unmultiplied(0xff, 0xd4, 0x00, 0x66),
            page_match_current: Color32::from_rgba_unmultiplied(0xff, 0x8a, 0x00, 0x8c),
            page_match_outline: Color32::from_rgb(0xd9, 0x6c, 0x00),
            edited_bg: Color32::from_rgba_unmultiplied(0x25, 0x63, 0xeb, 0x1a),
            edited_mark: Color32::from_rgb(0x25, 0x63, 0xeb),
            md_syntax: Color32::from_rgb(0xa2, 0xa2, 0xaa),
            md_list_marker: Color32::from_rgb(0x3a, 0x6c, 0xd8),
            md_code_bg: Color32::from_rgb(0xec, 0xec, 0xf0),
            md_code_block_bg: Color32::from_rgb(0xf2, 0xf2, 0xf5),
            md_quote_bar: Color32::from_rgb(0xcf, 0xcf, 0xd6),
            md_quote_text: Color32::from_rgb(0x4a, 0x4a, 0x52),
            md_link: Color32::from_rgb(0x25, 0x63, 0xeb),
            md_task_done: Color32::from_rgb(0x8a, 0x8a, 0x92),
            md_rule: Color32::from_rgb(0xd6, 0xd6, 0xdc),
            drawer_bg: Color32::from_rgb(0xf5, 0xf5, 0xf7),
            drawer_border: Color32::from_rgb(0xd4, 0xd4, 0xda),
            drawer_shadow: Color32::from_black_alpha(0x2c),
            drawer_item_hover: Color32::from_rgb(0xe7, 0xe7, 0xeb),
            drawer_item_active: Color32::from_rgba_unmultiplied(0x25, 0x63, 0xeb, 0x1f),
            drawer_grip: Color32::from_rgb(0x9a, 0x9a, 0xa2),
            minimap_bg: Color32::from_rgb(0xec, 0xec, 0xef),
            minimap_slider: Color32::from_rgba_unmultiplied(0x25, 0x63, 0xeb, 0x22),
            minimap_slider_active: Color32::from_rgba_unmultiplied(0x25, 0x63, 0xeb, 0x3a),
            minimap_slider_stroke: Color32::from_rgba_unmultiplied(0x25, 0x63, 0xeb, 0xa0),
            modal_backdrop: Color32::from_black_alpha(0x50),
            danger: Color32::from_rgb(0xc0, 0x1c, 0x28),
            note_glyph: Color32::from_rgb(0xd9, 0x8a, 0x1c),
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
    /// Primary face of a named family (bold, italic, ...).
    Family(&'static str),
}

/// Named font families for rich text (notes). All are registered at startup,
/// aliased to the regular faces, so using one never panics; their real faces
/// load on demand ([`request_rich_fonts`]), except semibold which titles need at once.
/// (Italics use egui's synthetic slant, so they need no font of their own.)
pub const SEMIBOLD: &str = "whir-semibold";
pub const BOLD: &str = "whir-bold";
pub const MONO_BOLD: &str = "whir-mono-bold";

/// `(family, is_monospace)` for every named family.
pub const NAMED_FAMILIES: &[(&str, bool)] = &[(SEMIBOLD, false), (BOLD, false), (MONO_BOLD, true)];

pub fn family(name: &'static str) -> egui::FontFamily {
    egui::FontFamily::Name(name.into())
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
    FontSpec {
        name: "Segoe UI Semibold",
        file: "seguisb.ttf",
        index: 0,
        role: FontRole::Family(SEMIBOLD),
    },
];

/// Bold faces, loaded the first time a note uses them.
pub const LAZY_RICH_FONTS: &[FontSpec] = &[
    FontSpec {
        name: "Segoe UI Bold",
        file: "segoeuib.ttf",
        index: 0,
        role: FontRole::Family(BOLD),
    },
    FontSpec {
        name: "Consolas Bold",
        file: "consolab.ttf",
        index: 0,
        role: FontRole::Family(MONO_BOLD),
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

static RICH_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Text styled bold/italic is about to be shown: load [`LAZY_RICH_FONTS`].
pub fn request_rich_fonts() {
    RICH_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The shell polls this once per frame.
pub fn rich_fonts_requested() -> bool {
    RICH_REQUESTED.load(std::sync::atomic::Ordering::Relaxed)
}
