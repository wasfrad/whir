//! The PDF tab: continuous scroll, fit-width, zoom at the cursor, tiles at
//! exact device resolution (stretched while a zoom settles), LRU texture budget.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use egui::{Color32, CursorIcon, Key, Modifiers, Pos2, Rect, Sense, Stroke, pos2, vec2};
use whir_core::scroll::{ScrollAxis, scrollbar};
use whir_core::theme::Theme;
use whir_core::{DocCx, DocKind, Document, OpenCx, TextureRegistry, UserTexture};

use crate::engine::{DocHandle, Event, Job, TileKey};
use crate::layout::{self, Layout};

/// GPU memory for rendered tiles, per document.
const TEXTURE_BUDGET: u64 = 96 << 20;
/// Hold off re-rendering while a zoom gesture is in progress.
const ZOOM_SETTLE: f64 = 0.15;
/// UI points per PDF point at "100%": 1 pt = 1/72 in, 1 UI point = 1/96 in.
const ZOOM_100: f64 = 96.0 / 72.0;
const MIN_ZOOM: f64 = 0.1;
const MAX_ZOOM: f64 = 12.0 * ZOOM_100;

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    FitWidth,
    Manual(f64),
}

enum State {
    Loading,
    Ready(Layout),
    Failed(String),
}

struct Cached {
    texture: UserTexture,
    last_used: u64,
}

pub struct PdfDoc {
    path: PathBuf,
    title: String,
    handle: DocHandle,
    rx: Receiver<Event>,
    textures: std::sync::Arc<dyn TextureRegistry>,
    state: State,
    tiles: HashMap<TileKey, Cached>,
    /// Tiles PDFium couldn't produce: never requested again.
    failed_tiles: std::collections::HashSet<TileKey>,
    got_first_tile: bool,
    zoom: Zoom,
    sy: ScrollAxis,
    sx: ScrollAxis,
    /// Scale key tiles are currently requested at (lags zoom while it settles).
    settled_scale: Option<u32>,
    zoom_changed_at: f64,
    last_wanted: Vec<TileKey>,
    frame: u64,
    toast: Option<(String, f64)>,
    last_scroll: f64,
}

impl PdfDoc {
    pub fn open(path: PathBuf, cx: &OpenCx) -> Self {
        let (tx, rx) = channel();
        let handle = DocHandle::open(path.clone(), tx, cx.waker.clone(), cx.textures.clone());
        Self {
            title: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            path,
            handle,
            rx,
            textures: cx.textures.clone(),
            state: State::Loading,
            tiles: HashMap::new(),
            failed_tiles: std::collections::HashSet::new(),
            got_first_tile: false,
            zoom: Zoom::FitWidth,
            sy: ScrollAxis::default(),
            sx: ScrollAxis::default(),
            settled_scale: None,
            zoom_changed_at: f64::NEG_INFINITY,
            last_wanted: Vec::new(),
            frame: 0,
            toast: None,
            last_scroll: 0.0,
        }
    }

    fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Opened { pages, .. } => self.state = State::Ready(Layout::new(pages)),
                Event::Text { .. } => {}
                Event::Failed(e) => self.state = State::Failed(e),
                Event::Tile { key, texture } => {
                    self.got_first_tile = true;
                    self.tiles.insert(
                        key,
                        Cached {
                            texture,
                            last_used: self.frame,
                        },
                    );
                }
                Event::TileFailed { key } => {
                    // Leave that area blank rather than retrying forever.
                    self.got_first_tile = true;
                    self.failed_tiles.insert(key);
                }
            }
        }
    }

    fn evict(&mut self, keep_frame: u64) {
        let bytes = |c: &Cached| {
            let [w, h] = c.texture.size();
            w as u64 * h as u64 * 4
        };
        let mut total: u64 = self.tiles.values().map(bytes).sum();
        if total <= TEXTURE_BUDGET {
            return;
        }
        let mut by_age: Vec<(u64, TileKey, u64)> = self
            .tiles
            .iter()
            .filter(|(_, c)| c.last_used < keep_frame)
            .map(|(k, c)| (c.last_used, *k, bytes(c)))
            .collect();
        by_age.sort_unstable_by_key(|e| e.0);
        for (_, key, b) in by_age {
            if total <= TEXTURE_BUDGET {
                break;
            }
            self.tiles.remove(&key);
            total -= b;
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let State::Ready(layout) = &self.state else {
            return;
        };
        let theme = Theme::current();
        let ppp = ui.ctx().pixels_per_point() as f64;
        let now = ui.input(|i| i.time);
        self.frame += 1;

        let (vp, resp) = ui.allocate_exact_size(ui.available_size(), Sense::CLICK | Sense::DRAG);
        let painter = ui.painter().with_clip_rect(vp);
        painter.rect_filled(vp, 0.0, theme.canvas_bg);
        if layout.is_empty() {
            return;
        }
        let vw = vp.width() as f64;
        let vh = vp.height() as f64;

        // --- zoom & scroll -----------------------------------------------------
        let mut zoom = match self.zoom {
            Zoom::FitWidth => layout.fit_width_zoom(vw - 14.0),
            Zoom::Manual(z) => z,
        };
        let set_axes = |sy: &mut ScrollAxis, sx: &mut ScrollAxis, zoom: f64| {
            sy.content = layout.total_height * zoom;
            sy.viewport = vh;
            sx.content = layout.content_width() * zoom;
            sx.viewport = vw;
            sy.clamp();
            sx.clamp();
        };
        set_axes(&mut self.sy, &mut self.sx, zoom);

        let (scroll, zoom_delta, hover) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.zoom_delta() as f64,
                i.pointer.hover_pos(),
            )
        });
        let mut new_zoom = None;
        if resp.hovered() {
            if zoom_delta != 1.0 {
                new_zoom = Some((zoom * zoom_delta, hover));
            } else if scroll != egui::Vec2::ZERO {
                self.sy.scroll_by(-scroll.y as f64);
                self.sx.scroll_by(-scroll.x as f64);
            }
        }
        if resp.dragged() {
            let d = resp.drag_delta();
            self.sy.scroll_by(-d.y as f64);
            self.sx.scroll_by(-d.x as f64);
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if resp.hovered() && (self.sy.is_scrollable() || self.sx.is_scrollable()) {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }

        let page_step = (vh * 0.9).max(40.0);
        let mut fit = false;
        ui.input_mut(|i| {
            let none = Modifiers::NONE;
            // Shift+Space first: consume_key ignores extra Shift, so plain Space would eat it.
            if i.consume_key(none, Key::PageUp) || i.consume_key(Modifiers::SHIFT, Key::Space) {
                self.sy.scroll_by(-page_step);
            }
            if i.consume_key(none, Key::PageDown) || i.consume_key(none, Key::Space) {
                self.sy.scroll_by(page_step);
            }
            if i.consume_key(none, Key::ArrowDown) {
                self.sy.scroll_by(48.0);
            }
            if i.consume_key(none, Key::ArrowUp) {
                self.sy.scroll_by(-48.0);
            }
            if i.consume_key(none, Key::ArrowRight) {
                self.sx.scroll_by(48.0);
            }
            if i.consume_key(none, Key::ArrowLeft) {
                self.sx.scroll_by(-48.0);
            }
            if i.consume_key(none, Key::Home) || i.consume_key(Modifiers::CTRL, Key::Home) {
                self.sy.scroll_to(0.0);
            }
            if i.consume_key(none, Key::End) || i.consume_key(Modifiers::CTRL, Key::End) {
                self.sy.scroll_to(f64::MAX);
            }
            if i.consume_key(Modifiers::CTRL, Key::Num0) || i.consume_key(none, Key::Num0) {
                fit = true;
            }
            let zoom_in = i.consume_key(Modifiers::CTRL, Key::Plus)
                || i.consume_key(Modifiers::CTRL, Key::Equals)
                || i.consume_key(none, Key::Plus)
                || i.consume_key(none, Key::Equals);
            let zoom_out =
                i.consume_key(Modifiers::CTRL, Key::Minus) || i.consume_key(none, Key::Minus);
            if zoom_in {
                new_zoom = Some((zoom * 1.25, None));
            }
            if zoom_out {
                new_zoom = Some((zoom / 1.25, None));
            }
        });
        if fit && self.zoom != Zoom::FitWidth {
            let target = layout.fit_width_zoom(vw - 14.0);
            new_zoom = Some((target, None));
            self.zoom = Zoom::FitWidth;
        }

        if let Some((target, anchor)) = new_zoom {
            let target = target.clamp(MIN_ZOOM, MAX_ZOOM);
            if (target - zoom).abs() > 1e-9 {
                // Keep the document point under the anchor fixed.
                let anchor = anchor.unwrap_or(vp.center());
                let (ax, ay) = ((anchor.x - vp.left()) as f64, (anchor.y - vp.top()) as f64);
                let x_off = |z: f64| ((vw - layout.content_width() * z) / 2.0).max(0.0);
                let doc_x = (self.sx.offset + ax - x_off(zoom)) / zoom;
                let doc_y = (self.sy.offset + ay) / zoom;
                if !fit {
                    self.zoom = Zoom::Manual(target);
                }
                zoom = target;
                set_axes(&mut self.sy, &mut self.sx, zoom);
                self.sy.scroll_to(doc_y * zoom - ay);
                self.sx.scroll_to(doc_x * zoom + x_off(zoom) - ax);
                self.zoom_changed_at = now;
                self.toast = Some((format!("{:.0}%", zoom / ZOOM_100 * 100.0), now + 1.0));
            }
        }

        // --- which tiles, at which scale -------------------------------------------
        let scale = zoom * ppp; // device px per PDF point
        let scale_key = (scale * 1000.0).round().max(1.0) as u32;
        let settled = match self.settled_scale {
            Some(k) if k != scale_key && now - self.zoom_changed_at < ZOOM_SETTLE => {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(ZOOM_SETTLE));
                k
            }
            _ => {
                self.settled_scale = Some(scale_key);
                scale_key
            }
        };
        let settled_scale = settled as f64 / 1000.0;
        let max_side = self.textures.max_texture_side();

        let x_off = ((vw - layout.content_width() * zoom) / 2.0).max(0.0);
        let snap = |v: f64| (v * ppp).round() / ppp;
        let page_rect = |i: usize| -> Rect {
            let [pw, ph] = layout.sizes[i];
            let doc_x = layout::MARGIN + (layout.max_width - pw as f64) / 2.0;
            let x = vp.left() as f64 + x_off + doc_x * zoom - self.sx.offset;
            let y = vp.top() as f64 + layout.top(i) * zoom - self.sy.offset;
            Rect::from_min_size(
                pos2(snap(x) as f32, snap(y) as f32),
                vec2((pw as f64 * zoom) as f32, (ph as f64 * zoom) as f32),
            )
        };
        let tile_rect = |page: Rect, r: [u32; 4], tile_scale: f64| -> Rect {
            let k = zoom / tile_scale; // UI points per tile pixel
            Rect::from_min_size(
                page.min + vec2((r[0] as f64 * k) as f32, (r[1] as f64 * k) as f32),
                vec2((r[2] as f64 * k) as f32, (r[3] as f64 * k) as f32),
            )
        };

        let y0 = self.sy.offset / zoom;
        let y1 = (self.sy.offset + vh) / zoom;
        let visible = layout.visible(y0, y1);
        let mut wanted: Vec<Job> = Vec::new();
        let frame = self.frame;
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

        for i in visible.clone() {
            let prect = page_rect(i);
            painter.rect_filled(prect.expand(1.0), 0.0, theme.separator);
            painter.rect_filled(prect, 0.0, Color32::WHITE);

            // Which current-scale tiles are on screen, and are they all ready?
            let page_px = page_px_at(layout.sizes[i], settled_scale);
            let current: Vec<(TileKey, [u32; 4], Rect)> = layout::tiles(page_px, max_side)
                .into_iter()
                .map(|r| {
                    (
                        tile_key(i, settled, r),
                        r,
                        tile_rect(prect, r, settled_scale),
                    )
                })
                .filter(|(_, _, tr)| tr.intersects(vp))
                .collect();
            let complete = current
                .iter()
                .all(|(k, _, _)| self.tiles.contains_key(k) || self.failed_tiles.contains(k));

            if complete {
                // Stretched stand-ins from other zoom levels are no longer needed.
                let page = i as u32;
                self.tiles
                    .retain(|k, _| k.page != page || k.scale == settled);
            } else {
                // Stale tiles (other scales) stand in, stretched, lowest resolution first.
                let mut stale: Vec<(TileKey, &mut Cached)> = self
                    .tiles
                    .iter_mut()
                    .filter(|(k, _)| k.page == i as u32 && k.scale != settled)
                    .map(|(k, c)| (*k, c))
                    .collect();
                stale.sort_by_key(|(k, _)| k.scale);
                for (k, c) in stale {
                    let s = k.scale as f64 / 1000.0;
                    let rects = layout::tiles(page_px_at(layout.sizes[i], s), max_side);
                    if let Some(r) = rects.into_iter().find(|r| tile_key(i, k.scale, *r) == k) {
                        let tr = tile_rect(prect, r, s);
                        if tr.intersects(vp) {
                            painter.image(c.texture.id(), tr, uv, Color32::WHITE);
                            c.last_used = frame;
                        }
                    }
                }
            }

            // Current tiles at exact device resolution.
            for (key, r, tr) in current {
                match self.tiles.get_mut(&key) {
                    Some(c) => {
                        painter.image(c.texture.id(), tr, uv, Color32::WHITE);
                        c.last_used = frame;
                    }
                    None if self.failed_tiles.contains(&key) => {}
                    None => wanted.push(Job {
                        key,
                        rect: r,
                        page_px,
                    }),
                }
            }
        }

        // Prefetch the neighbouring pages (whole-page tiles only).
        for i in [visible.start.checked_sub(1), Some(visible.end)]
            .into_iter()
            .flatten()
        {
            if i >= layout.len() {
                continue;
            }
            let page_px = page_px_at(layout.sizes[i], settled_scale);
            let rects = layout::tiles(page_px, max_side);
            if rects.len() == 1 {
                let key = tile_key(i, settled, rects[0]);
                match self.tiles.get_mut(&key) {
                    Some(c) => c.last_used = frame,
                    None if self.failed_tiles.contains(&key) => {}
                    None => wanted.push(Job {
                        key,
                        rect: rects[0],
                        page_px,
                    }),
                }
            }
        }

        let keys: Vec<TileKey> = wanted.iter().map(|j| j.key).collect();
        if keys != self.last_wanted {
            self.handle.want(wanted);
            self.last_wanted = keys;
        }

        // --- overlays ------------------------------------------------------------------
        let current = layout.page_at((self.sy.offset + vh / 2.0) / zoom) + 1;
        let pages = layout.len();
        self.evict(frame);
        if (self.sy.offset - self.last_scroll).abs() > 0.5 {
            self.last_scroll = self.sy.offset;
            self.toast = Some((format!("{current} / {pages}"), now + 1.2));
        }
        let vbar = Rect::from_min_max(
            pos2(vp.right() - 12.0, vp.top()),
            pos2(vp.right(), vp.bottom() - 12.0),
        );
        let hbar = Rect::from_min_max(
            pos2(vp.left(), vp.bottom() - 12.0),
            pos2(vp.right() - 12.0, vp.bottom()),
        );
        let id = ui.id().with("pdf");
        scrollbar(ui, id.with("v"), vbar, &mut self.sy, true);
        scrollbar(ui, id.with("h"), hbar, &mut self.sx, false);
        paint_toast(ui, vp, &mut self.toast, now, &theme);
    }
}

