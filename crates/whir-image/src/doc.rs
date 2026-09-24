//! The image tab.
//!
//! Memory model: every image is shown from a *fit level* no larger than the
//! screen (JPEGs decode straight to that size via DCT scaling). Full
//! resolution is decoded only when the user zooms past what the fit level can
//! show, and dropped again when the tab is hidden.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use egui::epaint::Vertex;
use egui::{
    Color32, ColorImage, CursorIcon, Key, Mesh, Modifiers, Rect, Sense, TextureOptions, pos2, vec2,
};
use whir_core::theme::Theme;
use whir_core::{DocCx, DocKind, Document, OpenCx, TextureRegistry, Waker};

use crate::pool::{self, JobHandle, Priority};
use crate::tiles::{self, Level};
use crate::view::{self, Mode, View};
use crate::{decode, folder};

enum Msg {
    Loaded {
        generation: u64,
        full: bool,
        result: Result<Level, String>,
    },
    Prefetched {
        path: PathBuf,
        level: Level,
    },
    Folder(Vec<PathBuf>),
}

struct Shown {
    path: PathBuf,
    fit: Arc<Level>,
    full: Option<Arc<Level>>,
}

impl Shown {
    fn full_size(&self) -> egui::Vec2 {
        let [w, h] = self.fit.full_size;
        vec2(w as f32, h as f32)
    }

    fn best(&self) -> &Level {
        self.full.as_deref().unwrap_or(&self.fit)
    }
}

pub struct ImageDoc {
    path: PathBuf,
    title: String,
    textures: Arc<dyn TextureRegistry>,
    waker: Waker,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    generation: u64,
    shown: Option<Shown>,
    loading: bool,
    error: Option<String>,
    fit_job: Option<JobHandle>,
    full_job: Option<JobHandle>,
    /// Full resolution couldn't be decoded/uploaded for the current image.
    full_failed: bool,
    visible: bool,
    view: View,
    folder: Option<Vec<PathBuf>>,
    folder_requested: bool,
    /// The user has navigated with ←/→ at least once.
    browsing: bool,
    /// Pre-decoded neighbours (fit level), for instant ←/→.
    cache: Vec<(PathBuf, Arc<Level>)>,
    prefetching: Vec<PathBuf>,
    /// Transient "1920 × 1080 · 50%" pill: text and when it hides.
    toast: Option<(String, f64)>,
    checker: Option<egui::TextureHandle>,
    /// Quarter turns clockwise per image (non-zero only), for as long as the
    /// tab is open. Only the screen turns: files are never changed.
    rotations: Vec<(PathBuf, u8)>,
}

/// Longest side of the fit level: the screen's longer side (physical px).
fn fit_cap() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    (w.max(h).max(1280)) as u32
}

impl ImageDoc {
    pub fn open(path: PathBuf, cx: &OpenCx) -> Self {
        let (tx, rx) = channel();
        let mut doc = Self {
            title: file_title(&path),
            path,
            textures: cx.textures.clone(),
            waker: cx.waker.clone(),
            tx,
            rx,
            generation: 0,
            shown: None,
            loading: true,
            error: None,
            fit_job: None,
            full_job: None,
            full_failed: false,
            visible: false,
            view: View::default(),
            folder: None,
            folder_requested: false,
            browsing: false,
            cache: Vec::new(),
            prefetching: Vec::new(),
            toast: None,
            checker: None,
            rotations: Vec::new(),
        };
        // Newly opened documents are almost always the active tab: decode now.
        doc.start_fit_decode(Priority::Visible);
        doc
    }

    fn decode_job(&self, path: PathBuf, full: bool, priority: Priority) -> JobHandle {
        let (tx, waker, textures, generation) = (
            self.tx.clone(),
            self.waker.clone(),
            self.textures.clone(),
            self.generation,
        );
        pool::submit(priority, move || {
            let max = if full { None } else { Some(fit_cap()) };
            let result = decode::decode(&path, max)
                .map_err(|e| e.to_string())
                .and_then(|d| tiles::upload(&textures, &d));
            let _ = tx.send(Msg::Loaded {
                generation,
                full,
                result,
            });
            waker.wake();
        })
    }

    fn start_fit_decode(&mut self, priority: Priority) {
        self.loading = true;
        self.fit_job = Some(self.decode_job(self.path.clone(), false, priority));
    }

