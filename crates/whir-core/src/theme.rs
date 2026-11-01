//! Design tokens and font setup.
//!
//! Everything visual that isn't egui's own widget styling lives here, so a future
//! vendored design system only has to replace this module.

pub use egui::Color32;
use egui::Stroke;
use egui::epaint::Shadow;
use egui::style::WidgetVisuals;

use crate::settings;

/// Which theme whir draws in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// As Windows' apps are.
    #[default]
    System,
    Light,
    Dark,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::System, Mode::Light, Mode::Dark];
    /// In `settings.ini`.
    const KEY: &str = "theme";

    fn name(self) -> &'static str {
        match self {
            Mode::System => "system",
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
    }

    fn from_name(name: &str) -> Mode {
        let same = |m: &Mode| m.name().eq_ignore_ascii_case(name.trim());
        Mode::ALL.into_iter().find(same).unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::System => "System",
            Mode::Light => "Light",
            Mode::Dark => "Dark",
        }
    }

    pub fn saved() -> Mode {
        settings::get_str(Mode::KEY).map_or(Mode::default(), |s| Mode::from_name(&s))
    }

    /// As `settings.ini` has it now, for the dock: the file is the window's,
    /// so what the dock read of it when it started says nothing.
    pub fn on_disk() -> Mode {
        let file = settings::IniFile::load(settings::app_data_file("settings.ini"));
        file.get(Mode::KEY).map_or(Mode::default(), Mode::from_name)
    }

    /// Remember this; `after` runs once it's on disk.
    pub fn save(self, after: impl FnOnce() + Send + 'static) {
        settings::set_str_then(Mode::KEY, self.name(), after);
    }
}

/// How big a note's text is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoteSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl NoteSize {
    pub const ALL: [NoteSize; 3] = [NoteSize::Small, NoteSize::Medium, NoteSize::Large];
    /// In `settings.ini`.
    const KEY: &str = "note.size";

    fn name(self) -> &'static str {
        match self {
            NoteSize::Small => "small",
            NoteSize::Medium => "medium",
            NoteSize::Large => "large",
        }
    }

    fn from_name(name: &str) -> NoteSize {
        let same = |s: &NoteSize| s.name().eq_ignore_ascii_case(name.trim());
        NoteSize::ALL.into_iter().find(same).unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        match self {
            NoteSize::Small => "Small",
            NoteSize::Medium => "Medium",
            NoteSize::Large => "Large",
        }
    }

    pub fn saved() -> NoteSize {
        settings::get_str(NoteSize::KEY).map_or(NoteSize::default(), |s| NoteSize::from_name(&s))
    }

    pub fn save(self) {
        settings::set_str(NoteSize::KEY, self.name());
    }

    /// `m`'s note text at this size, to the half point.
    fn scale(self, m: &mut Metrics) {
        let by = match self {
            NoteSize::Small => 0.9,
            NoteSize::Medium => return,
            NoteSize::Large => 1.15,
        };
        for size in [
            &mut m.note_title_size,
            &mut m.note_body_size,
            &mut m.note_code_size,
        ] {
            *size = (*size * by * 2.0).round() / 2.0;
        }
    }
}

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
    /// An outline that has to be seen: popups, fields, cards.
    pub border_strong: Color32,
    /// Lines, icons and text in the accent colour.
    pub accent: Color32,
    /// Filled with the accent colour (the one thing to do), and what's written on it.
    pub accent_fill: Color32,
    pub accent_fill_hover: Color32,
    pub on_accent: Color32,

    /// Menus, tooltips and other popups.
    pub surface_raised: Color32,
    /// Buttons at rest, hovered, pressed.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_pressed: Color32,
    /// The well of a text field.
    pub field_bg: Color32,
    /// Behind a page of Settings, and the cards on it.
    pub page_bg: Color32,
    pub card_bg: Color32,

    pub caption_hover: Color32,
    pub caption_pressed: Color32,
    pub close_hover: Color32,
    pub close_pressed: Color32,

    pub text: Color32,
    pub text_dim: Color32,
    /// Hints and things switched off: fine to go unread.
    pub text_faint: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,
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
    /// Tab / drawer glyph for each kind of document.
    pub note_glyph: Color32,
    pub image_glyph: Color32,
    pub pdf_glyph: Color32,
    pub csv_glyph: Color32,
    /// A milestone's name beside its project's (the capture dock's time).
    pub milestone: Color32,

    pub metrics: Metrics,
}

