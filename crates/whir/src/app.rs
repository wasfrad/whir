//! The shell: tabs, shortcuts, first-frame logic, and rendering.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{CentralPanel, Frame, Key, Modifiers, OutputCommand, Panel, ViewportId};
use whir_core::theme::{self, Theme};
use whir_core::{DocCx, Document, OpenCx, TextureRegistry, trace};
use whir_gfx::{GpuTextures, PresentResult, Renderer};
use whir_platform::{FrameInfo, FrameOutput, Handler, HitMap, RectPx, WindowCommand};

use crate::dispatch;
use crate::tab_bar::{self, TabInfo};

/// How long the first frame may wait for the active document's content, so a
/// double-clicked image appears *with* the window instead of a beat later.
const FIRST_FRAME_CONTENT_WAIT: Duration = Duration::from_millis(150);

struct Tab {
    id: u64,
    doc: Box<dyn Document>,
}

pub struct App {
    ctx: egui::Context,
    renderer: Option<Renderer>,
    open_cx: OpenCx,
    tabs: Vec<Tab>,
    active: usize,
    visible_tab: Option<u64>,
    recently_closed: Vec<PathBuf>,
    next_tab_id: u64,
    theme: Theme,
    shown: bool,
    first_frame_at: Option<Instant>,
    window_title: String,
    fallback_fonts_loaded: bool,
    pending: Vec<WindowCommand>,
    /// Set when the GPU device is lost: `main` relaunches whir with these tabs.
    restart: Rc<RefCell<Option<Vec<PathBuf>>>>,
    /// A close was queued because the last tab closed (not by the user's ✕).
    closing_because_empty: bool,
}

impl App {
    pub fn new(
        textures: Arc<GpuTextures>,
        waker: whir_core::Waker,
        dark: bool,
        restart: Rc<RefCell<Option<Vec<PathBuf>>>>,
    ) -> Self {
        let ctx = egui::Context::default();
        let theme = if dark { Theme::dark() } else { Theme::light() };
        ctx.set_fonts(whir_platform::fonts::definitions(theme::FONTS));
        theme.apply(&ctx);
        Theme::set_current(theme.clone());
        ctx.options_mut(|o| {
            // Viewers own Ctrl+plus/minus/0 for their zoom.
            o.zoom_with_keyboard = false;
        });
        let registry: Arc<dyn TextureRegistry> = textures;
        Self {
            ctx,
            renderer: None,
            open_cx: OpenCx {
                textures: registry,
                waker,
            },
            tabs: Vec::new(),
            active: 0,
            visible_tab: None,
            recently_closed: Vec::new(),
            next_tab_id: 1,
            theme,
            shown: false,
            first_frame_at: None,
            window_title: String::new(),
            fallback_fonts_loaded: false,
            pending: Vec::new(),
            restart,
            closing_because_empty: false,
        }
    }

    pub fn set_renderer(&mut self, renderer: Renderer) {
        self.renderer = Some(renderer);
    }

    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        let mut last = None;
        for path in paths {
            let path = whir_platform::ipc::canonicalize(&path);
            if let Some(i) = self.tabs.iter().position(|t| {
                t.doc
                    .path()
                    .is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
            }) {
                last = Some(i);
                continue;
            }
            let doc = dispatch::open(path, &self.open_cx);
            self.tabs.push(Tab {
                id: self.next_tab_id,
                doc,
            });
            self.next_tab_id += 1;
            last = Some(self.tabs.len() - 1);
        }
        if let Some(i) = last {
            self.active = i;
        }
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        if let Some(p) = tab.doc.path() {
            self.recently_closed.push(p.to_owned());
            if self.recently_closed.len() > 32 {
                self.recently_closed.remove(0);
            }
        }
        drop(tab);
        if self.tabs.is_empty() {
            self.closing_because_empty = true;
            self.pending.push(WindowCommand::Close);
            return;
        }
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn open_dialog_command() -> WindowCommand {
        WindowCommand::OpenFileDialog {
            title: "Open".to_owned(),
            filters: dispatch::open_dialog_filters(),
        }
    }