/// Size of a page's raster at `scale` device pixels per PDF point.
fn page_px_at(size: [f32; 2], scale: f64) -> [u32; 2] {
    [
        ((size[0] as f64 * scale).ceil() as u32).max(1),
        ((size[1] as f64 * scale).ceil() as u32).max(1),
    ]
}

/// Tiles are identified by their 1024-px grid cell (a whole-page tile is cell 0,0).
fn tile_key(page: usize, scale: u32, r: [u32; 4]) -> TileKey {
    TileKey {
        page: page as u32,
        scale,
        tx: (r[0] / 1024) as u16,
        ty: (r[1] / 1024) as u16,
    }
}

fn paint_toast(
    ui: &egui::Ui,
    vp: Rect,
    toast: &mut Option<(String, f64)>,
    now: f64,
    theme: &Theme,
) {
    let Some((text, until)) = toast else { return };
    if now >= *until {
        *toast = None;
        return;
    }
    let alpha = ((*until - now) / 0.25).clamp(0.0, 1.0) as f32;
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(
        text.clone(),
        egui::FontId::proportional(12.5),
        theme.text.gamma_multiply(alpha),
    );
    let pad = vec2(12.0, 6.0);
    let size = galley.size() + pad * 2.0;
    let rect = Rect::from_center_size(
        Pos2::new(vp.center().x, vp.bottom() - 28.0 - size.y / 2.0),
        size,
    );
    painter.rect_filled(rect, size.y / 2.0, theme.overlay_bg.gamma_multiply(alpha));
    painter.rect_stroke(
        rect,
        size.y / 2.0,
        Stroke::new(1.0, theme.separator.gamma_multiply(alpha)),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + pad, galley, theme.text);
    ui.ctx().request_repaint_after(Duration::from_millis(30));
}

