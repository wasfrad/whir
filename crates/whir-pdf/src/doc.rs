//! The PDF tab: continuous scroll, fit-width, zoom at the cursor, tiles at
//! exact device resolution (stretched while a zoom settles), LRU texture budget;
//! text selection, copy and find over text streamed from the PDF thread; a
//! minimap of page thumbnails with page controls.

mod find;
mod minimap;
mod selection;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use egui::{Color32, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use whir_core::find_bar::FindBar;
use whir_core::scroll::{ScrollAxis, scrollbar};
use whir_core::theme::Theme;
use whir_core::{DocCx, DocKind, Document, OpenCx, TextureRegistry, UserTexture};

use crate::engine::{DocHandle, Event, Job, TileKey};
use crate::layout::{self, Layout};
use crate::mini_layout::MiniLayout;
use crate::search::{Match, Search};
use crate::select::{ClickCounter, Gesture, Selection};
use crate::store::TextStore;
use crate::view::View;
use selection::consume_sel_key;

/// GPU memory for rendered tiles, per document.
const TEXTURE_BUDGET: u64 = 96 << 20;
/// Hold off re-rendering while a zoom gesture is in progress.
const ZOOM_SETTLE: f64 = 0.15;
/// UI points per PDF point at "100%": 1 pt = 1/72 in, 1 UI point = 1/96 in.
const ZOOM_100: f64 = 96.0 / 72.0;
const MIN_ZOOM: f64 = 0.1;
const MAX_ZOOM: f64 = 12.0 * ZOOM_100;
/// Scrollbar strip thickness.
const BAR: f32 = 12.0;
/// Documents up to this many pages extract all their text in the background
/// once the first screen is drawn, so Ctrl+F is instant on typical files.
const WARM_MAX_PAGES: usize = 200;
/// A match's page geometry normally arrives within a frame or two; if it
/// hasn't after this long, scroll to its page first and refine later.
const REVEAL_WAIT: f64 = 0.25;

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    FitWidth,
    Manual(f64),
}

enum State {
    Loading,
    Ready(Arc<Layout>),
    Failed(String),
}

struct Cached {
    texture: UserTexture,
    last_used: u64,
}

/// A match waiting to be scrolled into view.
#[derive(Clone, Copy)]
struct Reveal {
    page: u32,
    m: Match,
    since: f64,
    /// Its page was scrolled on screen while waiting for geometry.
    page_shown: bool,
}

pub struct PdfDoc {
    path: PathBuf,
    title: String,
    handle: DocHandle,
    rx: Receiver<Event>,
    textures: Arc<dyn TextureRegistry>,
    state: State,
    tiles: HashMap<TileKey, Cached>,
    /// Minimap thumbnails, at most one per page (kept apart from `tiles`,
    /// whose eviction is tuned for the screen). They also stand in, stretched,
    /// for pages with no tiles yet.
    thumbs: HashMap<TileKey, Cached>,
    /// Tiles and thumbnails PDFium couldn't produce: never requested again.
    failed_tiles: HashSet<TileKey>,
    /// A screen tile arrived (or failed): the document has shown itself.
    got_first_tile: bool,
    mini: Option<Arc<MiniLayout>>,
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
    /// The zoom and page area the last frame was drawn with.
    shown_zoom: Option<f64>,
    shown_vp: Rect,
    /// The go-to-page field's text while it's open.
    goto: Option<String>,
    goto_focus_pending: bool,