/// Sizes in egui points.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// The type scale: everything outside a note's text is one of these.
    pub small_text_size: f32,
    pub body_text_size: f32,
    pub title_text_size: f32,
    pub display_text_size: f32,
    /// Tabs, popups, cards.
    pub corner_radius: f32,
    /// Buttons, fields, rows.
    pub control_radius: f32,
    /// Height of buttons and fields.
    pub control_height: f32,
    /// The square a toolbar button takes up.
    pub tool_size: f32,
    pub icon_size: f32,

    /// Notes: widest text column, side/top padding, type sizes.
    pub note_column_max: f32,
    pub note_side_pad: f32,
    pub note_top_pad: f32,
    pub note_title_size: f32,
    pub note_body_size: f32,
    /// Line height as a multiple of the body size.
    pub note_line_factor: f32,
    pub note_code_size: f32,

    /// The drawer along the bottom: its header (all of it, folded), and a
    /// row of its notes list.
    pub drawer_header_h: f32,
    pub drawer_row_h: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            tab_strip_height: 36.0,
            toolbar_height: 36.0,
            tab_min_width: 112.0,
            tab_max_width: 232.0,
            tab_gap: 2.0,
            caption_button_width: 46.0,
            drag_area_min_width: 48.0,
            small_text_size: 11.0,
            body_text_size: 13.0,
            title_text_size: 16.0,
            display_text_size: 20.0,
            corner_radius: 8.0,
            control_radius: 4.0,
            control_height: 24.0,
            tool_size: 28.0,
            icon_size: 16.0,
            note_column_max: 720.0,
            note_side_pad: 32.0,
            note_top_pad: 44.0,
            note_title_size: 26.0,
            note_body_size: 15.0,
            note_line_factor: 1.55,
            note_code_size: 13.5,
            drawer_header_h: 30.0,
            drawer_row_h: 30.0,
        }
    }
}

impl Theme {
    /// The theme to draw in, as whoever's asking found `dark`, at the note
    /// text size picked.
    pub fn chosen(dark: bool) -> Self {
        let mut theme = if dark { Theme::dark() } else { Theme::light() };
        NoteSize::saved().scale(&mut theme.metrics);
        theme
    }