    fn request_full(&mut self) {
        if self.full_job.is_some() || self.full_failed {
            return;
        }
        let Some(shown) = &self.shown else { return };
        // While ←/→ is still decoding the next image, `shown` is the previous one.
        if !folder::same_path(&shown.path, &self.path) {
            return;
        }
        if shown.fit.is_full_resolution() || shown.full.is_some() {
            return;
        }
        self.full_job = Some(self.decode_job(shown.path.clone(), true, Priority::Visible));
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Loaded {
                    generation,
                    full,
                    result,
                } => {
                    if generation != self.generation {
                        continue;
                    }
                    match result {
                        Ok(level) if full => {
                            self.full_job = None;
                            if let Some(s) = &mut self.shown
                                && folder::same_path(&s.path, &self.path)
                            {
                                s.full = Some(Arc::new(level));
                            }
                        }
                        Ok(level) => {
                            self.fit_job = None;
                            self.loading = false;
                            self.error = None;
                            let level = Arc::new(level);
                            let full = level.is_full_resolution().then(|| level.clone());
                            self.shown = Some(Shown {
                                path: self.path.clone(),
                                fit: level,
                                full,
                            });
                            self.prefetch_neighbours();
                        }
                        Err(e) => {
                            if full {
                                // Keep showing the fit level; don't retry every frame.
                                self.full_job = None;
                                self.full_failed = true;
                            } else {
                                self.fit_job = None;
                                self.loading = false;
                                self.shown = None;
                                self.error = Some(e);
                            }
                        }
                    }
                }
                Msg::Prefetched { path, level } => {
                    self.prefetching.retain(|p| !folder::same_path(p, &path));
                    if self.visible && self.is_neighbour(&path) {
                        self.cache.push((path, Arc::new(level)));
                        self.trim_cache();
                    }
                }
                Msg::Folder(list) => {
                    self.folder = Some(list);
                    self.prefetch_neighbours();
                }
            }
        }
    }

    // --- folder navigation ------------------------------------------------------

    fn ensure_folder(&mut self) {
        if self.folder_requested {
            return;
        }
        self.folder_requested = true;
        let Some(dir) = self.path.parent().map(Path::to_owned) else {
            return;
        };
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        std::thread::Builder::new()
            .name("image-folder".into())
            .spawn(move || {
                let _ = tx.send(Msg::Folder(folder::list_images(&dir)));
                waker.wake();
            })
            .ok();
    }

    fn index_in_folder(&self) -> Option<usize> {
        self.folder
            .as_ref()?
            .iter()
            .position(|p| folder::same_path(p, &self.path))
    }

    fn neighbours(&self) -> Vec<PathBuf> {
        let (Some(list), Some(i)) = (&self.folder, self.index_in_folder()) else {
            return Vec::new();
        };
        [i.checked_add(1), i.checked_sub(1)]
            .into_iter()
            .flatten()
            .filter_map(|j| list.get(j).cloned())
            .collect()
    }

    fn is_neighbour(&self, path: &Path) -> bool {
        self.neighbours().iter().any(|p| folder::same_path(p, path))
    }

    fn trim_cache(&mut self) {
        let keep = self.neighbours();
        self.cache
            .retain(|(p, _)| keep.iter().any(|k| folder::same_path(k, p)));
    }

    fn prefetch_neighbours(&mut self) {
        // Only once the user is browsing (has pressed ←/→): opening a single
        // image shouldn't pay memory for its neighbours.
        if !self.browsing || !self.visible || self.shown.is_none() {
            return;
        }
        self.trim_cache();
        for path in self.neighbours() {
            let cached = self.cache.iter().any(|(p, _)| folder::same_path(p, &path));
            let pending = self.prefetching.iter().any(|p| folder::same_path(p, &path));
            if cached || pending {
                continue;
            }
            self.prefetching.push(path.clone());
            let (tx, waker, textures) =
                (self.tx.clone(), self.waker.clone(), self.textures.clone());
            pool::submit(Priority::Prefetch, move || {
                let level = decode::decode(&path, Some(fit_cap()))
                    .ok()
                    .and_then(|d| tiles::upload(&textures, &d).ok());
                if let Some(level) = level {
                    let _ = tx.send(Msg::Prefetched { path, level });
                    waker.wake();
                }
            });
        }
    }

    fn navigate(&mut self, delta: isize, time: f64) {
        self.browsing = true;
        let (Some(list), Some(i)) = (&self.folder, self.index_in_folder()) else {
            return;
        };
        let target = match delta {
            isize::MIN => 0,
            isize::MAX => list.len() - 1,
            d => (i as isize + d).clamp(0, list.len() as isize - 1) as usize,
        };
        if target == i {
            return;
        }
        let path = list[target].clone();
        let position = format!("{} / {}", target + 1, list.len());
        self.go_to(path);
        self.toast = Some((position, time + 1.2));
    }

    fn go_to(&mut self, path: PathBuf) {
        if let Some(j) = self.fit_job.take() {
            j.cancel();
        }
        if let Some(j) = self.full_job.take() {
            j.cancel();
        }
        self.generation += 1;
        self.full_failed = false;
        self.title = file_title(&path);
        self.path = path;
        self.view = View::default();
        self.error = None;
        let cached = self
            .cache
            .iter()
            .position(|(p, _)| folder::same_path(p, &self.path))
            .map(|i| self.cache.swap_remove(i).1);
        match cached {
            Some(level) => {
                let full = level.is_full_resolution().then(|| level.clone());
                // The outgoing image becomes a neighbour: keep it for instant back-navigation.
                if let Some(old) = self.shown.take() {
                    self.cache.push((old.path, old.fit));
                }
                self.shown = Some(Shown {
                    path: self.path.clone(),
                    fit: level,
                    full,
                });
                self.loading = false;
                self.prefetch_neighbours();
            }
            // Keep showing the previous image until the new one is decoded.
            None => self.start_fit_decode(Priority::Visible),
        }
    }

    // --- rotation ----------------------------------------------------------------

    /// Quarter turns clockwise of the image on screen.
    fn turns(&self) -> u8 {
        let Some(shown) = &self.shown else { return 0 };
        self.rotations
            .iter()
            .find(|(p, _)| folder::same_path(p, &shown.path))
            .map_or(0, |r| r.1)
    }

    /// `image` is the on-screen size before the turn.
    fn rotate(&mut self, clockwise: bool, image: egui::Vec2, vp: egui::Vec2, ppp: f32, time: f64) {
        let Some(path) = self.shown.as_ref().map(|s| s.path.clone()) else {
            return;
        };
        let turns = (self.turns() + if clockwise { 1 } else { 3 }) % 4;
        self.rotations.retain(|(p, _)| !folder::same_path(p, &path));
        if turns != 0 {
            self.rotations.push((path, turns));
        }
        self.view.rotate(clockwise, image, vp, ppp);
        let label = if clockwise {
            "Rotated right"
        } else {
            "Rotated left"
        };
        self.toast = Some((label.to_owned(), time + 1.0));
    }

    // --- drawing -----------------------------------------------------------------

    fn checker(&mut self, ctx: &egui::Context, theme: &Theme) -> egui::TextureId {
        self.checker
            .get_or_insert_with(|| {
                let (a, b) = (theme.checker_a, theme.checker_b);
                ctx.load_texture(
                    "whir-checker",
                    ColorImage::new([2, 2], vec![a, b, b, a]),
                    TextureOptions::NEAREST_REPEAT,
                )
            })
            .id()
    }

    fn handle_input(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        viewport: Rect,
        image: egui::Vec2,
    ) {
        let ppp = ui.ctx().pixels_per_point();
        let vp = viewport.size();
        let time = ui.input(|i| i.time);
        let before = self.view.zoom(image, vp, ppp);

        // Wheel zooms around the cursor (smoothed by egui); Ctrl+wheel / pinch too.
        if resp.hovered() {
            let (scroll, zoom_delta, pointer) =
                ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
            let mut factor = zoom_delta;
            if scroll.y != 0.0 {
                factor *= (scroll.y / 200.0).exp();
            }
            if factor != 1.0 {
                let anchor = pointer.map_or(egui::Vec2::ZERO, |p| p - viewport.center());
                self.view.zoom_by(factor, anchor, image, vp, ppp);
            }
        }
        if resp.dragged_by(egui::PointerButton::Primary)
            || resp.dragged_by(egui::PointerButton::Middle)
        {
            self.view.pan(resp.drag_delta(), image, vp, ppp);
        }
        if resp.double_clicked() {
            let anchor = resp
                .interact_pointer_pos()
                .map_or(egui::Vec2::ZERO, |p| p - viewport.center());
            match self.view.mode {
                Mode::Fit if view::fit_zoom(image, vp, ppp) < view::actual_size_zoom(ppp) => {
                    self.view
                        .set_zoom(view::actual_size_zoom(ppp), anchor, image, vp, ppp);
                }
                _ => self.view.mode = Mode::Fit,
            }
        }

        // Keyboard (this tab is active whenever ui() runs).
        let mut nav = 0isize;
        let mut turn = None;
        ui.input_mut(|i| {
            let none = Modifiers::NONE;
            if i.consume_key(none, Key::ArrowRight) || i.consume_key(none, Key::PageDown) {
                nav = 1;
            }
            if i.consume_key(none, Key::ArrowLeft) || i.consume_key(none, Key::PageUp) {
                nav = -1;
            }
            if i.consume_key(none, Key::Home) {
                nav = isize::MIN;
            }
            if i.consume_key(none, Key::End) {
                nav = isize::MAX;
            }
            if i.consume_key(none, Key::Num0) || i.consume_key(Modifiers::CTRL, Key::Num0) {
                self.view.mode = Mode::Fit;
            }
            if i.consume_key(none, Key::Num1) {
                self.view.set_zoom(
                    view::actual_size_zoom(ppp),
                    egui::Vec2::ZERO,
                    image,
                    vp,
                    ppp,
                );
            }
            let zoom_in = i.consume_key(none, Key::Plus)
                || i.consume_key(none, Key::Equals)
                || i.consume_key(Modifiers::CTRL, Key::Plus)
                || i.consume_key(Modifiers::CTRL, Key::Equals);
            let zoom_out =
                i.consume_key(none, Key::Minus) || i.consume_key(Modifiers::CTRL, Key::Minus);
            if zoom_in {
                self.view.zoom_by(1.25, egui::Vec2::ZERO, image, vp, ppp);
            }
            if zoom_out {
                self.view.zoom_by(0.8, egui::Vec2::ZERO, image, vp, ppp);
            }
            // Shift+R first: a pattern without Shift also matches with it.
            if i.consume_key(Modifiers::SHIFT, Key::R) {
                turn = Some(false);
            } else if i.consume_key(none, Key::R) {
                turn = Some(true);
            }
        });
        resp.context_menu(|ui| {
            let right = egui::Button::new("Rotate right").shortcut_text("R");
            if ui.add(right).clicked() {
                turn = Some(true);
                ui.close();
            }
            let left = egui::Button::new("Rotate left").shortcut_text("Shift+R");
            if ui.add(left).clicked() {
                turn = Some(false);
                ui.close();
            }
        });
        if nav != 0 {
            self.ensure_folder();
            self.navigate(nav, time);
            return;
        }
        if let Some(clockwise) = turn {
            self.rotate(clockwise, image, vp, ppp, time);
            return;
        }

        let after = self.view.zoom(image, vp, ppp);
        if (after - before).abs() > f32::EPSILON {
            let pct = (after * ppp * 100.0).round();
            self.toast = Some((format!("{} × {}  ·  {pct}%", image.x, image.y), time + 1.0));
        }
    }

    fn paint(&mut self, ui: &egui::Ui, viewport: Rect, theme: &Theme) {
        let ppp = ui.ctx().pixels_per_point();
        let turns = self.turns();
        let Some(shown) = &self.shown else { return };
        let image = view::turned(shown.full_size(), turns);
        let rect = self.view.image_rect(viewport, image, ppp);
        let painter = ui.painter().with_clip_rect(viewport);

        // Need more pixels than the fit level holds? Fetch full resolution.
        let screen_px_per_level_px =
            self.view.zoom(image, viewport.size(), ppp) * ppp / shown.fit.scale();
        let wants_full = screen_px_per_level_px > 1.05;

        if shown.best().has_alpha {
            let cell = 8.0;
            let uv = Rect::from_min_max(
                pos2(0.0, 0.0),
                pos2(rect.width() / (2.0 * cell), rect.height() / (2.0 * cell)),
            );
            let visible = rect.intersect(viewport);
            let clip = painter.with_clip_rect(visible);
            let checker = self.checker(ui.ctx(), theme);
            clip.image(checker, rect, uv, Color32::WHITE);
        }

        let shown = self.shown.as_ref().expect("checked above");
        let level = shown.best();
        // A level pixel corner → screen, through the quarter turns.
        let [lw, lh] = level.size.map(|v| v as f32);
        let to_screen = |x: u32, y: u32| {
            let p = view::turn_point(pos2(x as f32 / lw, y as f32 / lh), turns);
            rect.min + p.to_vec2() * rect.size()
        };
        for tile in &level.tiles {
            let ([x0, y0], [w, h]) = (tile.origin, tile.size);
            let corners = [
                to_screen(x0, y0),
                to_screen(x0 + w, y0),
                to_screen(x0 + w, y0 + h),
                to_screen(x0, y0 + h),
            ];
            if !Rect::from_points(&corners).intersects(viewport) {
                continue;
            }
            let uvs = [
                pos2(0.0, 0.0),
                pos2(1.0, 0.0),
                pos2(1.0, 1.0),
                pos2(0.0, 1.0),
            ];
            let mut mesh = Mesh::with_texture(tile.texture.id());
            for (pos, uv) in corners.into_iter().zip(uvs) {
                mesh.vertices.push(Vertex {
                    pos,
                    uv,
                    color: Color32::WHITE,
                });
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(mesh);
        }
        if wants_full && self.visible {
            self.request_full();
        }
    }

    fn paint_toast(&mut self, ui: &egui::Ui, viewport: Rect, theme: &Theme) {
        let Some((text, until)) = &self.toast else {
            return;
        };
        let now = ui.input(|i| i.time);
        if now >= *until {
            self.toast = None;
            return;
        }
        let alpha = ((until - now) / 0.25).clamp(0.0, 1.0) as f32;
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(
            text.clone(),
            egui::FontId::proportional(12.5),
            theme.text.gamma_multiply(alpha),
        );
        let pad = vec2(12.0, 6.0);
        let size = galley.size() + pad * 2.0;
        let rect = Rect::from_center_size(
            pos2(viewport.center().x, viewport.bottom() - 28.0 - size.y / 2.0),
            size,
        );
        painter.rect_filled(rect, size.y / 2.0, theme.overlay_bg.gamma_multiply(alpha));
        painter.galley(rect.min + pad, galley, theme.text);
        ui.ctx().request_repaint_after(
            Duration::from_millis(30).min(Duration::from_secs_f64(until - now)),
        );
    }
}