    text: TextStore,
    sel: Option<Selection>,
    gesture: Gesture,
    clicks: ClickCounter,
    find: FindBar,
    search: Search,
    pending_reveal: Option<Reveal>,
    /// Ctrl+C arrived before every selected page's text: copy when it has.
    pending_copy: bool,
    /// Pages last asked for urgently (text + geometry).
    last_text_urgent: Vec<u32>,
    /// Highest `Event::Text::seq` received (lets the PDF thread skip pages
    /// already on their way).
    text_seen: u64,
    /// The background sweep is running.
    sweep_on: bool,
    /// Pages whose geometry is worth keeping when it arrives: last frame's
    /// visible pages ± 2.
    near: Range<usize>,
    /// Small document, first screen drawn: extract everything in the background.
    warm: bool,
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
            thumbs: HashMap::new(),
            failed_tiles: HashSet::new(),
            got_first_tile: false,
            mini: None,
            zoom: Zoom::FitWidth,
            sy: ScrollAxis::default(),
            sx: ScrollAxis::default(),
            settled_scale: None,
            zoom_changed_at: f64::NEG_INFINITY,
            last_wanted: Vec::new(),
            frame: 0,
            toast: None,
            last_scroll: 0.0,
            shown_zoom: None,
            shown_vp: Rect::NOTHING,
            goto: None,
            goto_focus_pending: false,
            text: TextStore::new(0, false),
            sel: None,
            gesture: Gesture::None,
            clicks: ClickCounter::default(),
            find: FindBar::new("Find in document", 0.1),
            search: Search::default(),
            pending_reveal: None,
            pending_copy: false,
            last_text_urgent: Vec::new(),
            text_seen: 0,
            sweep_on: false,
            near: 0..0,
            warm: false,
        }
    }

    fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Opened { pages, text } => {
                    self.text = TextStore::new(pages.len(), text);
                    let layout = Layout::new(pages);
                    self.mini = Some(Arc::new(MiniLayout::new(&layout, minimap::THUMB_W)));
                    self.state = State::Ready(Arc::new(layout));
                }
                Event::Failed(e) => self.state = State::Failed(e),
                Event::Tile { key, texture } => {
                    let cached = Cached {
                        texture,
                        last_used: self.frame,
                    };
                    if key.thumb {
                        // One per page: a new scale replaces the old.
                        self.thumbs.retain(|k, _| k.page != key.page);
                        self.thumbs.insert(key, cached);
                    } else {
                        self.got_first_tile = true;
                        self.tiles.insert(key, cached);
                    }
                }
                Event::TileFailed { key } => {
                    // Leave that area blank rather than retrying forever.
                    self.got_first_tile |= !key.thumb;
                    self.failed_tiles.insert(key);
                }
                Event::Text { page, seq, text } => {
                    self.text_seen = self.text_seen.max(seq);
                    let keep_geom = self.near.contains(&(page as usize))
                        || self.last_text_urgent.contains(&page)
                        || self.pending_reveal.is_some_and(|r| r.page == page);
                    if self.text.insert(page, *text, keep_geom, self.frame) {
                        self.search.text_arrived(page);
                    }
                }
            }
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let State::Ready(layout) = &self.state else {
            return;
        };
        let layout = layout.clone();
        let l = &*layout;
        let theme = Theme::current();
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point() as f64;
        let now = ui.input(|i| i.time);
        self.frame += 1;
        let frame = self.frame;
        // Before anything consumes keys: while the find field has focus, its
        // keys (typing, arrows, Home/End, Ctrl+A, Ctrl+C) are its own. (Its
        // focus survives Esc, which the field handles itself.)
        let find_focus = self.find.has_focus(&ctx);
        // Either text field: its keys are its own.
        let typing = find_focus || self.goto_has_focus(&ctx);

        let (full, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
        ui.painter().rect_filled(full, 0.0, theme.canvas_bg);
        if l.is_empty() {
            return;
        }
        // With room for it, the minimap takes a column at the right, the
        // scrollbar beside it; the pages get the rest.
        let split = (minimap::enabled() && full.width() >= minimap::MIN_WINDOW)
            .then(|| minimap::split(full));
        let vp = split.map_or(full, |(pages, _)| pages);
        let panel = split.map(|(_, panel)| panel);
        let resp = ui.interact(vp, ui.id().with("pdf.pages"), Sense::CLICK | Sense::DRAG);
        let painter = ui.painter().with_clip_rect(vp);
        let vw = vp.width() as f64;
        let vh = vp.height() as f64;

        self.find_keys(ui, typing, now);

        // --- zoom & scroll -----------------------------------------------------
        let mut zoom = match self.zoom {
            Zoom::FitWidth => fit_zoom(l, vw, panel.is_some()),
            Zoom::Manual(z) => z,
        };
        // At fit width the zoom follows the page area's width (the window was
        // resized, the minimap toggled): stay at the same place in the document.
        if let Some(prev) = self.shown_zoom
            && self.zoom == Zoom::FitWidth
            && (zoom - prev).abs() > 1e-9
        {
            self.sy.offset *= zoom / prev;
            self.sx.offset *= zoom / prev;
            self.zoom_changed_at = now;
        }
        let set_axes = |sy: &mut ScrollAxis, sx: &mut ScrollAxis, zoom: f64| {
            sy.content = l.total_height * zoom;
            sy.viewport = vh;
            sx.content = l.content_width() * zoom;
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

        // Shift+arrows extend a selection; they go before the plain arrows,
        // which would otherwise match them too.
        let sel_key = match self.sel {
            Some(_) if !typing => ui.input_mut(consume_sel_key),
            _ => None,
        };
        let page_step = (vh * 0.9).max(40.0);
        let mut fit = false;
        // Next (true) or previous page.
        let mut step = None;
        let (mut toggle_minimap, mut goto) = (false, false);
        ui.input_mut(|i| {
            let none = Modifiers::NONE;
            let ctrl = Modifiers::CTRL;
            // Ctrl shortcuts work even while typing in the find field.
            fit |= i.consume_key(ctrl, Key::Num0);
            let mut zoom_in = i.consume_key(ctrl, Key::Plus) || i.consume_key(ctrl, Key::Equals);
            let mut zoom_out = i.consume_key(ctrl, Key::Minus);
            toggle_minimap = i.consume_key(ctrl, Key::M);
            goto = i.consume_key(ctrl, Key::G);
            if !typing {
                if i.consume_key(ctrl, Key::ArrowUp) {
                    step = Some(false);
                }
                if i.consume_key(ctrl, Key::ArrowDown) {
                    step = Some(true);
                }
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
                // Left/right pan when there's something to pan to; else they turn pages.
                if i.consume_key(none, Key::ArrowRight) {
                    if self.sx.is_scrollable() {
                        self.sx.scroll_by(48.0);
                    } else {
                        step = Some(true);
                    }
                }
                if i.consume_key(none, Key::ArrowLeft) {
                    if self.sx.is_scrollable() {
                        self.sx.scroll_by(-48.0);
                    } else {
                        step = Some(false);
                    }
                }
                if i.consume_key(none, Key::Home) || i.consume_key(ctrl, Key::Home) {
                    self.sy.scroll_to(0.0);
                }
                if i.consume_key(none, Key::End) || i.consume_key(ctrl, Key::End) {
                    self.sy.scroll_to(f64::MAX);
                }
                fit |= i.consume_key(none, Key::Num0);
                zoom_in |= i.consume_key(none, Key::Plus) || i.consume_key(none, Key::Equals);
                zoom_out |= i.consume_key(none, Key::Minus);
            }
            if zoom_in {
                new_zoom = Some((zoom * 1.25, None));
            }
            if zoom_out {
                new_zoom = Some((zoom / 1.25, None));
            }
        });
        if let Some(k) = sel_key {
            self.sel_key(k, frame);
        }
        if let Some(forward) = step {
            self.step_page(l, zoom, forward);
        }
        if toggle_minimap {
            minimap::toggle();
            ctx.request_repaint();
        }
        if goto {
            let current = l.most_visible(self.sy.offset / zoom, (self.sy.offset + vh) / zoom);
            self.open_goto(current);
        }
        if !typing {
            let (all, copy) = ui.input_mut(|i| {
                let all = i.consume_key(Modifiers::CTRL, Key::A);
                let n = i.events.len();
                i.events
                    .retain(|e| !matches!(e, egui::Event::Copy | egui::Event::Cut));
                (all, i.events.len() != n)
            });
            if all && self.text.available {
                self.set_sel(Some(Selection::all(l.len() as u32)));
            }
            if copy {
                self.copy(&ctx, now);
            }
        }
        if fit && self.zoom != Zoom::FitWidth {
            let target = fit_zoom(l, vw, panel.is_some());
            new_zoom = Some((target, None));
            self.zoom = Zoom::FitWidth;
        }

        if let Some((target, anchor)) = new_zoom {
            let target = target.clamp(MIN_ZOOM, MAX_ZOOM);
            if (target - zoom).abs() > 1e-9 {
                // Keep the document point under the anchor fixed.
                let anchor = anchor.unwrap_or(vp.center());
                let (ax, ay) = ((anchor.x - vp.left()) as f64, (anchor.y - vp.top()) as f64);
                let x_off = |z: f64| ((vw - l.content_width() * z) / 2.0).max(0.0);
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
        self.shown_zoom = Some(zoom);
        self.shown_vp = vp;

        // --- reveal, pointer ---------------------------------------------------
        let x_off = ((vw - l.content_width() * zoom) / 2.0).max(0.0);
        let view = |d: &Self| View {
            vp,
            zoom,
            x_off,
            sx: d.sx.offset,
            sy: d.sy.offset,
            ppp,
        };
        self.apply_reveal(l, vp, zoom, x_off, now, frame);
        self.pointer(ui, &resp, l, view(self), frame);
        let v = view(self);
        self.hover_cursor(ui, &resp, l, v, frame);

        // --- which tiles, at which scale -------------------------------------------
        let scale = zoom * ppp; // device px per PDF point
        let scale_key = (scale * 1000.0).round().max(1.0) as u32;
        let settled = match self.settled_scale {
            Some(k) if k != scale_key && now - self.zoom_changed_at < ZOOM_SETTLE => {
                ctx.request_repaint_after(Duration::from_secs_f64(ZOOM_SETTLE));
                k
            }
            _ => {
                self.settled_scale = Some(scale_key);
                scale_key
            }
        };
        let settled_scale = settled as f64 / 1000.0;
        let max_side = self.textures.max_texture_side();

        let tile_rect = |page: Rect, r: [u32; 4], tile_scale: f64| -> Rect {
            let k = zoom / tile_scale; // UI points per tile pixel
            Rect::from_min_size(
                page.min + vec2((r[0] as f64 * k) as f32, (r[1] as f64 * k) as f32),
                vec2((r[2] as f64 * k) as f32, (r[3] as f64 * k) as f32),
            )
        };

        let y0 = self.sy.offset / zoom;
        let y1 = (self.sy.offset + vh) / zoom;
        let visible = l.visible(y0, y1);
        let mut wanted: Vec<Job> = Vec::new();
        // Thumbnails of on-screen pages with nothing else to show yet: a
        // quick preview while their tiles render.
        let mut previews: Vec<Job> = Vec::new();
        let mini = self.mini.clone();
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

        for i in visible.clone() {
            let prect = v.page_rect(l, i);
            painter.rect_filled(prect.expand(1.0), 0.0, theme.separator);
            painter.rect_filled(prect, 0.0, Color32::WHITE);

            // Which current-scale tiles are on screen, and are they all ready?
            let page_px = page_px_at(l.sizes[i], settled_scale);
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
                // The page's thumbnail stands in first, stretched…
                if let Some(m) = &mini {
                    let job = minimap::thumb_job(m, l, i, ppp);
                    let preview = self.got_first_tile
                        && self.needs_thumb(job.key)
                        && !self.tiles.keys().any(|k| k.page == i as u32);
                    match self.thumb_of(job.key) {
                        Some(c) => {
                            painter.image(c.texture.id(), prect, uv, Color32::WHITE);
                            c.last_used = frame;
                        }
                        None if preview => previews.push(job),
                        None => {}
                    }
                }
                // …then stale tiles (other scales), lowest resolution first.
                let mut stale: Vec<(TileKey, &mut Cached)> = self
                    .tiles
                    .iter_mut()
                    .filter(|(k, _)| k.page == i as u32 && k.scale != settled)
                    .map(|(k, c)| (*k, c))
                    .collect();
                stale.sort_by_key(|(k, _)| k.scale);
                for (k, c) in stale {
                    let s = k.scale as f64 / 1000.0;
                    let rects = layout::tiles(page_px_at(l.sizes[i], s), max_side);
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
            if i >= l.len() {
                continue;
            }
            let page_px = page_px_at(l.sizes[i], settled_scale);
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

        self.paint_highlights(&painter, l, v, visible.clone(), frame, &theme);

        // Previews, then the screen's tiles, then the minimap's thumbnails.
        let current = l.most_visible(y0, y1);
        let place = match (panel, &mini) {
            (Some(panel), Some(m)) => Some(minimap::Placement::new(m.clone(), panel, &self.sy)),
            _ => None,
        };
        let tiles_pending = !wanted.is_empty();
        let mut jobs = previews;
        jobs.append(&mut wanted);
        if let Some(place) = &place {
            self.minimap_jobs(place, l, current, ppp, &mut jobs);
        }
        let keys: Vec<TileKey> = jobs.iter().map(|j| j.key).collect();
        if keys != self.last_wanted {
            self.handle.want(jobs);
            self.last_wanted = keys;
        }

        // --- text requests, copy, search -----------------------------------------
        self.near = visible.start.saturating_sub(2)..(visible.end + 2).min(l.len());
        self.request_text(l, visible.clone(), current, tiles_pending, frame);
        evict(&mut self.tiles, TEXTURE_BUDGET, frame);
        evict(&mut self.thumbs, minimap::THUMB_BUDGET, frame);
        self.text.evict_geom(frame);
        if self.pending_copy {
            self.copy(&ctx, now);
        }
        // Closed, the search waits (text arrivals stay queued): the view never
        // jumps to a match nobody is looking for.
        if self.find.open {
            self.run_search(&ctx, now);
        }
        self.context_menu(&resp, &ctx, l.len(), now);

        // --- overlays ------------------------------------------------------------------
        let pages = l.len();
        if (self.sy.offset - self.last_scroll).abs() > 0.5 {
            self.last_scroll = self.sy.offset;
            // The minimap's header shows the page number already.
            if place.is_none() {
                self.toast = Some((format!("{} / {pages}", current + 1), now + 1.2));
            }
        }
        let mut moved = false;
        if let Some(place) = &place {
            moved |= self.show_minimap(ui, place, l, v, current, frame);
        } else if self.goto.is_some() {
            // No minimap to hold the go-to field: it floats at the top.
            let rect = Rect::from_center_size(
                pos2(vp.center().x, vp.top() + 8.0 + 16.0),
                vec2(120.0, 32.0),
            );
            ui.interact(
                rect,
                ui.id().with("pdf.goto.bg"),
                Sense::CLICK | Sense::DRAG,
            );
            ui.painter().rect_filled(rect, 8.0, theme.overlay_bg);
            ui.painter().rect_stroke(
                rect,
                8.0,
                Stroke::new(1.0, theme.separator),
                StrokeKind::Inside,
            );
            moved |= self.goto_field(ui, rect.shrink2(vec2(12.0, 5.0)), l, zoom);
        }
        let (vbar, hbar_end) = match panel {
            Some(p) => (
                Rect::from_min_max(pos2(p.right() - BAR, p.top()), p.max),
                vp.right(),
            ),
            None => (
                Rect::from_min_max(
                    pos2(vp.right() - BAR, vp.top()),
                    pos2(vp.right(), vp.bottom() - BAR),
                ),
                vp.right() - BAR,
            ),
        };
        let hbar = Rect::from_min_max(
            pos2(vp.left(), vp.bottom() - BAR),
            pos2(hbar_end, vp.bottom()),
        );
        let id = ui.id().with("pdf");
        moved |= scrollbar(ui, id.with("v"), vbar, &mut self.sy, true);
        moved |= scrollbar(ui, id.with("h"), hbar, &mut self.sx, false);
        if moved {
            // Drawn after the pages: show them where the view went.
            ctx.request_repaint();
        }
        paint_toast(ui, vp, &mut self.toast, now, &theme);

        // The find bar goes last: drawn on top, and it sees the keys the
        // document left alone.
        let status = self.find_status();
        let out = self.find.show(ui, vp, 0.0, &status);
        if let Some(q) = out.search {
            let anchor = self.search_anchor(l, v, visible, frame);
            self.start_search(&q.text, anchor, now, &ctx);
        }
        if out.next || out.prev {
            self.step_match(out.next, now);
        }
        if out.closed {
            self.close_find();
        }
        if let Some(r) = self.pending_reveal {
            if r.since >= now {
                ctx.request_repaint();
            } else if !r.page_shown {
                ctx.request_repaint_after(Duration::from_secs_f64(
                    (REVEAL_WAIT - (now - r.since)).max(0.01),
                ));
            }
        }
    }

    // --- text requests -----------------------------------------------------------------

    /// Ask the PDF thread for what the screen needs (text + geometry of the
    /// visible pages and their neighbours), and run the background sweep while
    /// find is open, a copy waits, or the document is small.
    fn request_text(
        &mut self,
        l: &Layout,
        visible: Range<usize>,
        current: usize,
        tiles_pending: bool,
        frame: u64,
    ) {
        if !self.text.available {
            return;
        }
        // Geometry near the screen stays fresh in the LRU even when nothing
        // on it is highlighted.
        for p in self.near.clone() {
            self.text.touch_geom(p as u32, frame);
        }
        let mut urgent: Vec<u32> = Vec::new();
        let reveal = self.pending_reveal.map(|r| r.page as usize);
        let neighbours = [visible.start.checked_sub(1), Some(visible.end)];
        for p in reveal
            .into_iter()
            .chain(visible)
            .chain(neighbours.into_iter().flatten())
        {
            let p = p as u32;
            if (p as usize) < l.len() && self.text.needs_geom(p) && !urgent.contains(&p) {
                urgent.push(p);
            }
        }
        if urgent != self.last_text_urgent {
            self.handle.want_text(urgent.clone(), self.text_seen);
            self.last_text_urgent = urgent;
        }

        if !self.warm && l.len() <= WARM_MAX_PAGES && self.got_first_tile && !tiles_pending {
            self.warm = true;
        }
        let want = !self.text.all_known()
            && !self.text.full()
            && (self.find.open || self.pending_copy || self.warm);
        if want != self.sweep_on {
            let from = match self.sel {
                Some(s) if self.pending_copy => s.start.page,
                _ => current as u32,
            };
            self.handle.sweep(want.then_some(from));
            self.sweep_on = want;
        }
    }
}

/// Drop the least recently used textures (not used since `keep_frame`) until
/// `cache` fits in `budget` bytes.
fn evict(cache: &mut HashMap<TileKey, Cached>, budget: u64, keep_frame: u64) {
    let bytes = |c: &Cached| {
        let [w, h] = c.texture.size();
        w as u64 * h as u64 * 4
    };
    let mut total: u64 = cache.values().map(bytes).sum();
    if total <= budget {
        return;
    }
    let mut by_age: Vec<(u64, TileKey, u64)> = cache
        .iter()
        .filter(|(_, c)| c.last_used < keep_frame)
        .map(|(k, c)| (c.last_used, *k, bytes(c)))
        .collect();
    by_age.sort_unstable_by_key(|e| e.0);
    for (_, key, b) in by_age {
        if total <= budget {
            break;
        }
        cache.remove(&key);
        total -= b;
    }
}

/// Fit-width zoom for a page area `vw` wide. Without the minimap's column
/// the scrollbar overlays the page area: leave it room.
fn fit_zoom(l: &Layout, vw: f64, column: bool) -> f64 {
    l.fit_width_zoom(if column { vw } else { vw - 14.0 })
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
        thumb: false,
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
        StrokeKind::Inside,
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
            // Hidden tabs keep only tiles at the current scale…
            if let Some(s) = self.settled_scale {
                self.tiles.retain(|k, _| k.scale == s);
            }
            // …and the thumbnails the minimap was showing.
            let frame = self.frame;
            self.thumbs.retain(|_, c| c.last_used == frame);
            self.goto = None;
            self.handle.want(Vec::new());
            self.last_wanted.clear();
            // …and no geometry beyond what was on screen; text stays (it's small).
            // The next visible frame asks again for whatever it needs.
            self.handle.want_text(Vec::new(), self.text_seen);
            self.handle.sweep(None);
            self.last_text_urgent.clear();
            self.sweep_on = false;
            self.text.drop_geom_except(self.frame);
            self.gesture = Gesture::None;
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use egui::{Event as In, PointerButton};
    use whir_core::find_bar::FindStatus;
    use whir_core::{Library, Waker};

    use super::*;
    use crate::engine::test_support::{NoTextures, pages_pdf, pdfium_lock, test_pdf};

    const NONE: Modifiers = Modifiers::NONE;

    /// Drives a `PdfDoc` through egui frames: no window, no GPU (tiles fail,
    /// which the viewer tolerates), the real PDF thread and pdfium.dll.
    struct Harness {
        ctx: egui::Context,
        doc: PdfDoc,
        textures: Arc<dyn TextureRegistry>,
        screen: Rect,
        time: f64,
        copied: Vec<String>,
    }

    impl Harness {
        fn new(path: PathBuf) -> Self {
            let ctx = egui::Context::default();
            let mut fonts = egui::FontDefinitions::empty();
            let data = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf").expect("Segoe UI");
            fonts
                .font_data
                .insert("ui".into(), Arc::new(egui::FontData::from_owned(data)));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.insert(family, vec!["ui".into()]);
            }
            ctx.set_fonts(fonts);
            let textures: Arc<dyn TextureRegistry> = Arc::new(NoTextures);
            // Defaults (minimap on), and never the user's settings file.
            whir_core::settings::memory_only();
            let cx = OpenCx {
                textures: textures.clone(),
                waker: Waker::noop(),
                library: Library::at(std::env::temp_dir(), Waker::noop()),
                captures: whir_core::Captures::at(
                    std::env::temp_dir().join("whir-no-captures"),
                    Waker::noop(),
                ),
            };
            Self {
                ctx,
                doc: PdfDoc::open(path, &cx),
                textures,
                screen: Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0)),
                time: 1.0,
                copied: Vec::new(),
            }
        }

        fn frame(&mut self, events: Vec<In>, modifiers: Modifiers) {
            self.time += 1.0 / 60.0;
            let mut all = vec![In::ModifiersChanged(modifiers)];
            all.extend(events);
            let input = egui::RawInput {
                screen_rect: Some(self.screen),
                time: Some(self.time),
                events: all,
                ..Default::default()
            };
            let (doc, textures) = (&mut self.doc, &self.textures);
            let waker = Waker::noop();
            let mut requests = Vec::new();
            let mut out = self.ctx.run_ui(input, |ui| {
                let mut cx = DocCx {
                    textures,
                    waker: &waker,
                    just_activated: false,
                    requests: &mut requests,
                };
                doc.ui(ui, &mut cx);
            });
            for c in std::mem::take(&mut out.platform_output.commands) {
                if let egui::OutputCommand::CopyText(s) = c {
                    self.copied.push(s);
                }
            }
            out.drop_without_applying_deltas();
        }

        fn idle(&mut self) {
            self.frame(Vec::new(), NONE);
        }

        /// Run frames until `done` (the PDF thread works in the background).
        fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
            let t0 = Instant::now();
            while !done(self) {
                assert!(t0.elapsed() < Duration::from_secs(10), "timed out: {what}");
                std::thread::sleep(Duration::from_millis(5));
                self.idle();
            }
        }

        /// A screen point `fx` of the way across char `k` of `page`.
        fn at(&mut self, page: u32, k: usize, fx: f32) -> Pos2 {
            let State::Ready(l) = &self.doc.state else {
                panic!("not open");
            };
            let l = l.clone();
            let vp = self.doc.shown_vp;
            let vw = vp.width() as f64;
            let zoom = self.doc.shown_zoom.expect("drawn");
            let v = View {
                vp,
                zoom,
                x_off: ((vw - l.content_width() * zoom) / 2.0).max(0.0),
                sx: self.doc.sx.offset,
                sy: self.doc.sy.offset,
                ppp: self.ctx.pixels_per_point() as f64,
            };
            let frame = self.doc.frame;
            let b = self.doc.text.geom(page, frame).expect("geometry").boxes[k];
            let r = v.to_screen(v.page_rect(&l, page as usize), b);
            pos2(r.left() + r.width() * fx, r.center().y)
        }

        fn button(&mut self, p: Pos2, pressed: bool, modifiers: Modifiers) {
            let e = In::PointerButton {
                pos: p,
                button: PointerButton::Primary,
                pressed,
                modifiers,
            };
            self.frame(vec![e], modifiers);
        }

        fn click(&mut self, p: Pos2, modifiers: Modifiers) {
            self.frame(vec![In::PointerMoved(p)], modifiers);
            self.button(p, true, modifiers);
            self.button(p, false, modifiers);
        }

        fn key(&mut self, key: Key, modifiers: Modifiers) {
            let e = |pressed| In::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            };
            self.frame(vec![e(true), e(false)], modifiers);
        }

        /// Ctrl+C, and what reached the clipboard (if anything).
        fn copy(&mut self) -> Option<String> {
            self.frame(vec![In::Copy], NONE);
            self.copied.pop()
        }
    }

    /// Selection by drag, click counts, Shift, keyboard and Ctrl+A; copy; find.
    /// `cargo test -p whir-pdf -- --ignored`
    #[test]
    #[ignore = "loads pdfium.dll"]
    fn select_copy_and_find() {
        let _pdfium = pdfium_lock();
        let path = std::env::temp_dir().join(format!("whir-pdf-ui-{}.pdf", std::process::id()));
        std::fs::write(&path, test_pdf()).unwrap();
        let mut h = Harness::new(path.clone());
        h.until("page 0's text and geometry", |h| {
            h.doc.text.has_text(0) == Some(true) && !h.doc.text.needs_geom(0)
        });
        let chars = String::from_utf16_lossy(&h.doc.text.page(0).unwrap().chars);
        assert!(chars.starts_with("Hello world"), "{chars:?}");
        let second = chars.find("second").unwrap();

        // Drag across "Hello".
        let (a, b) = (h.at(0, 0, 0.1), h.at(0, 4, 0.9));
        h.frame(vec![In::PointerMoved(a)], NONE);
        h.button(a, true, NONE);
        h.frame(vec![In::PointerMoved(a.lerp(b, 0.5))], NONE);
        h.frame(vec![In::PointerMoved(b)], NONE);
        h.button(b, false, NONE);
        assert_eq!(h.copy().as_deref(), Some("Hello"));

        // A plain click leaves nothing to copy.
        let p = h.at(0, 2, 0.5);
        h.click(p, NONE);
        assert_eq!(h.copy(), None);

        // Double-click: the word; triple-click: the line.
        let w = h.at(0, 8, 0.5);
        h.click(w, NONE);
        h.click(w, NONE);
        assert_eq!(h.copy().as_deref(), Some("world"));
        let s = h.at(0, second + 2, 0.5);
        h.click(s, NONE);
        h.click(s, NONE);
        h.click(s, NONE);
        assert_eq!(h.copy().as_deref().map(str::trim_end), Some("second line"));

        // Shift+click extends from the anchor; Shift(+Ctrl)+Left shrinks it.
        let (p, q) = (h.at(0, 0, 0.1), h.at(0, 10, 0.9));
        h.click(p, NONE);
        h.click(q, Modifiers::SHIFT);
        assert_eq!(h.copy().as_deref(), Some("Hello world"));
        h.key(Key::ArrowLeft, Modifiers::SHIFT);
        assert_eq!(h.copy().as_deref(), Some("Hello worl"));
        h.key(Key::ArrowLeft, Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(h.copy().as_deref(), Some("Hello "));

        // Ctrl+A copies both pages (waiting for page 1's text if needed).
        h.key(Key::A, Modifiers::CTRL);
        h.frame(vec![In::Copy], NONE);
        h.until("the whole-document copy", |h| !h.copied.is_empty());
        let all = h.copied.pop().unwrap();
        assert_eq!(all.matches("second line").count(), 2, "{all:?}");
        assert!(!all.contains('Z') && !all.contains('\r'), "{all:?}");

        // Find (case-insensitive) from the selection's start: page 0 first.
        // Typing goes to the field: its Space doesn't page the document down.
        h.key(Key::F, Modifiers::CTRL);
        assert!(h.doc.find.open);
        h.frame(vec![In::Text("SECOND".into())], NONE);
        h.key(Key::Space, NONE);
        h.frame(vec![In::Text(" LINE".into())], NONE);
        h.until("both matches", |h| {
            h.doc.search.total() == 2 && !h.doc.search.pending() && h.doc.text.all_known()
        });
        assert_eq!(h.doc.find.query, "SECOND LINE");
        assert_eq!(h.doc.sy.offset, 0.0);
        let (page, m) = h.doc.search.current().expect("a current match");
        assert_eq!((page, m.start as usize, m.len), (0, second, 11));
        assert_eq!(
            h.doc.find_status(),
            FindStatus::Results {
                current: Some(0),
                total: 2,
                more: false
            }
        );
        // Enter steps to page 1 and scrolls it into view.
        h.key(Key::Enter, NONE);
        let (page, m) = h.doc.search.current().expect("a current match");
        assert_eq!(page, 1);
        h.until("the match on screen", |h| {
            h.doc.pending_reveal.is_none() && !h.doc.text.needs_geom(1)
        });
        let on_screen = h.at(1, m.start as usize, 0.5);
        assert!(h.screen.contains(on_screen), "{on_screen:?}");
        assert!(h.doc.sy.offset > 0.0);
        // Esc closes the bar and leaves the match selected.
        h.key(Key::Escape, NONE);
        assert!(!h.doc.find.open);
        assert_eq!(h.copy().as_deref(), Some("second line"));

        drop(h);
        // Let the PDF thread close the document before the lock is released.
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_file(&path);
    }

    /// Page keys, the minimap's controls, clicks and slider drags, go to
    /// page, and toggling the minimap.
    /// `cargo test -p whir-pdf -- --ignored`
    #[test]
    #[ignore = "loads pdfium.dll"]
    fn minimap_and_page_nav() {
        let _pdfium = pdfium_lock();
        let path = std::env::temp_dir().join(format!("whir-pdf-nav-{}.pdf", std::process::id()));
        std::fs::write(&path, pages_pdf(6)).unwrap();
        let mut h = Harness::new(path.clone());
        h.until("the first frame", |h| h.doc.shown_zoom.is_some());
        let State::Ready(l) = &h.doc.state else {
            panic!("not open");
        };
        let l = l.clone();
        let zoom = h.doc.shown_zoom.unwrap();
        let (pages, panel) = minimap::split(h.screen);
        assert_eq!(h.doc.shown_vp, pages, "the minimap takes its column");
        let at = |h: &Harness, p: usize| {
            let want = (l.landing(p) * zoom).min(h.doc.sy.max_offset());
            (h.doc.sy.offset - want).abs() < 1e-6
        };
        let ctrl = Modifiers::CTRL;

        // Ctrl+↓/↑ step pages; at fit width, → and ← do too.
        h.key(Key::ArrowDown, ctrl);
        assert!(at(&h, 1));
        h.key(Key::ArrowDown, ctrl);
        assert!(at(&h, 2));
        h.key(Key::ArrowUp, ctrl);
        assert!(at(&h, 1));
        h.key(Key::ArrowRight, NONE);
        assert!(at(&h, 2));
        h.key(Key::ArrowLeft, NONE);
        assert!(at(&h, 1));
        // From partway down a page, back goes to its top.
        h.key(Key::ArrowDown, NONE);
        h.key(Key::ArrowUp, ctrl);
        assert!(at(&h, 1));
        // The minimap shows the page number: no toast.
        assert!(h.doc.toast.is_none());

        // The header's ˅ and ˄ buttons.
        let place =
            |h: &Harness| minimap::Placement::new(h.doc.mini.clone().unwrap(), panel, &h.doc.sy);
        let (prev, next, _) = minimap::header_rects(place(&h).header);
        h.click(next.center(), NONE);
        assert!(at(&h, 2));
        h.click(prev.center(), NONE);
        assert!(at(&h, 1));

        // Clicking a thumbnail goes to its page.
        let ppp = h.ctx.pixels_per_point() as f64;
        let thumb = place(&h).thumb_rect(&l, 4, ppp);
        h.click(thumb.center(), NONE);
        assert!(at(&h, 4), "{}", h.doc.sy.offset);

        // Ctrl+G, a number, Enter.
        h.key(Key::G, ctrl);
        assert!(h.doc.goto.is_some());
        h.idle();
        h.frame(vec![In::Text("3".into())], NONE);
        h.key(Key::Enter, NONE);
        assert!(at(&h, 2), "{}", h.doc.sy.offset);
        assert!(h.doc.goto.is_none());

        // Ctrl+M: the pages take the whole width (a new fit zoom), still at
        // the same place in the document; and back.
        let y = h.doc.sy.offset / zoom;
        h.key(Key::M, ctrl);
        h.idle();
        assert!(!minimap::enabled());
        assert_eq!(h.doc.shown_vp, h.screen);
        let wide = h.doc.shown_zoom.unwrap();
        assert!(wide > zoom);
        assert!((h.doc.sy.offset / wide - y).abs() < 1e-6);
        h.key(Key::M, ctrl);
        h.idle();
        assert!(minimap::enabled());
        assert_eq!(h.doc.shown_zoom, Some(zoom));
        assert!((h.doc.sy.offset / zoom - y).abs() < 1e-6);

        // Dragging the slider to the bottom reaches the end; to the top, the start.
        let p = place(&h);
        let slider = p.slider(&h.doc.sy, &h.doc.sx, zoom, 0.0);
        let (a, b) = (
            slider.center(),
            pos2(slider.center().x, p.area.bottom() + 50.0),
        );
        h.frame(vec![In::PointerMoved(a)], NONE);
        h.button(a, true, NONE);
        for k in 1..=4 {
            h.frame(vec![In::PointerMoved(a.lerp(b, k as f32 / 4.0))], NONE);
        }
        h.button(b, false, NONE);
        assert_eq!(h.doc.sy.offset, h.doc.sy.max_offset());
        let p = place(&h);
        let slider = p.slider(&h.doc.sy, &h.doc.sx, zoom, 0.0);
        let (a, b) = (
            slider.center(),
            pos2(slider.center().x, p.area.top() - 50.0),
        );
        h.frame(vec![In::PointerMoved(a)], NONE);
        h.button(a, true, NONE);
        for k in 1..=4 {
            h.frame(vec![In::PointerMoved(a.lerp(b, k as f32 / 4.0))], NONE);
        }
        h.button(b, false, NONE);
        assert_eq!(h.doc.sy.offset, 0.0);

        // Thumbnails were asked for (without a GPU they come back failed).
        h.until("a thumbnail", |h| {
            h.doc.failed_tiles.iter().any(|k| k.thumb)
        });

        drop(h);
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_file(&path);
    }
}