    pub fn dark() -> Self {
        let rgb = Color32::from_rgb;
        let accent = rgb(0x5b, 0x9b, 0xff);
        let tint = |alpha| Color32::from_rgba_unmultiplied(0x5b, 0x9b, 0xff, alpha);
        Self {
            dark: true,
            content_bg: rgb(0x26, 0x26, 0x28),
            strip_bg: rgb(0x1b, 0x1b, 0x1d),
            tab_hover_bg: rgb(0x2a, 0x2a, 0x2d),
            tab_text: rgb(0xeb, 0xeb, 0xec),
            tab_text_inactive: rgb(0xa2, 0xa2, 0xa6),
            separator: rgb(0x39, 0x39, 0x3c),
            border_strong: rgb(0x4c, 0x4c, 0x51),
            accent,
            accent_fill: rgb(0x2f, 0x6f, 0xed),
            accent_fill_hover: rgb(0x3f, 0x7d, 0xf5),
            on_accent: Color32::WHITE,
            surface_raised: rgb(0x2f, 0x2f, 0x32),
            control: rgb(0x33, 0x33, 0x36),
            control_hover: rgb(0x3b, 0x3b, 0x3f),
            control_pressed: rgb(0x2c, 0x2c, 0x2f),
            field_bg: rgb(0x1e, 0x1e, 0x20),
            page_bg: rgb(0x20, 0x20, 0x22),
            card_bg: rgb(0x2b, 0x2b, 0x2e),
            caption_hover: rgb(0x2e, 0x2e, 0x31),
            caption_pressed: rgb(0x38, 0x38, 0x3b),
            close_hover: rgb(0xc4, 0x2b, 0x1c),
            close_pressed: rgb(0xa8, 0x26, 0x1a),
            text: rgb(0xeb, 0xeb, 0xec),
            text_dim: rgb(0xa8, 0xa8, 0xac),
            text_faint: rgb(0x7c, 0x7c, 0x81),
            error: rgb(0xff, 0x6b, 0x6b),
            warning: rgb(0xf2, 0xb1, 0x3d),
            success: rgb(0x4c, 0xc3, 0x84),
            selection_bg: Color32::from_rgba_premultiplied(0x1d, 0x3a, 0x6e, 0xc0),
            selection_stroke: accent,
            highlight_bg: Color32::from_rgba_premultiplied(0x6e, 0x5a, 0x10, 0xc0),
            checker_a: rgb(0x3a, 0x3a, 0x3e),
            checker_b: rgb(0x2e, 0x2e, 0x32),
            canvas_bg: rgb(0x18, 0x18, 0x1a),
            grid_line: rgb(0x34, 0x34, 0x37),
            header_bg: rgb(0x2d, 0x2d, 0x30),
            gutter_bg: rgb(0x21, 0x21, 0x23),
            overlay_bg: Color32::from_rgba_premultiplied(0x2b, 0x2b, 0x2e, 0xf4),
            page_selection: Color32::from_rgba_unmultiplied(0x3b, 0x82, 0xf6, 0x5c),
            page_match: Color32::from_rgba_unmultiplied(0xff, 0xd4, 0x00, 0x66),
            page_match_current: Color32::from_rgba_unmultiplied(0xff, 0x8a, 0x00, 0x8c),
            page_match_outline: rgb(0xd9, 0x6c, 0x00),
            edited_bg: tint(0x22),
            edited_mark: accent,
            md_syntax: rgb(0x6c, 0x6c, 0x72),
            md_list_marker: rgb(0x7f, 0xa6, 0xf0),
            md_code_bg: rgb(0x32, 0x32, 0x36),
            md_code_block_bg: rgb(0x1f, 0x1f, 0x21),
            md_quote_bar: rgb(0x4c, 0x4c, 0x51),
            md_quote_text: rgb(0xb6, 0xb6, 0xbb),
            md_link: rgb(0x72, 0xa8, 0xff),
            md_task_done: rgb(0x80, 0x80, 0x86),
            md_rule: rgb(0x3e, 0x3e, 0x43),
            drawer_bg: rgb(0x20, 0x20, 0x22),
            drawer_border: rgb(0x39, 0x39, 0x3c),
            drawer_shadow: Color32::from_black_alpha(0x70),
            drawer_item_hover: rgb(0x2b, 0x2b, 0x2e),
            drawer_item_active: tint(0x2c),
            drawer_grip: rgb(0x70, 0x70, 0x76),
            minimap_bg: rgb(0x21, 0x21, 0x23),
            minimap_slider: tint(0x2c),
            minimap_slider_active: tint(0x48),
            minimap_slider_stroke: tint(0xb0),
            modal_backdrop: Color32::from_black_alpha(0x80),
            danger: rgb(0xff, 0x6b, 0x6b),
            note_glyph: rgb(0xe8, 0xa3, 0x3d),
            image_glyph: rgb(0x5b, 0xa4, 0xf5),
            pdf_glyph: rgb(0xf0, 0x60, 0x55),
            csv_glyph: rgb(0x4c, 0xc3, 0x84),
            milestone: rgb(0xe8, 0xa3, 0x3d),
            metrics: Metrics::default(),
        }
    }