fn file_title(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl Document for ImageDoc {
    fn title(&self) -> &str {
        &self.title
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn kind(&self) -> DocKind {
        DocKind::Image
    }

    fn is_loading(&self) -> bool {
        self.loading && self.error.is_none()
    }

    fn on_visibility(&mut self, visible: bool) {
        self.visible = visible;
        let priority = if visible {
            Priority::Visible
        } else {
            Priority::Background
        };
        if let Some(j) = &self.fit_job {
            j.set_priority(priority);
        }
        if visible {
            self.ensure_folder();
            self.prefetch_neighbours();
        } else {
            // Background tabs keep only their fit level.
            if let Some(j) = self.full_job.take() {
                j.cancel();
            }
            if let Some(s) = &mut self.shown
                && !s.fit.is_full_resolution()
            {
                s.full = None;
            }
            self.cache.clear();
        }
    }

    fn poll(&mut self) {
        self.drain();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _cx: &mut DocCx<'_>) {
        self.drain();
        let theme = Theme::current();
        let (viewport, resp) =
            ui.allocate_exact_size(ui.available_size(), Sense::CLICK | Sense::DRAG);
        ui.painter().rect_filled(viewport, 0.0, theme.canvas_bg);

        if let Some(size) = self.shown.as_ref().map(Shown::full_size) {
            let image = view::turned(size, self.turns());
            self.handle_input(ui, &resp, viewport, image);
            self.paint(ui, viewport, &theme);
            let ppp = ui.ctx().pixels_per_point();
            if self.view.pannable(image, viewport.size(), ppp) {
                ui.ctx().set_cursor_icon(if resp.dragged() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                });
            }
        } else if let Some(err) = &self.error {
            whir_core::message_doc::show_message(
                ui,
                "Can't show this image",
                err,
                Some(&self.path),
            );
            // Still allow ←/→ past a broken file.
            let (mut nav, time) = (0isize, ui.input(|i| i.time));
            ui.input_mut(|i| {
                if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                    nav = 1;
                }
                if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                    nav = -1;
                }
            });
            if nav != 0 {
                self.ensure_folder();
                self.navigate(nav, time);
            }
        }
        if self.loading && self.shown.is_none() && self.error.is_none() {
            ui.put(
                Rect::from_center_size(viewport.center(), vec2(24.0, 24.0)),
                egui::Spinner::new().size(20.0).color(theme.text_dim),
            );
        }
        self.paint_toast(ui, viewport, &theme);
    }
}

impl Drop for ImageDoc {
    fn drop(&mut self) {
        if let Some(j) = self.fit_job.take() {
            j.cancel();
        }
        if let Some(j) = self.full_job.take() {
            j.cancel();
        }
    }
}