    fn handle_shortcuts(&mut self, ui: &mut egui::Ui) {
        let n = self.tabs.len();
        let mut close = false;
        let mut step: i32 = 0;
        let mut goto = None;
        let mut reopen = false;
        let mut open = false;
        ui.input_mut(|i| {
            let ctrl = Modifiers::CTRL;
            let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
            if i.consume_key(ctrl_shift, Key::T) {
                reopen = true;
            }
            if i.consume_key(ctrl_shift, Key::Tab) || i.consume_key(ctrl, Key::PageUp) {
                step = -1;
            }
            if i.consume_key(ctrl, Key::Tab) || i.consume_key(ctrl, Key::PageDown) {
                step = 1;
            }
            if i.consume_key(ctrl, Key::W) || i.consume_key(ctrl, Key::F4) {
                close = true;
            }
            if i.consume_key(ctrl, Key::O) {
                open = true;
            }
            const DIGITS: [Key; 9] = [
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
                Key::Num7,
                Key::Num8,
                Key::Num9,
            ];
            for (d, key) in DIGITS.iter().enumerate() {
                if i.consume_key(ctrl, *key) {
                    goto = Some(if d == 8 { n.saturating_sub(1) } else { d });
                }
            }
        });
        if open {
            self.pending.push(Self::open_dialog_command());
        }
        if reopen && let Some(p) = self.recently_closed.pop() {
            self.open_paths(vec![p]);
        }
        if n > 0 {
            if step != 0 {
                self.active = (self.active as i32 + step).rem_euclid(n as i32) as usize;
            }
            if let Some(g) = goto
                && g < n
            {
                self.active = g;
            }
            if close {
                self.close_tab(self.active);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, info: &FrameInfo, hit: &mut HitMap) {
        for tab in &mut self.tabs {
            tab.doc.poll();
        }
        self.handle_shortcuts(ui);
        let theme = self.theme.clone();
        let m = theme.metrics;
        let ppp = info.pixels_per_point;

        // Tab strip / title bar.
        let infos: Vec<TabInfo<'_>> = self
            .tabs
            .iter()
            .map(|t| TabInfo {
                id: t.id,
                title: t.doc.title(),
                tooltip: t.doc.path().map(|p| p.display().to_string()),
                kind: t.doc.kind(),
                loading: t.doc.is_loading(),
            })
            .collect();
        let strip = Panel::top("tab_strip")
            .exact_size(m.tab_strip_height)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(theme.strip_bg))
            .show(ui, |ui| {
                tab_bar::show(ui, &infos, self.active, info.maximized, &theme)
            });
        let bar = strip.inner;
        drop(infos);

        hit.caption_height = (strip.response.rect.bottom() * ppp).round() as i32;
        hit.interactive = bar
            .interactive
            .iter()
            .map(|r| RectPx::from_points(*r, ppp))
            .collect();
        hit.maximize_button = bar.maximize.map(|r| RectPx::from_points(r, ppp));

        if let Some(i) = bar.activate {
            self.active = i;
        }
        if let Some(i) = bar.close {
            self.close_tab(i);
        }
        if bar.open_dialog {
            self.pending.push(Self::open_dialog_command());
        }
        if bar.minimize {
            self.pending.push(WindowCommand::Minimize);
        }
        if bar.toggle_maximize {
            self.pending.push(WindowCommand::ToggleMaximize);
        }
        if bar.close_window {
            self.pending.push(WindowCommand::Close);
        }

        // Visibility bookkeeping.
        let active_id = self.tabs.get(self.active).map(|t| t.id);
        let just_activated = active_id.is_some() && active_id != self.visible_tab;
        if just_activated {
            if let Some(prev) = self.visible_tab
                && let Some(t) = self.tabs.iter_mut().find(|t| t.id == prev)
            {
                t.doc.on_visibility(false);
            }
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.doc.on_visibility(true);
            }
            self.visible_tab = active_id;
        }

        // Content.
        let open_cx = self.open_cx.clone();
        let active = self.active;
        let mut open_requested = false;
        CentralPanel::no_frame()
            .frame(Frame::NONE.fill(theme.content_bg))
            .show(ui, |ui| match self.tabs.get_mut(active) {
                Some(tab) => {
                    let mut cx = DocCx {
                        textures: &open_cx.textures,
                        waker: &open_cx.waker,
                        just_activated,
                    };
                    tab.doc.ui(ui, &mut cx);
                }
                None => open_requested = empty_state(ui, &theme),
            });
        if open_requested {
            self.pending.push(Self::open_dialog_command());
        }

        // Window title (taskbar, Alt+Tab).
        let title = match self.tabs.get(self.active) {
            Some(t) => format!("{} - whir", t.doc.title()),
            None => "whir".to_owned(),
        };
        if title != self.window_title {
            self.pending.push(WindowCommand::SetTitle(title.clone()));
            self.window_title = title;
        }
    }
}