    pub fn light() -> Self {
        let rgb = Color32::from_rgb;
        let accent = rgb(0x25, 0x63, 0xeb);
        let tint = |alpha| Color32::from_rgba_unmultiplied(0x25, 0x63, 0xeb, alpha);
        Self {
            dark: false,
            content_bg: rgb(0xfc, 0xfc, 0xfd),
            strip_bg: rgb(0xec, 0xec, 0xee),
            tab_hover_bg: rgb(0xe0, 0xe0, 0xe3),
            tab_text: rgb(0x1a, 0x1a, 0x1c),
            tab_text_inactive: rgb(0x5a, 0x5a, 0x60),
            separator: rgb(0xde, 0xde, 0xe1),
            border_strong: rgb(0xc4, 0xc4, 0xc9),
            accent,
            accent_fill: accent,
            accent_fill_hover: rgb(0x1d, 0x4f, 0xd8),
            on_accent: Color32::WHITE,
            surface_raised: rgb(0xff, 0xff, 0xff),
            control: rgb(0xf0, 0xf0, 0xf2),
            control_hover: rgb(0xe6, 0xe6, 0xe9),
            control_pressed: rgb(0xdc, 0xdc, 0xe0),
            field_bg: rgb(0xff, 0xff, 0xff),
            page_bg: rgb(0xf5, 0xf5, 0xf6),
            card_bg: rgb(0xff, 0xff, 0xff),
            caption_hover: rgb(0xdc, 0xdc, 0xdf),
            caption_pressed: rgb(0xcf, 0xcf, 0xd3),
            close_hover: rgb(0xc4, 0x2b, 0x1c),
            close_pressed: rgb(0xa8, 0x26, 0x1a),
            text: rgb(0x1a, 0x1a, 0x1c),
            text_dim: rgb(0x5e, 0x5e, 0x64),
            text_faint: rgb(0x8c, 0x8c, 0x92),
            error: rgb(0xc0, 0x1c, 0x28),
            warning: rgb(0xa8, 0x5f, 0x00),
            success: rgb(0x16, 0x7f, 0x4a),
            selection_bg: Color32::from_rgba_premultiplied(0xb4, 0xcc, 0xf4, 0xe0),
            selection_stroke: accent,
            highlight_bg: Color32::from_rgba_premultiplied(0xf0, 0xd8, 0x60, 0xe0),
            checker_a: rgb(0xff, 0xff, 0xff),
            checker_b: rgb(0xe4, 0xe4, 0xe8),
            canvas_bg: rgb(0xe4, 0xe4, 0xe7),
            grid_line: rgb(0xe4, 0xe4, 0xe7),
            header_bg: rgb(0xf2, 0xf2, 0xf4),
            gutter_bg: rgb(0xf6, 0xf6, 0xf7),
            overlay_bg: Color32::from_rgba_premultiplied(0xfb, 0xfb, 0xfc, 0xf6),
            page_selection: Color32::from_rgba_unmultiplied(0x3b, 0x82, 0xf6, 0x5c),
            page_match: Color32::from_rgba_unmultiplied(0xff, 0xd4, 0x00, 0x66),
            page_match_current: Color32::from_rgba_unmultiplied(0xff, 0x8a, 0x00, 0x8c),
            page_match_outline: rgb(0xd9, 0x6c, 0x00),
            edited_bg: tint(0x1a),
            edited_mark: accent,
            md_syntax: rgb(0xa2, 0xa2, 0xa8),
            md_list_marker: rgb(0x3a, 0x6c, 0xd8),
            md_code_bg: rgb(0xee, 0xee, 0xf1),
            md_code_block_bg: rgb(0xf4, 0xf4, 0xf6),
            md_quote_bar: rgb(0xcf, 0xcf, 0xd5),
            md_quote_text: rgb(0x4a, 0x4a, 0x50),
            md_link: accent,
            md_task_done: rgb(0x8a, 0x8a, 0x90),
            md_rule: rgb(0xd8, 0xd8, 0xdd),
            drawer_bg: rgb(0xf5, 0xf5, 0xf6),
            drawer_border: rgb(0xde, 0xde, 0xe1),
            drawer_shadow: Color32::from_black_alpha(0x2c),
            drawer_item_hover: rgb(0xe9, 0xe9, 0xec),
            drawer_item_active: tint(0x1f),
            drawer_grip: rgb(0x9a, 0x9a, 0xa0),
            minimap_bg: rgb(0xee, 0xee, 0xf0),
            minimap_slider: tint(0x22),
            minimap_slider_active: tint(0x3a),
            minimap_slider_stroke: tint(0xa0),
            modal_backdrop: Color32::from_black_alpha(0x50),
            danger: rgb(0xc0, 0x1c, 0x28),
            note_glyph: rgb(0xc9, 0x7c, 0x10),
            image_glyph: rgb(0x1d, 0x6f, 0xd8),
            pdf_glyph: rgb(0xd4, 0x38, 0x2c),
            csv_glyph: rgb(0x16, 0x8f, 0x57),
            // Darker than the glyph: small text on white needs the contrast.
            milestone: rgb(0xa8, 0x5f, 0x00),
            metrics: Metrics::default(),
        }
    }

