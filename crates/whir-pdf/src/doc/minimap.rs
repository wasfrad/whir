//! The minimap: the document in miniature in a column beside the scrollbar,
//! a slider over what's on screen, and the page controls above it
//! (previous, next, go to page). Like VS Code's, it scrolls in proportion to
//! the document, so the slider never leaves it.

use std::ops::Range;
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange};
use egui::{Align2, Color32, FontId, Key, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use whir_core::icons::{self, Icon};
use whir_core::scroll::ScrollAxis;
use whir_core::theme::Theme;

use super::{BAR, Cached, PdfDoc, page_px_at};
use crate::engine::{Job, TileKey};
use crate::layout::{Layout, MARGIN};
use crate::mini_layout::MiniLayout;
use crate::prefs;
use crate::view::View;

/// Column width (the scrollbar strip sits to its right).
pub(super) const WIDTH: f32 = 120.0;
/// Thumbnail width of the widest page.
pub(super) const THUMB_W: f64 = 96.0;
/// Narrower windows hide the minimap: the pages need the room.
pub(super) const MIN_WINDOW: f32 = 560.0;
/// GPU memory for thumbnails, per document.
pub(super) const THUMB_BUDGET: u64 = 24 << 20;
/// The page controls above the thumbnails.
const HEADER_H: f32 = 32.0;
/// Thumbnails asked for beyond the ones in view, each way.
const AHEAD: usize = 2;
/// Thumbnails asked for at once, at most.
const MAX_JOBS: usize = 64;

pub(super) fn enabled() -> bool {
    prefs::minimap()
}

pub(super) fn toggle() {
    prefs::set_minimap(!enabled());
}

/// The document's rect → the page area, and the panel at its right (the
/// minimap column, then the scrollbar strip).
pub(super) fn split(full: Rect) -> (Rect, Rect) {
    let x = full.right() - WIDTH - BAR;
    (
        Rect::from_min_max(full.min, pos2(x, full.bottom())),
        Rect::from_min_max(pos2(x, full.top()), full.max),
    )
}

/// Page `i`'s thumbnail at this frame's device scale: always one whole-page job.
pub(super) fn thumb_job(m: &MiniLayout, l: &Layout, i: usize, ppp: f64) -> Job {
    let scale = ((m.scale(i) * ppp * 1000.0).round() as u32).max(1);
    let page_px = page_px_at(l.sizes[i], scale as f64 / 1000.0);
    Job {
        key: TileKey {
            page: i as u32,
            scale,
            tx: 0,
            ty: 0,
            thumb: true,
        },
        rect: [0, 0, page_px[0], page_px[1]],
        page_px,
    }
}

fn goto_id() -> egui::Id {
    egui::Id::new("whir.pdf.goto")
}

/// This frame's placement of the minimap.
#[derive(Clone)]
pub(super) struct Placement {
    pub(super) mini: Arc<MiniLayout>,
    /// Column and scrollbar strip.
    panel: Rect,
    pub(super) header: Rect,
    /// Where the thumbnails show.
    pub(super) area: Rect,
    /// Left edge of the widest thumbnail (the column is centred in `area`).
    left: f32,
    /// How far the thumbnails are scrolled (minimap points).
    offset: f64,
}

impl Placement {
    pub(super) fn new(mini: Arc<MiniLayout>, panel: Rect, sy: &ScrollAxis) -> Self {
        let column = Rect::from_min_max(panel.min, pos2(panel.right() - BAR, panel.bottom()));
        let header = Rect::from_min_size(column.min, vec2(column.width(), HEADER_H));
        let area = Rect::from_min_max(pos2(column.left(), header.bottom()), column.max);
        let max = sy.max_offset();
        let t = if max > 0.0 { sy.offset / max } else { 0.0 };
        let offset = mini.offset(area.height() as f64, t);
        Self {
            left: area.left() + ((area.width() as f64 - THUMB_W) / 2.0) as f32,
            mini,
            panel,
            header,
            area,
            offset,
        }
    }

    /// Minimap y → screen y.
    fn y(&self, m: f64) -> f32 {
        self.area.top() + (m - self.offset) as f32
    }

    /// Screen y → minimap y.
    fn m(&self, y: f32) -> f64 {
        (y - self.area.top()) as f64 + self.offset
    }

    /// Pages whose thumbnails are in view.
    fn in_view(&self) -> Range<usize> {
        self.mini
            .visible(self.offset, self.offset + self.area.height() as f64)
    }

    /// Minimap x of document x (PDF points, margins included).
    fn x(&self, doc_x: f64) -> f32 {
        self.left + ((doc_x - MARGIN) * self.mini.base()) as f32
    }

    pub(super) fn thumb_rect(&self, l: &Layout, i: usize, ppp: f64) -> Rect {
        let [x, y, w, h] = self.mini.thumb(l, i);
        let snap = |v: f64| ((v * ppp).round() / ppp) as f32;
        Rect::from_min_size(
            pos2(
                snap(self.left as f64 + x),
                snap(self.area.top() as f64 + y - self.offset),
            ),
            vec2(w as f32, h as f32),
        )
    }

    /// The viewport slider: what's on screen, across the column (or just the
    /// part shown, zoomed in past the width).
    pub(super) fn slider(&self, sy: &ScrollAxis, sx: &ScrollAxis, zoom: f64, x_off: f64) -> Rect {
        let m = &self.mini;
        let top = self.y(m.doc_to_map(sy.offset / zoom));
        let bottom = self.y(m.doc_to_map((sy.offset + sy.viewport) / zoom));
        let (mut x0, mut x1) = (self.area.left() + 4.0, self.area.right() - 4.0);
        if sx.is_scrollable() {
            x0 = self.x((sx.offset - x_off) / zoom).max(x0);
            x1 = self.x((sx.offset + sx.viewport - x_off) / zoom).min(x1);
        }
        Rect::from_min_max(pos2(x0, top), pos2(x1.max(x0 + 2.0), bottom.max(top + 2.0)))
    }
}

impl PdfDoc {
    /// Thumbnails the minimap needs, nearest the current page first.
    pub(super) fn minimap_jobs(
        &self,
        place: &Placement,
        l: &Layout,
        current: usize,
        ppp: f64,
        jobs: &mut Vec<Job>,
    ) {
        let r = place.in_view();
        let r = r.start.saturating_sub(AHEAD)..(r.end + AHEAD).min(l.len());
        let mut pages: Vec<usize> = r.collect();
        pages.sort_by_key(|&p| p.abs_diff(current));
        for p in pages.into_iter().take(MAX_JOBS) {
            let job = thumb_job(&place.mini, l, p, ppp);
            if self.needs_thumb(job.key) && !jobs.iter().any(|j| j.key == job.key) {
                jobs.push(job);
            }
        }
    }

    /// Not cached at this scale, and PDFium hasn't failed at it.
    pub(super) fn needs_thumb(&self, key: TileKey) -> bool {
        !self.thumbs.contains_key(&key) && !self.failed_tiles.contains(&key)
    }

    /// A thumbnail of `key.page`: at `key`'s scale if it's here, else any
    /// scale still cached (after a DPI change, until the new one arrives).
    pub(super) fn thumb_of(&mut self, key: TileKey) -> Option<&mut Cached> {
        if self.thumbs.contains_key(&key) {
            return self.thumbs.get_mut(&key);
        }
        self.thumbs
            .iter_mut()
            .find(|(k, _)| k.page == key.page)
            .map(|(_, c)| c)
    }

    /// Scroll so `page` sits at the top of the viewport.
    pub(super) fn go_to_page(&mut self, l: &Layout, page: usize, zoom: f64) {
        if !l.is_empty() {
            self.sy.scroll_to(l.landing(page.min(l.len() - 1)) * zoom);
        }
    }

    pub(super) fn step_page(&mut self, l: &Layout, zoom: f64, forward: bool) {
        let page = l.page_step(self.sy.offset / zoom, forward);
        self.go_to_page(l, page, zoom);
    }

    /// The go-to-page field has keyboard focus (the document leaves keys alone).
    pub(super) fn goto_has_focus(&self, ctx: &egui::Context) -> bool {
        self.goto.is_some() && ctx.memory(|m| m.has_focus(goto_id()))
    }

    /// Open the go-to-page field, `current` (0-based) in it, selected.
    pub(super) fn open_goto(&mut self, current: usize) {
        self.goto = Some((current + 1).to_string());
        self.goto_focus_pending = true;
    }

    /// Paint the minimap and handle its input. Returns true if it moved the view.
    pub(super) fn show_minimap(
        &mut self,
        ui: &mut egui::Ui,
        place: &Placement,
        l: &Layout,
        v: View,
        current: usize,
        frame: u64,
    ) -> bool {
        let theme = Theme::current();
        let (zoom, ppp) = (v.zoom, v.ppp);
        let before = (self.sy.offset, self.sx.offset);
        let panel = ui.painter().with_clip_rect(place.panel);
        panel.rect_filled(place.panel, 0.0, theme.minimap_bg);
        panel.rect_filled(
            Rect::from_min_max(
                place.panel.min,
                pos2(place.panel.left() + 1.0, place.panel.bottom()),
            ),
            0.0,
            theme.separator,
        );

        // --- input ---------------------------------------------------------------------
        let id = ui.id().with("pdf.minimap");
        let resp = ui.interact(place.area, id, Sense::CLICK | Sense::DRAG);
        let slider = place.slider(&self.sy, &self.sx, zoom, v.x_off);
        if resp.hovered() {
            let d = ui.input(|i| i.smooth_scroll_delta);
            if d.y != 0.0 {
                self.sy.scroll_by(-d.y as f64);
            }
        }
        // Dragging: keep hold of the slider where it was grabbed, or (grabbed
        // elsewhere) centre it on the pointer.
        let grab_id = id.with("grab");
        if resp.drag_started() {
            let origin = ui.input(|i| i.pointer.press_origin());
            if let Some(p) = origin.or(resp.interact_pointer_pos()) {
                let grab = if slider.contains(p) {
                    p - slider.min
                } else {
                    slider.size() / 2.0
                };
                ui.data_mut(|d| d.insert_temp(grab_id, grab));
            }
        }
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let grab: Vec2 = ui
                .data(|d| d.get_temp(grab_id))
                .unwrap_or(slider.size() / 2.0);
            let target = (p.y - grab.y - place.area.top()) as f64;
            let area_h = place.area.height() as f64;
            let max = self.sy.max_offset();
            let off = place.mini.offset_for_slider(area_h, max, zoom, target);
            self.sy.scroll_to(off);
            if self.sx.is_scrollable() {
                let doc_x = (p.x - grab.x - place.left) as f64 / place.mini.base() + MARGIN;
                self.sx.scroll_to(doc_x * zoom + v.x_off);
            }
        } else if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let page = place.mini.page_at(place.m(p.y));
            self.go_to_page(l, page, zoom);
        }
        let mut moved = self.header(ui, place, l, zoom, current, &theme);
        moved |= (self.sy.offset, self.sx.offset) != before;
        // Draw where the view is now, not where it was.
        let place = if moved {
            &Placement::new(place.mini.clone(), place.panel, &self.sy)
        } else {
            place
        };

        // --- thumbnails, slider --------------------------------------------------------
        let p = ui.painter().with_clip_rect(place.area);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let hovered = resp
            .hover_pos()
            .filter(|_| !resp.dragged())
            .map(|h| place.mini.page_at(place.m(h.y)));
        let matches = self.find.open;
        for i in place.in_view() {
            let r = place.thumb_rect(l, i, ppp);
            p.rect_filled(r.expand(1.0), 0.0, theme.separator);
            p.rect_filled(r, 0.0, Color32::WHITE);
            let key = thumb_job(&place.mini, l, i, ppp).key;
            if let Some(c) = self.thumb_of(key) {
                p.image(c.texture.id(), r, uv, Color32::WHITE);
                c.last_used = frame;
            }
            if matches && !self.search.matches_on(i as u32).is_empty() {
                let tick = Rect::from_min_max(
                    pos2(r.right() + 3.0, r.top()),
                    pos2(r.right() + 5.0, r.bottom()),
                );
                p.rect_filled(tick, 1.0, theme.page_match_outline);
            }
            if i == current {
                p.rect_stroke(r, 0.0, Stroke::new(1.0, theme.accent), StrokeKind::Outside);
            } else if hovered == Some(i) {
                p.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.0, theme.text_dim),
                    StrokeKind::Outside,
                );
            }
            if hovered == Some(i) {
                let galley = p.layout_no_wrap(
                    format!("{}", i + 1),
                    FontId::proportional(theme.metrics.small_text_size),
                    theme.text,
                );
                let size = galley.size() + vec2(10.0, 4.0);
                let pill = Rect::from_center_size(
                    pos2(
                        r.center().x,
                        (r.bottom() - 4.0 - size.y / 2.0).max(r.top() + size.y / 2.0),
                    ),
                    size,
                );
                p.rect_filled(pill, size.y / 2.0, theme.overlay_bg);
                p.galley(pill.min + vec2(5.0, 2.0), galley, theme.text);
            }
        }
        let slider = place.slider(&self.sy, &self.sx, zoom, v.x_off);
        let active = resp.hovered() || resp.dragged();
        let fill = if active {
            theme.minimap_slider_active
        } else {
            theme.minimap_slider
        };
        p.rect_filled(slider, 2.0, fill);
        p.rect_stroke(
            slider,
            2.0,
            Stroke::new(1.0, theme.minimap_slider_stroke),
            StrokeKind::Inside,
        );
        moved
    }

    /// Previous / next page buttons and the page counter (or, while it's
    /// open, the go-to-page field). Returns true if it moved the view.
    fn header(
        &mut self,
        ui: &mut egui::Ui,
        place: &Placement,
        l: &Layout,
        zoom: f64,
        current: usize,
        theme: &Theme,
    ) -> bool {
        let (prev, next, counter) = header_rects(place.header);
        let id = ui.id().with("pdf.minimap.header");
        let p = ui.painter().with_clip_rect(place.header);
        let at_top = self.sy.offset <= 0.5;
        let at_end = self.sy.offset >= self.sy.max_offset() - 0.5;
        let mut moved = false;
        let buttons = [
            (prev, false, "Previous page (Ctrl+↑)"),
            (next, true, "Next page (Ctrl+↓)"),
        ];
        for (rect, forward, tip) in buttons {
            let enabled = if forward { !at_end } else { !at_top };
            let sense = if enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let resp = ui
                .interact(rect, id.with(forward), sense)
                .on_hover_text(tip);
            if resp.clicked() {
                self.step_page(l, zoom, forward);
                moved = true;
            }
            paint_chevron(&p, &resp, rect, enabled, !forward, theme);
        }
        if self.goto.is_some() {
            moved |= self.goto_field(ui, counter, l, zoom);
        } else {
            let resp = ui
                .interact(counter, id.with("counter"), Sense::click())
                .on_hover_text("Go to page (Ctrl+G)");
            if resp.hovered() {
                p.rect_filled(counter, 4.0, theme.tab_hover_bg);
            }
            let n = l.len();
            let font = FontId::proportional(theme.metrics.small_text_size);
            let mut text = format!("{} / {n}", current + 1);
            if p.layout_no_wrap(text.clone(), font.clone(), theme.text)
                .size()
                .x
                > counter.width()
            {
                text = format!("{}/{n}", current + 1);
            }
            p.text(
                counter.center(),
                Align2::CENTER_CENTER,
                text,
                font,
                theme.text,
            );
            if resp.clicked() {
                self.open_goto(current);
            }
        }
        p.rect_filled(
            Rect::from_min_max(
                pos2(place.header.left(), place.header.bottom() - 1.0),
                place.header.max,
            ),
            0.0,
            theme.separator,
        );
        moved
    }

    /// The go-to-page field in `rect`: Enter goes, Esc or a click elsewhere
    /// cancels. Returns true if it moved the view.
    pub(super) fn goto_field(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        l: &Layout,
        zoom: f64,
    ) -> bool {
        let Some(mut text) = self.goto.take() else {
            return false;
        };
        let theme = Theme::current();
        let id = goto_id();
        if self.goto_focus_pending {
            self.goto_focus_pending = false;
            ui.memory_mut(|m| m.request_focus(id));
            let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            let n = text.chars().count();
            state
                .cursor
                .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(n))));
            state.store(ui.ctx(), id);
        }
        let n = l.len();
        let font = FontId::proportional(theme.metrics.small_text_size);
        let label = format!("/ {n}");
        let label_w = ui
            .painter()
            .layout_no_wrap(label.clone(), font.clone(), theme.text_dim)
            .size()
            .x;
        let field = Rect::from_min_max(
            rect.min,
            pos2(
                (rect.right() - label_w - 4.0).max(rect.left() + 24.0),
                rect.bottom(),
            ),
        );
        let edit = egui::TextEdit::singleline(&mut text)
            .id(id)
            .font(font.clone())
            .char_limit(7)
            .horizontal_align(egui::Align::Center)
            .margin(vec2(2.0, 2.0))
            .desired_width(field.width());
        let resp = ui.put(field, edit);
        ui.painter().text(
            pos2(rect.right(), rect.center().y),
            Align2::RIGHT_CENTER,
            label,
            font,
            theme.text_dim,
        );
        text.retain(|c| c.is_ascii_digit());
        if resp.lost_focus() {
            // Closed either way; Enter also goes.
            if ui.input(|i| i.key_pressed(Key::Enter))
                && let Ok(page) = text.parse::<usize>()
                && n > 0
            {
                self.go_to_page(l, page.clamp(1, n) - 1, zoom);
                return true;
            }
            return false;
        }
        self.goto = Some(text);
        false
    }
}

