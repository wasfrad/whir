//! The tab strip, which doubles as the window's title bar: tabs on the left, an
//! open button, empty draggable caption space, and min/max/close on the right.

use egui::epaint::CornerRadius;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, FontId, PointerButton, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use whir_core::DocKind;
use whir_core::theme::Theme;

pub struct TabInfo<'a> {
    pub id: u64,
    pub title: &'a str,
    pub tooltip: Option<String>,
    pub kind: DocKind,
    pub loading: bool,
}

#[derive(Default)]
pub struct TabBarOutput {
    pub activate: Option<usize>,
    pub close: Option<usize>,
    pub open_dialog: bool,
    pub minimize: bool,
    pub toggle_maximize: bool,
    pub close_window: bool,
    /// Rects (points) inside the strip that need client-area input.
    pub interactive: Vec<Rect>,
    /// The maximize button (points), reported to Windows as HTMAXBUTTON.
    pub maximize: Option<Rect>,
}

pub fn show(
    ui: &mut Ui,
    tabs: &[TabInfo<'_>],
    active: usize,
    maximized: bool,
    theme: &Theme,
) -> TabBarOutput {
    let m = theme.metrics;
    let strip = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(strip, 0.0, theme.strip_bg);
    let mut out = TabBarOutput::default();

    // --- caption buttons (right) ---
    let bw = m.caption_button_width;
    let close_rect =
        Rect::from_min_max(pos2(strip.right() - bw, strip.top()), strip.right_bottom());
    let max_rect = close_rect.translate(vec2(-bw, 0.0));
    let min_rect = max_rect.translate(vec2(-bw, 0.0));
    if caption_button(ui, min_rect, "min", theme, false, Glyph::Minimize).clicked() {
        out.minimize = true;
    }
    let glyph = if maximized {
        Glyph::Restore
    } else {
        Glyph::Maximize
    };
    if caption_button(ui, max_rect, "max", theme, false, glyph).clicked() {
        out.toggle_maximize = true;
    }
    if caption_button(ui, close_rect, "close", theme, true, Glyph::Close).clicked() {
        out.close_window = true;
    }
    out.interactive.push(min_rect);
    out.interactive.push(close_rect);
    out.maximize = Some(max_rect);

    // --- tabs (left) ---
    let top_gap = if maximized { 4.0 } else { 7.0 };
    let plus_size = 28.0;
    let left = strip.left() + 8.0;
    let tabs_right = min_rect.left() - m.drag_area_min_width - plus_size - 6.0;
    let avail = (tabs_right - left).max(0.0);
    let n = tabs.len().max(1) as f32;
    let tab_w = (avail / n - m.tab_gap).clamp(36.0, m.tab_max_width);
    let tab_h = strip.height() - top_gap;
    let text_color = |active: bool| {
        if active {
            theme.tab_text
        } else {
            theme.tab_text_inactive
        }
    };

    let mut x = left;
    let mut hovered_index = None;
    let mut rects = Vec::with_capacity(tabs.len());
    for (i, tab) in tabs.iter().enumerate() {
        let rect = Rect::from_min_size(pos2(x, strip.top() + top_gap), vec2(tab_w, tab_h));
        x += tab_w + m.tab_gap;
        if rect.left() >= tabs_right {
            break; // overflow: hidden (reachable via Ctrl+Tab)
        }
        let rect = rect.intersect(Rect::everything_left_of(tabs_right));
        rects.push(rect);
        let id = ui.id().with(("tab", tab.id));
        let resp = ui.interact(rect, id, Sense::CLICK);
        if resp.hovered() {
            hovered_index = Some(i);
        }
        let is_active = i == active;

        // Close button: registered after the tab so it wins hit-testing.
        let show_close = rect.width() >= 60.0 || is_active;
        let close_rect = Rect::from_center_size(
            pos2(rect.right() - 16.0, rect.center().y - 1.0),
            vec2(18.0, 18.0),
        );
        let close_resp = show_close.then(|| ui.interact(close_rect, id.with("x"), Sense::CLICK));
        let close_hovered = close_resp.as_ref().is_some_and(|r| r.hovered());

        if resp.hovered()
            && !close_hovered
            && ui.input(|inp| inp.pointer.button_pressed(PointerButton::Primary))
        {
            out.activate = Some(i);
        }
        if resp.clicked_by(PointerButton::Middle)
            || close_resp.as_ref().is_some_and(|r| r.clicked())
        {
            out.close = Some(i);
        }
        if let Some(tip) = &tab.tooltip {
            resp.clone().on_hover_text_at_pointer(tip);
        }

        // Paint.
        let r = m.corner_radius as u8;
        if is_active {
            let body = Rect::from_min_max(rect.min, pos2(rect.right(), strip.bottom()));
            painter.rect_filled(
                body,
                CornerRadius {
                    nw: r,
                    ne: r,
                    sw: 0,
                    se: 0,
                },
                theme.content_bg,
            );
        } else if resp.hovered() {
            painter.rect_filled(
                rect.shrink2(vec2(0.0, 2.0)).translate(vec2(0.0, -1.0)),
                r,
                theme.tab_hover_bg,
            );
        }
        let icon_center = pos2(rect.left() + 17.0, rect.center().y - 1.0);
        if rect.width() >= 44.0 || !show_close {
            if tab.loading {
                paint_spinner(
                    &painter,
                    icon_center,
                    6.0,
                    theme.accent,
                    ui.input(|i| i.time),
                );
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(33));
            } else {
                paint_kind_glyph(&painter, icon_center, tab.kind, theme);
            }
        }
        let text_left = rect.left() + 31.0;
        let text_right = if show_close {
            close_rect.left() - 4.0
        } else {
            rect.right() - 8.0
        };
        if text_right - text_left > 8.0 {
            whir_core::theme::note_text(tab.title);
            let mut job = LayoutJob::single_section(
                tab.title.to_owned(),
                TextFormat::simple(
                    FontId::proportional(m.body_text_size),
                    text_color(is_active),
                ),
            );
            job.wrap = TextWrapping {
                max_width: text_right - text_left,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = painter.layout_job(job);
            let pos = pos2(text_left, icon_center.y - galley.size().y / 2.0);
            painter
                .with_clip_rect(rect)
                .galley(pos, galley, theme.tab_text);
        }
        if let Some(cr) = &close_resp
            && (is_active || resp.hovered() || cr.hovered())
        {
            if cr.hovered() {
                let bg = if cr.is_pointer_button_down_on() {
                    theme.caption_pressed
                } else {
                    theme.caption_hover
                };
                painter.rect_filled(close_rect, 4.0, bg);
            }
            paint_glyph(
                &painter,
                close_rect.center(),
                Glyph::CloseSmall,
                text_color(is_active),
            );
        }
    }

    // Subtle separators between inactive, un-hovered neighbours.
    for (i, rect) in rects.iter().enumerate().skip(1) {
        let quiet = |j: usize| j != active && Some(j) != hovered_index;
        if quiet(i - 1) && quiet(i) {
            let x = painter.round_to_pixel_center(rect.left() - m.tab_gap / 2.0);
            let y = rect.center().y;
            painter.line_segment(
                [pos2(x, y - 8.0), pos2(x, y + 8.0)],
                Stroke::new(1.0, theme.separator),
            );
        }
    }
    out.interactive.extend(rects.iter().copied());

    // --- open button ---
    let plus_left = rects.last().map_or(left, |r| r.right() + m.tab_gap + 2.0);
    let plus = Rect::from_min_size(
        pos2(
            plus_left,
            strip.top() + top_gap + (tab_h - plus_size) / 2.0 - 1.0,
        ),
        vec2(plus_size, plus_size),
    );
    let plus_resp = ui.interact(plus, ui.id().with("open"), Sense::CLICK);
    if plus_resp.hovered() {
        painter.rect_filled(plus, 6.0, theme.tab_hover_bg);
    }
    paint_glyph(
        &painter,
        plus.center(),
        Glyph::Plus,
        theme.tab_text_inactive,
    );
    if plus_resp.clicked() {
        out.open_dialog = true;
    }
    plus_resp.on_hover_text("Open file (Ctrl+O)");
    out.interactive.push(plus);
    out
}

#[derive(Clone, Copy)]
enum Glyph {
    Minimize,
    Maximize,
    Restore,
    Close,
    CloseSmall,
    Plus,
}

fn caption_button(
    ui: &mut Ui,
    rect: Rect,
    id: &str,
    theme: &Theme,
    is_close: bool,
    glyph: Glyph,
) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(("caption", id)), Sense::CLICK);
    let (bg, fg) = if resp.is_pointer_button_down_on() {
        (
            Some(if is_close {
                theme.close_pressed
            } else {
                theme.caption_pressed
            }),
            if is_close {
                Color32::WHITE
            } else {
                theme.tab_text
            },
        )
    } else if resp.hovered() {
        (
            Some(if is_close {
                theme.close_hover
            } else {
                theme.caption_hover
            }),
            if is_close {
                Color32::WHITE
            } else {
                theme.tab_text
            },
        )
    } else {
        (None, theme.tab_text)
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, 0.0, bg);
    }
    paint_glyph(ui.painter(), rect.center(), glyph, fg);
    resp
}