    /// What's under anything floating over the window.
    pub fn popup_shadow(&self) -> Shadow {
        Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(if self.dark { 110 } else { 45 }),
        }
    }

    /// Apply to egui's own widget styling: every stock button, field, menu and
    /// tooltip gets its look from here.
    pub fn apply(&self, ctx: &egui::Context) {
        let m = self.metrics;
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = self.content_bg;
        visuals.window_fill = self.surface_raised;
        visuals.window_stroke = Stroke::new(1.0, self.border_strong);
        visuals.window_corner_radius = (m.corner_radius as u8).into();
        visuals.menu_corner_radius = (m.corner_radius as u8).into();
        visuals.popup_shadow = self.popup_shadow();
        visuals.window_shadow = Shadow {
            offset: [0, 8],
            blur: 28,
            ..self.popup_shadow()
        };

        visuals.extreme_bg_color = self.canvas_bg;
        visuals.text_edit_bg_color = Some(self.field_bg);
        visuals.faint_bg_color = self.gutter_bg;
        visuals.code_bg_color = self.md_code_bg;
        visuals.selection.bg_fill = self.selection_bg;
        visuals.selection.stroke = Stroke::new(1.0, self.selection_stroke);
        visuals.hyperlink_color = self.md_link;
        visuals.weak_text_color = Some(self.text_dim);
        visuals.warn_fg_color = self.warning;
        visuals.error_fg_color = self.error;
        visuals.indent_has_left_vline = false;
        visuals.image_loading_spinners = false;
        visuals.disabled_alpha = 0.45;

        let corner_radius = (m.control_radius as u8).into();
        let widget = |fill: Color32, stroke: Color32, text: Color32| WidgetVisuals {
            bg_fill: fill,
            weak_bg_fill: fill,
            bg_stroke: Stroke::new(1.0, stroke),
            corner_radius,
            fg_stroke: Stroke::new(1.0, text),
            expansion: 0.0,
        };
        let w = &mut visuals.widgets;
        w.noninteractive = widget(self.content_bg, self.separator, self.text);
        w.inactive = widget(self.control, self.separator, self.text);
        w.hovered = widget(self.control_hover, self.border_strong, self.text);
        w.active = widget(self.control_pressed, self.border_strong, self.text);
        w.open = widget(self.control_hover, self.border_strong, self.text);

        let theme = if self.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        // The caret keeps time with Windows, and stops blinking when idle: that
        // isn't the theme's, and must outlive a change of theme.
        let caret = ctx.style_of(theme).visuals.text_cursor.clone();
        visuals.text_cursor.blink = caret.blink;
        visuals.text_cursor.on_duration = caret.on_duration;
        visuals.text_cursor.off_duration = caret.off_duration;
        ctx.set_visuals_of(theme, visuals);
        ctx.set_theme(theme);
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
                    FontId::new(m.title_text_size, family(SEMIBOLD)),
                ),
                (
                    TextStyle::Monospace,
                    FontId::new(m.body_text_size, FontFamily::Monospace),
                ),
            ]
            .into();
            let s = &mut style.spacing;
            s.item_spacing = egui::vec2(8.0, 3.0);
            s.button_padding = egui::vec2(10.0, 3.0);
            s.interact_size = egui::vec2(40.0, m.control_height);
            s.menu_margin = egui::Margin::same(4);
            s.menu_spacing = 0.0;
            s.indent = 14.0;
            s.icon_width = 16.0;
            s.icon_width_inner = 10.0;
            s.icon_spacing = 6.0;
            s.tooltip_width = 360.0;
            s.scroll.bar_width = 8.0;
            s.scroll.floating_width = 3.0;
            s.scroll.bar_outer_margin = 2.0;
            style.interaction.tooltip_delay = 0.35;
            style.animation_time = 0.09;
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
/// The icon font: see [`crate::icons`].
pub const ICONS: &str = "whir-icons";
/// Text marks on images: see [`crate::marks::font`].
pub const MARKS: &str = "whir-marks";