impl Document for PdfDoc {
    fn title(&self) -> &str {
        &self.title
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn kind(&self) -> DocKind {
        DocKind::Pdf
    }

    fn is_loading(&self) -> bool {
        match &self.state {
            State::Loading => true,
            State::Ready(l) => !self.got_first_tile && !l.is_empty(),
            State::Failed(_) => false,
        }
    }

    fn poll(&mut self) {
        self.drain();
    }

    fn on_visibility(&mut self, visible: bool) {
        if !visible {
            // Hidden tabs keep only tiles at the current scale.
            if let Some(s) = self.settled_scale {
                self.tiles.retain(|k, _| k.scale == s);
            }
            self.handle.want(Vec::new());
            self.last_wanted.clear();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _cx: &mut DocCx<'_>) {
        self.drain();
        match &self.state {
            State::Loading => {
                let theme = Theme::current();
                let r = ui.max_rect();
                ui.painter().rect_filled(r, 0.0, theme.canvas_bg);
                ui.put(
                    Rect::from_center_size(r.center(), vec2(24.0, 24.0)),
                    egui::Spinner::new().size(20.0),
                );
            }
            State::Failed(e) => {
                let e = e.clone();
                whir_core::message_doc::show_message(
                    ui,
                    "Can't open this PDF",
                    &e,
                    Some(&self.path),
                );
            }
            State::Ready(_) => self.show(ui),
        }
    }
}
