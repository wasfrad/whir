//! How whir looks: its theme, and how big a note's text is.

use egui::{Align2, CornerRadius, FontId, Painter, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use whir_core::kit;
use whir_core::theme::{Mode, NoteSize, Theme};
use whir_core::{DocCx, DocRequest};

/// Between the theme's tiles.
const GAP: f32 = 10.0;
const TILE_MAX: f32 = 150.0;
/// A tile's picture, as wide as it is high by this much.
const PICTURE: f32 = 0.62;
const LABEL_H: f32 = 28.0;

pub fn ui(ui: &mut egui::Ui, cx: &mut DocCx<'_>, theme: &Theme) {
    kit::page_header(ui, "Appearance", "", theme, |_| {});
    let mode = Mode::saved();
    kit::group(ui, "Theme", theme, |g| {
        g.row(|ui| {
            let width = ((ui.available_width() - 2.0 * GAP) / 3.0).min(TILE_MAX);
            ui.spacing_mut().item_spacing.x = GAP;
            ui.horizontal(|ui| {
                for m in Mode::ALL {
                    if tile(ui, m, m == mode, width, theme) && m != mode {
                        cx.requests.push(DocRequest::Theme(m));
                    }
                }
            });
        });
    });
    kit::group(ui, "Notes", theme, |g| {
        if let Some(size) = g.choice(
            "Text size",
            "How notes are shown here. The files don't change.",
            NoteSize::saved(),
            &NoteSize::ALL,
            |s| s.label().to_owned(),
        ) {
            cx.requests.push(DocRequest::NoteSize(size));
        }
    });
}

/// A theme to pick: a little window drawn in its colours, with its name under
/// it. True when clicked.
fn tile(ui: &mut egui::Ui, mode: Mode, on: bool, width: f32, theme: &Theme) -> bool {
    let height = (width * PICTURE).round();
    let size = vec2(width, height + LABEL_H);
    let (rect, response) = ui.allocate_exact_size(size, Sense::CLICK);
    let picture = Rect::from_min_size(rect.min, vec2(width, height));
    let painter = ui.painter();
    match mode {
        Mode::Light => window(painter, picture, &Theme::light()),
        Mode::Dark => window(painter, picture, &Theme::dark()),
        Mode::System => {
            let middle = picture.center().x;
            let (mut left, mut right) = (picture, picture);
            left.max.x = middle;
            right.min.x = middle;
            window(&painter.with_clip_rect(left), picture, &Theme::light());
            window(&painter.with_clip_rect(right), picture, &Theme::dark());
        }
    }
    let radius = theme.metrics.corner_radius;
    let edge = if on {
        Stroke::new(2.0, theme.accent)
    } else if response.hovered() {
        Stroke::new(1.0, theme.text_dim)
    } else {
        Stroke::new(1.0, theme.border_strong)
    };
    painter.rect_stroke(picture, radius, edge, StrokeKind::Outside);
    let color = if on { theme.text } else { theme.text_dim };
    painter.text(
        pos2(rect.left() + 2.0, rect.bottom() - LABEL_H / 2.0 + 2.0),
        Align2::LEFT_CENTER,
        mode.label(),
        FontId::proportional(theme.metrics.body_text_size),
        color,
    );
    response.clicked()
}

/// A whir window, small: tabs, a toolbar's worth of text, and a button.
fn window(painter: &Painter, r: Rect, t: &Theme) {
    let radius = t.metrics.corner_radius;
    let unit = r.height() / 80.0;
    painter.rect_filled(r, radius, t.content_bg);
    let strip = Rect::from_min_size(r.min, vec2(r.width(), 14.0 * unit));
    let top = CornerRadius {
        nw: radius as u8,
        ne: radius as u8,
        sw: 0,
        se: 0,
    };
    painter.rect_filled(strip, top, t.strip_bg);
    let tab = Rect::from_min_size(
        r.min + vec2(7.0 * unit, 4.0 * unit),
        vec2(r.width() * 0.34, 10.0 * unit),
    );
    painter.rect_filled(tab, 2.0 * unit, t.content_bg);
    let line = t.text_dim.gamma_multiply(0.55);
    for (i, share) in [0.7, 0.56, 0.64].into_iter().enumerate() {
        let at = r.min + vec2(9.0 * unit, (26.0 + i as f32 * 10.0) * unit);
        let bar = Rect::from_min_size(at, vec2(r.width() * share, 3.0 * unit));
        painter.rect_filled(bar, 1.5 * unit, line);
    }
    let button = Rect::from_min_size(
        r.min + vec2(9.0 * unit, 60.0 * unit),
        vec2(r.width() * 0.26, 10.0 * unit),
    );
    painter.rect_filled(button, 2.0 * unit, t.accent_fill);
}