/// Thin line icons drawn on the physical pixel grid (1 device pixel strokes).
fn paint_glyph(painter: &egui::Painter, center: Pos2, glyph: Glyph, color: Color32) {
    let ppp = painter.pixels_per_point();
    let px = 1.0 / ppp;
    let stroke = Stroke::new(px.max(1.0 / ppp), color);
    let snap = |v: f32| painter.round_to_pixel_center(v);
    let c = pos2(snap(center.x), snap(center.y));
    match glyph {
        Glyph::Minimize => {
            painter.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], stroke);
        }
        Glyph::Maximize => {
            let r = Rect::from_center_size(c, vec2(10.0, 10.0));
            painter.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Middle);
        }
        Glyph::Restore => {
            let back = Rect::from_center_size(c + vec2(2.0, -2.0), vec2(8.0, 8.0));
            let front = Rect::from_center_size(c + vec2(-1.0, 1.0), vec2(8.0, 8.0));
            painter.line_segment([pos2(back.left(), front.top()), back.left_top()], stroke);
            painter.line_segment([back.left_top(), back.right_top()], stroke);
            painter.line_segment([back.right_top(), back.right_bottom()], stroke);
            painter.line_segment(
                [back.right_bottom(), pos2(front.right(), back.bottom())],
                stroke,
            );
            painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Middle);
        }
        Glyph::Close => {
            let d = 5.0;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], stroke);
            painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], stroke);
        }
        Glyph::CloseSmall => {
            let d = 3.75;
            let s = Stroke::new(1.25, color);
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], s);
            painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], s);
        }
        Glyph::Plus => {
            let d = 6.0;
            let s = Stroke::new(1.25, color);
            painter.line_segment([c + vec2(-d, 0.0), c + vec2(d, 0.0)], s);
            painter.line_segment([c + vec2(0.0, -d), c + vec2(0.0, d)], s);
        }
    }
}