/// `(family, is_monospace)` for every named family.
pub const NAMED_FAMILIES: &[(&str, bool)] = &[
    (SEMIBOLD, false),
    (BOLD, false),
    (MONO_BOLD, true),
    (ICONS, false),
    (MARKS, false),
];

pub fn family(name: &'static str) -> egui::FontFamily {
    egui::FontFamily::Name(name.into())
}

/// egui's own fonts with every named family bound to them, for what doesn't
/// load whir's (tests): using a named family would otherwise panic.
pub fn test_fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let body = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for &(name, _) in NAMED_FAMILIES {
        fonts.families.insert(family(name), body.clone());
    }
    fonts
}

/// A context with [`test_fonts`].
pub fn test_context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.set_fonts(test_fonts());
    ctx
}

/// A font the shell loads from `%WINDIR%\Fonts` at startup.
/// Described as data so a vendored design system can swap the list wholesale.
#[derive(Clone, Copy, Debug)]
pub struct FontSpec {
    pub name: &'static str,
    pub file: &'static str,
    /// Used when `file` isn't there (older Windows). Takes no `axes`.
    pub alternate: Option<&'static str>,
    /// Face index inside a `.ttc` collection.
    pub index: u32,
    pub role: FontRole,
    /// Where a variable font sits on its axes.
    pub axes: &'static [(&'static [u8; 4], f32)],
    /// Grid-fit the outlines. An unhinted font costs nothing to set up.
    pub hinted: bool,
    /// Glyphs moved down by this fraction of the font's size.
    pub drop: f32,
}

impl FontSpec {
    const fn new(name: &'static str, file: &'static str, role: FontRole) -> Self {
        Self {
            name,
            file,
            alternate: None,
            index: 0,
            role,
            axes: &[],
            hinted: true,
            drop: 0.0,
        }
    }

    const fn or(self, alternate: &'static str) -> Self {
        Self {
            alternate: Some(alternate),
            ..self
        }
    }

    const fn at(self, axes: &'static [(&'static [u8; 4], f32)]) -> Self {
        Self {
            axes,
            hinted: false,
            ..self
        }
    }

    const fn unhinted(self) -> Self {
        Self {
            hinted: false,
            ..self
        }
    }

    const fn dropped(self, drop: f32) -> Self {
        Self { drop, ..self }
    }
}