/// Previous button, next button, page counter.
pub(super) fn header_rects(header: Rect) -> (Rect, Rect, Rect) {
    let size = vec2(22.0, 24.0);
    let prev = Rect::from_center_size(pos2(header.left() + 16.0, header.center().y - 0.5), size);
    let next = Rect::from_center_size(pos2(header.right() - 16.0, header.center().y - 0.5), size);
    let counter = Rect::from_min_max(
        pos2(prev.right() + 2.0, prev.top()),
        pos2(next.left() - 2.0, prev.bottom()),
    );
    (prev, next, counter)
}

/// A chevron button's look: hover and press backgrounds, dimmed when disabled.
fn paint_chevron(
    p: &egui::Painter,
    resp: &egui::Response,
    rect: Rect,
    enabled: bool,
    up: bool,
    theme: &Theme,
) {
    if enabled && resp.hovered() {
        let pressed = resp.is_pointer_button_down_on();
        let bg = if pressed {
            theme.caption_pressed
        } else {
            theme.tab_hover_bg
        };
        p.rect_filled(rect, 4.0, bg);
    }
    let color = if enabled {
        theme.text
    } else {
        theme.text_dim.gamma_multiply(0.5)
    };
    let icon = if up {
        Icon::ChevronUp
    } else {
        Icon::ChevronDown
    };
    icons::paint(p, rect.center(), icon, 10.0, color);
}