fn paint_kind_glyph(painter: &egui::Painter, c: Pos2, kind: DocKind, theme: &Theme) {
    let color = match kind {
        DocKind::Image => Color32::from_rgb(0x4c, 0x9a, 0xff),
        DocKind::Pdf => Color32::from_rgb(0xf0, 0x55, 0x4a),
        DocKind::Csv => Color32::from_rgb(0x3c, 0xb3, 0x71),
        DocKind::Note => theme.note_glyph,
        DocKind::Error => theme.text_dim,
    };
    let s = Stroke::new(1.3, color);
    match kind {
        DocKind::Image => {
            let r = Rect::from_center_size(c, vec2(13.0, 11.0));
            painter.rect_stroke(r, 2.0, s, egui::StrokeKind::Middle);
            painter.line_segment(
                [
                    pos2(r.left() + 1.5, r.bottom() - 2.0),
                    pos2(r.left() + 5.0, r.top() + 5.0),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.left() + 5.0, r.top() + 5.0),
                    pos2(r.left() + 8.0, r.bottom() - 3.5),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.left() + 7.5, r.bottom() - 3.0),
                    pos2(r.left() + 9.5, r.top() + 6.5),
                ],
                s,
            );
            painter.line_segment(
                [
                    pos2(r.left() + 9.5, r.top() + 6.5),
                    pos2(r.right() - 1.5, r.bottom() - 2.0),
                ],
                s,
            );
            painter.circle_filled(pos2(r.right() - 3.5, r.top() + 3.5), 1.3, color);
        }
        DocKind::Pdf => {
            let r = Rect::from_center_size(c, vec2(10.0, 13.0));
            let fold = 3.5;
            let pts = [
                r.left_top(),
                pos2(r.right() - fold, r.top()),
                pos2(r.right(), r.top() + fold),
                r.right_bottom(),
                r.left_bottom(),
                r.left_top(),
            ];
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], s);
            }
            for dy in [5.5, 8.0, 10.5] {
                painter.line_segment(
                    [
                        pos2(r.left() + 2.5, r.top() + dy),
                        pos2(r.right() - 2.5, r.top() + dy),
                    ],
                    Stroke::new(1.0, color),
                );
            }
        }
        DocKind::Csv => {
            let r = Rect::from_center_size(c, vec2(13.0, 12.0));
            painter.rect_stroke(r, 1.5, s, egui::StrokeKind::Middle);
            let thin = Stroke::new(1.0, color);
            painter.line_segment(
                [
                    pos2(r.left(), r.top() + 4.0),
                    pos2(r.right(), r.top() + 4.0),
                ],
                thin,
            );
            painter.line_segment(
                [
                    pos2(r.left(), r.top() + 8.0),
                    pos2(r.right(), r.top() + 8.0),
                ],
                thin,
            );
            painter.line_segment(
                [
                    pos2(r.left() + 5.0, r.top()),
                    pos2(r.left() + 5.0, r.bottom()),
                ],
                thin,
            );
        }
        DocKind::Note => {
            // A page with its bottom-right corner folded, and three text lines.
            let r = Rect::from_center_size(c, vec2(11.0, 13.0));
            let fold = 3.5;
            let pts = [
                r.left_top(),
                r.right_top(),
                pos2(r.right(), r.bottom() - fold),
                pos2(r.right() - fold, r.bottom()),
                r.left_bottom(),
                r.left_top(),
            ];
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], s);
            }
            painter.line_segment(
                [
                    pos2(r.right() - fold, r.bottom()),
                    pos2(r.right() - fold, r.bottom() - fold),
                ],
                Stroke::new(1.0, color),
            );
            painter.line_segment(
                [
                    pos2(r.right() - fold, r.bottom() - fold),
                    pos2(r.right(), r.bottom() - fold),
                ],
                Stroke::new(1.0, color),
            );
            for (dy, w) in [(3.5, 6.0), (6.0, 6.0), (8.5, 3.5)] {
                painter.line_segment(
                    [
                        pos2(r.left() + 2.5, r.top() + dy),
                        pos2(r.left() + 2.5 + w, r.top() + dy),
                    ],
                    Stroke::new(1.0, color),
                );
            }
        }
        DocKind::Error => {
            painter.circle_stroke(c, 6.0, s);
            painter.text(
                c,
                Align2::CENTER_CENTER,
                "!",
                FontId::proportional(10.0),
                color,
            );
        }
    }
}

fn paint_spinner(painter: &egui::Painter, c: Pos2, radius: f32, color: Color32, time: f64) {
    let start = (time * 5.0) as f32;
    let n = 20;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = start + i as f32 / n as f32 * std::f32::consts::TAU * 0.7;
            c + radius * vec2(a.cos(), a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.6, color)));
}