/// Loaded in order; the first of each role is the primary face.
/// Ships no font bytes in the binary: Windows 11 has all of these, and every
/// Windows 10 install has the alternates.
pub const FONTS: &[FontSpec] = &[
    // Grayscale text reads thin, so the regular face sits a little above 400.
    FontSpec::new("Segoe UI Variable", "SegUIVar.ttf", FontRole::Proportional)
        .or("segoeui.ttf")
        .at(&[(b"wght", 450.0), (b"opsz", 10.5)]),
    FontSpec::new("Consolas", "consola.ttf", FontRole::Monospace),
    FontSpec::new(
        "Segoe UI Variable Semibold",
        "SegUIVar.ttf",
        FontRole::Family(SEMIBOLD),
    )
    .or("seguisb.ttf")
    .at(&[(b"wght", 600.0), (b"opsz", 14.0)]),
    FontSpec::new(
        "Segoe Fluent Icons",
        "SegoeIcons.ttf",
        FontRole::Family(ICONS),
    )
    .or("segmdl2.ttf")
    .unhinted()
    // Its glyphs sit high in the line, against the text beside them.
    .dropped(0.08),
];

/// Faces loaded the first time they're used: bold in a note, text on an image.
pub const LAZY_RICH_FONTS: &[FontSpec] = &[
    FontSpec::new("Segoe UI Bold", "segoeuib.ttf", FontRole::Family(BOLD)),
    FontSpec::new("Consolas Bold", "consolab.ttf", FontRole::Family(MONO_BOLD)),
    FontSpec::new("Segoe UI", "segoeui.ttf", FontRole::Family(MARKS)),
];

/// Loaded on demand the first time text needs glyphs the primary fonts lack
/// (symbols, CJK, emoji) -- see [`request_fallback_fonts`]. Never loaded at startup.
pub const LAZY_FALLBACK_FONTS: &[FontSpec] = &[
    FontSpec::new("Segoe UI Symbol", "seguisym.ttf", FontRole::Fallback),
    FontSpec::new("Microsoft YaHei", "msyh.ttc", FontRole::Fallback),
    FontSpec::new("Yu Gothic", "YuGothM.ttc", FontRole::Fallback),
    FontSpec::new("Malgun Gothic", "malgun.ttf", FontRole::Fallback),
    FontSpec::new("Nirmala UI", "Nirmala.ttc", FontRole::Fallback),
    FontSpec::new("Segoe UI Emoji", "seguiemj.ttf", FontRole::Fallback),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_are_read_back_as_they_were_written() {
        for mode in Mode::ALL {
            assert_eq!(Mode::from_name(mode.name()), mode);
        }
        for size in NoteSize::ALL {
            assert_eq!(NoteSize::from_name(size.name()), size);
        }
        assert_eq!(Mode::from_name(" DARK "), Mode::Dark);
    }

    #[test]
    fn what_cant_be_read_is_the_default() {
        assert_eq!(Mode::from_name("mauve"), Mode::System);
        assert_eq!(NoteSize::from_name(""), NoteSize::Medium);
    }

    #[test]
    fn note_sizes_step_up_from_small_to_large() {
        let body = |size: NoteSize| {
            let mut m = Metrics::default();
            size.scale(&mut m);
            m.note_body_size
        };
        assert_eq!(body(NoteSize::Medium), Metrics::default().note_body_size);
        assert!(body(NoteSize::Small) < body(NoteSize::Medium));
        assert!(body(NoteSize::Large) > body(NoteSize::Medium));
    }

    #[test]
    fn only_a_notes_text_changes_size() {
        let mut m = Metrics::default();
        NoteSize::Large.scale(&mut m);
        let d = Metrics::default();
        assert_eq!(m.body_text_size, d.body_text_size);
        assert_eq!(m.note_column_max, d.note_column_max);
    }

    #[test]
    fn a_change_of_theme_leaves_the_caret_keeping_windows_time() {
        let ctx = test_context();
        Theme::dark().apply(&ctx);
        ctx.all_styles_mut(|s| {
            s.visuals.text_cursor.on_duration = 0.25;
            s.visuals.text_cursor.off_duration = 0.75;
            s.visuals.text_cursor.blink = false;
        });
        Theme::light().apply(&ctx);
        Theme::dark().apply(&ctx);
        let caret = ctx.style_of(egui::Theme::Dark).visuals.text_cursor.clone();
        assert_eq!(caret.on_duration, 0.25);
        assert_eq!(caret.off_duration, 0.75);
        assert!(!caret.blink);
    }
}