fn empty_state(ui: &mut egui::Ui, theme: &Theme) -> bool {
    let mut clicked = false;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.4 - 40.0).max(16.0));
        ui.label(
            egui::RichText::new("Drop a file here")
                .size(18.0)
                .color(theme.text),
        );
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Images, PDFs and CSV files").color(theme.text_dim));
        ui.add_space(14.0);
        clicked = ui.button("  Open…  ").clicked();
    });
    clicked
}

impl Handler for App {
    fn frame(&mut self, mut input: egui::RawInput, info: &FrameInfo) -> FrameOutput {
        let now = Instant::now();
        let first_frame_at = *self.first_frame_at.get_or_insert_with(|| {
            trace::mark("first frame");
            now
        });

        if !self.fallback_fonts_loaded && theme::fallback_fonts_requested() {
            self.fallback_fonts_loaded = true;
            whir_platform::fonts::add_fallbacks(&self.ctx, theme::LAZY_FALLBACK_FONTS);
        }
        if let Some(r) = &self.renderer {
            input.max_texture_side = Some(r.max_texture_side() as usize);
        }

        let mut hit = HitMap::default();
        let ctx = self.ctx.clone();
        let mut full = ctx.run_ui(input, |ui| self.ui(ui, info, &mut hit));

        let mut out = FrameOutput {
            cursor: full.platform_output.cursor_icon,
            hit_map: hit,
            ..Default::default()
        };
        for cmd in full.platform_output.commands {
            if let OutputCommand::CopyText(text) = cmd {
                out.copied_text = Some(text);
            }
        }
        let delay = full
            .viewport_output
            .get(&ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        if delay < Duration::from_secs(60 * 60) {
            out.repaint_after = Some(delay);
        }
        // Text needing CJK/emoji glyphs appeared this frame: load fallbacks now.
        if !self.fallback_fonts_loaded && theme::fallback_fonts_requested() {
            out.repaint_after = Some(Duration::ZERO);
        }

        if let Some(renderer) = &mut self.renderer {
            if info.minimized {
                renderer.apply_textures(&mut full.textures_delta);
            } else {
                let primitives = ctx.tessellate(full.shapes, full.pixels_per_point);
                let result = renderer.render(
                    &primitives,
                    &mut full.textures_delta,
                    full.pixels_per_point,
                    self.theme.strip_bg,
                    !info.in_size_move,
                );
                if result == PresentResult::DeviceLost {
                    // Driver update / TDR / remoting change: every GPU object is gone.
                    // Relaunch with the same tabs (see main) rather than rebuild in place.
                    // Refreshed every frame until we close, so late-arriving files are kept.
                    let paths = self
                        .tabs
                        .iter()
                        .filter_map(|t| t.doc.path().map(PathBuf::from))
                        .collect();
                    *self.restart.borrow_mut() = Some(paths);
                    self.pending.push(WindowCommand::Close);
                }
            }
        }

        // Reveal the window once the first document has content (or we stop waiting).
        if !self.shown {
            let loading = self
                .tabs
                .get(self.active)
                .is_some_and(|t| t.doc.is_loading());
            let waited = now.duration_since(first_frame_at);
            if !loading || waited >= FIRST_FRAME_CONTENT_WAIT {
                self.shown = true;
                out.commands.push(WindowCommand::Show);
                trace::mark(if loading {
                    "shown (content pending)"
                } else {
                    "shown with content"
                });
                trace::flush();
            } else {
                let remaining = FIRST_FRAME_CONTENT_WAIT - waited;
                out.repaint_after = Some(out.repaint_after.map_or(remaining, |d| d.min(remaining)));
            }
        }

        out.commands.append(&mut self.pending);
        out
    }

    fn resized(&mut self, size_px: [u32; 2]) {
        if let Some(r) = &mut self.renderer {
            r.resize(size_px);
        }
    }

    fn open_files(&mut self, paths: Vec<PathBuf>) {
        self.open_paths(paths);
    }

    fn close_requested(&mut self) -> bool {
        // "Close because empty" is void if files arrived in the meantime.
        let cancel =
            self.closing_because_empty && !self.tabs.is_empty() && self.restart.borrow().is_none();
        self.closing_because_empty = false;
        !cancel
    }

    fn activated(&mut self, active: bool) {
        if !active {
            // Let other apps save/replace files we have open.
            whir_core::file::release_all();
        }
    }

    fn system_theme_changed(&mut self) {
        let dark = whir_platform::os::prefers_dark();
        if dark != self.theme.dark {
            self.theme = if dark { Theme::dark() } else { Theme::light() };
            self.theme.apply(&self.ctx);
            Theme::set_current(self.theme.clone());
            self.pending.push(WindowCommand::SetDarkMode(dark));
        }
    }
}
