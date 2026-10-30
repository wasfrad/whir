//! The shell: tabs, shortcuts, saving and closing, the drawer along the
//! bottom, first-frame logic, and rendering.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{CentralPanel, Event, Frame, Key, Modifiers, OutputCommand, Panel, ViewportId};
use whir_core::theme::{self, Theme};
use whir_core::{
    DocCx, DocKind, DocRequest, Document, FindQuery, OpenCx, SaveAsSpec, SaveResult,
    TextureRegistry, needs_attention, trace,
};
use whir_gfx::{GpuTextures, PresentResult, Renderer};
use whir_platform::ipc::Changed;
use whir_platform::{
    FrameInfo, FrameOutput, Handler, HitMap, RectPx, SaveDialogSpec, WindowCommand,
};

use crate::close_flow::{self, CloseFlow, Outcome};
use crate::dispatch;
use crate::drawer::{self, Drawer};
use crate::keys::consume_exact;
use crate::search_panel::{self, SearchPanel};
use crate::settings::{Section, SettingsDoc};
use crate::tab_bar::{self, TabInfo};

/// How long the first frame may wait for the active document's content, so a
/// double-clicked image appears *with* the window instead of a beat later.
const FIRST_FRAME_CONTENT_WAIT: Duration = Duration::from_millis(150);
/// The text caret stops blinking this long after the last input, so an idle
/// whir with a note open uses no CPU.
const BLINK_FOR: f64 = 5.0;

pub(crate) struct Tab {
    pub id: u64,
    pub doc: Box<dyn Document>,
}

/// Why a Save dialog is open.
enum DialogFor {
    /// Ctrl+S on a document without a location, or Ctrl+Shift+S.
    SaveAs(u64),
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
    rich_fonts_loaded: bool,
    pending: Vec<WindowCommand>,
    /// Set when the GPU device is lost: `main` relaunches whir with these tabs.
    restart: Rc<RefCell<Option<Vec<PathBuf>>>>,
    /// A close was queued because the last tab closed (not by the user's ✕).
    closing_because_empty: bool,

    /// Closed tabs whose last save is still being written.
    zombies: Vec<Tab>,
    close_flow: Option<CloseFlow>,
    /// The user settled every unsaved document: the window may close.
    close_confirmed: bool,
    /// Close once background saves finish (exiting would cut them off).
    close_when_idle: bool,
    dialogs: Vec<(u64, DialogFor)>,
    next_token: u64,
    drawer: Drawer,
    /// Find in all tabs, along the bottom.
    search: SearchPanel,
    library_scanned: bool,
    toast: Option<(String, f64)>,
    blink_on: bool,
    last_input: f64,
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
        let (double_click, blink_ms) = unsafe {
            (
                windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime(),
                windows::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime(),
            )
        };
        ctx.options_mut(|o| {
            // Viewers own Ctrl+plus/minus/0 for their zoom.
            o.zoom_with_keyboard = false;
            // Double/triple clicks follow the Windows setting.
            if (100..=5000).contains(&double_click) {
                o.input_options.max_double_click_delay = double_click as f64 / 1000.0;
            }
        });
        if (100..=5000).contains(&blink_ms) {
            let secs = blink_ms as f32 / 1000.0;
            ctx.all_styles_mut(|s| {
                s.visuals.text_cursor.on_duration = secs;
                s.visuals.text_cursor.off_duration = secs;
            });
        }
        let registry: Arc<dyn TextureRegistry> = textures;
        Self {
            ctx,
            renderer: None,
            open_cx: OpenCx {
                textures: registry,
                library: whir_core::Library::new(waker.clone()),
                captures: whir_core::Captures::new(waker.clone()),
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
            rich_fonts_loaded: false,
            pending: Vec::new(),
            restart,
            closing_because_empty: false,
            zombies: Vec::new(),
            close_flow: None,
            close_confirmed: false,
            close_when_idle: false,
            dialogs: Vec::new(),
            next_token: 0,
            drawer: Drawer::default(),
            search: SearchPanel::default(),
            library_scanned: false,
            toast: None,
            blink_on: true,
            last_input: 0.0,
        }
    }

    pub fn set_renderer(&mut self, renderer: Renderer) {
        self.renderer = Some(renderer);
    }

    fn push_tab(&mut self, doc: Box<dyn Document>) -> usize {
        self.tabs.push(Tab {
            id: self.next_tab_id,
            doc,
        });
        self.next_tab_id += 1;
        self.tabs.len() - 1
    }

    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        // A lone untouched blank note gives way to what the user opened.
        if self.tabs.len() == 1 && self.tabs[0].doc.is_pristine() {
            self.tabs.clear();
            self.active = 0;
            self.visible_tab = None;
        }
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
            last = Some(self.push_tab(doc));
        }
        if let Some(i) = last {
            self.active = i;
        }
    }

    /// Ctrl+N: a fresh note (or the untouched one that's already open).
    fn new_note(&mut self) {
        if let Some(i) = self.tabs.iter().position(|t| t.doc.is_pristine()) {
            self.active = i;
            return;
        }
        let doc = whir_notes::new_note(&self.open_cx);
        self.active = self.push_tab(doc);
    }

    /// Settings, at `page` ("timesheet", "timers", "startup"): a tab of its
    /// own, the one already open if there is one.
    pub fn open_settings(&mut self, page: &str) {
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.doc.kind() == DocKind::Settings)
        {
            self.active = i;
            self.tabs[i].doc.show_section(page);
            return;
        }
        // A lone untouched blank note gives way, as it does to opened files.
        if self.tabs.len() == 1 && self.tabs[0].doc.is_pristine() {
            self.tabs.clear();
            self.visible_tab = None;
        }
        let section = Section::from_name(page).unwrap_or(Section::Timesheet);
        self.active = self.push_tab(SettingsDoc::new(section).boxed());
    }

    /// Ctrl+Shift+N: a blank table.
    fn new_table(&mut self) {
        let doc = whir_csv::new_blank(&self.open_cx);
        self.active = self.push_tab(doc);
    }

    /// Close a tab now, letting an in-flight save finish in the background.
    /// Closing the last one closes whir.
    fn close_tab_now(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        self.remove_tab(index);
        if self.tabs.is_empty() {
            self.closing_because_empty = true;
            self.pending.push(WindowCommand::Close);
        }
    }

    fn remove_tab(&mut self, index: usize) {
        let mut tab = self.tabs.remove(index);
        tab.doc.flush();
        if let Some(p) = tab.doc.path() {
            self.recently_closed.push(p.to_owned());
            if self.recently_closed.len() > 32 {
                self.recently_closed.remove(0);
            }
        }
        if tab.doc.is_saving() {
            self.zombies.push(tab);
        } else {
            drop(tab);
        }
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// The user asked to close a tab: ask first if it has unsaved work.
    fn request_close_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if self.close_flow.is_some() {
            return;
        }
        if needs_attention(&*tab.doc) {
            let focus = self.ctx.memory(|m| m.focused());
            self.active = index;
            self.close_flow = Some(CloseFlow::new(
                close_flow::Kind::Tabs,
                vec![tab.id],
                vec![tab.id],
                focus,
            ));
        } else {
            self.close_tab_now(index);
        }
    }

    /// The tab menu: close several tabs, asking once about their unsaved work.
    fn request_close_tabs(&mut self, ids: Vec<u64>, kind: close_flow::Kind) {
        if self.close_flow.is_some() || ids.is_empty() {
            return;
        }
        let queue: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| ids.contains(&t.id) && needs_attention(&*t.doc))
            .map(|t| t.id)
            .collect();
        if queue.is_empty() {
            self.close_tabs_by_id(&ids, kind);
        } else {
            let focus = self.ctx.memory(|m| m.focused());
            self.close_flow = Some(CloseFlow::new(kind, queue, ids, focus));
        }
    }

    fn close_tabs_by_id(&mut self, ids: &[u64], kind: close_flow::Kind) {
        for id in ids {
            let Some(i) = self.tabs.iter().position(|t| t.id == *id) else {
                continue;
            };
            if kind == close_flow::Kind::AllTabs {
                self.remove_tab(i);
            } else {
                self.close_tab_now(i);
            }
        }
        // "Close all" leaves a blank note, as a bare launch does.
        if self.tabs.is_empty() && kind == close_flow::Kind::AllTabs {
            self.new_note();
        }
    }

    fn ask_save_path(&mut self, tab: u64, spec: SaveAsSpec) {
        self.next_token += 1;
        let token = self.next_token;
        self.dialogs.push((token, DialogFor::SaveAs(tab)));
        self.pending.push(save_dialog_command(token, spec));
    }

    fn save_active(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let id = tab.id;
        match tab.doc.save() {
            SaveResult::NeedsPath(spec) => self.ask_save_path(id, spec),
            SaveResult::Failed(e) => self.show_toast(e),
            SaveResult::Saved | SaveResult::Started | SaveResult::Unsupported => {}
        }
    }

    fn save_active_as(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if let Some(spec) = tab.doc.save_as_spec() {
            self.ask_save_path(tab.id, spec);
        }
    }

    /// Search every table for `q` (Find in all tabs).
    fn find_in_tabs(&mut self, q: FindQuery) {
        let ids = self
            .tabs
            .iter_mut()
            .filter(|t| t.doc.searchable())
            .map(|t| {
                t.doc.search(&q);
                t.id
            })
            .collect();
        self.search.started(q, ids);
        self.ctx.request_repaint();
    }

    /// A result was chosen: show its tab.
    fn activate_tab(&mut self, id: u64) {
        if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
            self.active = i;
        }
    }

    /// The drawer the active tab has of its own (screenshots, notes).
    fn own_drawer(&self) -> Option<drawer::Mode> {
        let t = self.tabs.get(self.active)?;
        drawer::mode_for(t.doc.kind(), t.doc.path(), &self.open_cx)
    }

    /// Show the capture dock (a process of its own), starting it if needed.
    fn open_dock(&mut self) {
        if let Err(e) = whir_capture::launch(whir_capture::Command::Reveal) {
            self.show_toast(format!("The capture dock couldn't be started: {e}"));
        }
    }

    fn show_toast(&mut self, text: impl Into<String>) {
        let now = self.ctx.input(|i| i.time);
        self.toast = Some((text.into(), now + 3.0));
        self.ctx.request_repaint();
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
        let (mut new_note, mut new_table, mut save, mut save_as, mut drawer) =
            (false, false, false, false, false);
        let mut settings = false;
        let (mut find_in_tabs, mut result_step) = (false, None);
        ui.input_mut(|i| {
            let ctrl = Modifiers::CTRL;
            let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
            if consume_exact(i, ctrl_shift, Key::T) {
                reopen = true;
            }
            if consume_exact(i, ctrl_shift, Key::Tab) || consume_exact(i, ctrl, Key::PageUp) {
                step = -1;
            }
            if consume_exact(i, ctrl, Key::Tab) || consume_exact(i, ctrl, Key::PageDown) {
                step = 1;
            }
            if consume_exact(i, ctrl, Key::W) || consume_exact(i, ctrl, Key::F4) {
                close = true;
            }
            if consume_exact(i, ctrl, Key::O) {
                open = true;
            }
            new_table = consume_exact(i, ctrl_shift, Key::N);
            new_note = consume_exact(i, ctrl, Key::N);
            save_as = consume_exact(i, ctrl_shift, Key::S);
            save = consume_exact(i, ctrl, Key::S);
            drawer = consume_exact(i, ctrl, Key::E);
            settings = consume_exact(i, ctrl, Key::Comma);
            find_in_tabs = consume_exact(i, ctrl_shift, Key::F);
            if consume_exact(i, Modifiers::SHIFT, Key::F4) {
                result_step = Some(false);
            }
            if consume_exact(i, Modifiers::NONE, Key::F4) {
                result_step = Some(true);
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
                if consume_exact(i, ctrl, *key) {
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
        if new_note {
            self.new_note();
        }
        if new_table {
            self.new_table();
        }
        if save {
            self.save_active();
        }
        if save_as {
            self.save_active_as();
        }
        if drawer {
            let tab = self.tabs.get(self.active).map(|t| t.id);
            let own = self.own_drawer();
            self.drawer.toggle(tab, own);
        }
        if find_in_tabs {
            self.search.focus();
        }
        if settings {
            self.open_settings("");
        }
        if let Some(forward) = result_step
            && let Some(id) = self.search.step(&mut self.tabs, forward)
        {
            self.activate_tab(id);
        }
        let n = self.tabs.len();
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
                self.request_close_tab(self.active);
            }
        }
    }

    fn poll_zombies(&mut self) {
        let mut i = 0;
        while i < self.zombies.len() {
            self.zombies[i].doc.poll();
            if self.zombies[i].doc.is_saving() {
                i += 1;
                continue;
            }
            let z = self.zombies.remove(i);
            if z.doc.save_error().is_some() {
                // The last save failed: bring the document back so it isn't lost.
                self.tabs.push(z);
                self.active = self.tabs.len() - 1;
                self.closing_because_empty = false;
            }
        }
    }

    /// The caret blinks only while the user is active.
    fn update_blink(&mut self, ui: &egui::Ui) {
        let now = ui.input(|i| i.time);
        let active_input = ui.input(|i| {
            i.events.iter().any(|e| {
                !matches!(
                    e,
                    Event::WindowFocused(_) | Event::PointerGone | Event::ModifiersChanged(_)
                )
            })
        });
        if active_input {
            self.last_input = now;
        }
        let want = now - self.last_input < BLINK_FOR;
        if want != self.blink_on {
            self.blink_on = want;
            self.ctx
                .all_styles_mut(|s| s.visuals.text_cursor.blink = want);
        }
        if want {
            self.ctx.request_repaint_after(Duration::from_secs_f64(
                (self.last_input + BLINK_FOR - now).max(0.05),
            ));
        }
    }

    fn run_close_flow(&mut self, ui: &mut egui::Ui) {
        let Some(mut flow) = self.close_flow.take() else {
            return;
        };
        let (outcome, dialog) =
            flow.update(&self.ctx, &mut self.tabs, &self.theme, &mut self.next_token);
        if let Some(d) = dialog {
            self.pending.push(save_dialog_command(d.token, d.spec));
        }
        match outcome {
            Outcome::Pending => self.close_flow = Some(flow),
            Outcome::Aborted => {
                self.close_confirmed = false;
                if let Some(id) = flow.restore_focus {
                    self.ctx.memory_mut(|m| m.request_focus(id));
                }
            }
            Outcome::Finished { close } => match flow.kind {
                close_flow::Kind::Tabs | close_flow::Kind::AllTabs => {
                    self.close_tabs_by_id(&close, flow.kind)
                }
                close_flow::Kind::Window => {
                    self.close_confirmed = true;
                    self.pending.push(WindowCommand::Close);
                }
            },
        }
        // The dialog is modal: documents get no keyboard input meanwhile.
        if self.close_flow.is_some() {
            ui.input_mut(|i| {
                i.events.retain(|e| {
                    !matches!(
                        e,
                        Event::Key { .. }
                            | Event::Text(_)
                            | Event::Paste(_)
                            | Event::Copy
                            | Event::Cut
                    )
                })
            });
        }
    }

    fn handle_requests(&mut self, tab_id: u64, requests: Vec<DocRequest>) {
        for r in requests {
            let index = self.tabs.iter().position(|t| t.id == tab_id);
            match r {
                DocRequest::Open(paths) => self.open_paths(paths),
                DocRequest::Save => {
                    if let Some(i) = index {
                        self.active = i;
                        self.save_active();
                    }
                }
                DocRequest::SaveAs => {
                    if let Some(i) = index {
                        self.active = i;
                        self.save_active_as();
                    }
                }
                DocRequest::Close => {
                    if let Some(i) = index {
                        self.request_close_tab(i);
                    }
                }
                DocRequest::FindInTabs(q) => {
                    self.search.show_query(&q);
                    self.find_in_tabs(q);
                }
                DocRequest::CopyImage(image) => {
                    self.pending.push(WindowCommand::CopyImage(image.0));
                }
                DocRequest::Reveal(path) => self.pending.push(WindowCommand::Reveal(path)),
                DocRequest::Toast(text) => self.show_toast(text),
            }
        }
    }

    fn handle_drawer(&mut self, out: drawer::Out) {
        // A drawer came into view: its folder may have changed since it was read.
        let stale = |t: Option<Instant>| t.is_none_or(|t| t.elapsed() > Duration::from_secs(2));
        match out.rescan {
            Some(drawer::Mode::Notes) if stale(self.open_cx.library.snapshot().scanned_at) => {
                self.open_cx.library.scan();
            }
            Some(drawer::Mode::Shots) if stale(self.open_cx.captures.snapshot().scanned_at) => {
                self.open_cx.captures.scan();
            }
            _ => {}
        }
        if out.capture {
            self.open_dock();
        }
        if let Some(path) = out.open {
            self.open_paths(vec![path]);
        }
        if out.new_note {
            self.new_note();
        }
        if let Some(path) = out.reveal {
            self.pending.push(WindowCommand::Reveal(path));
        }
        if let Some(path) = out.delete {
            // Close it without saving (the user is deleting it), then recycle.
            if let Some(i) = self.tabs.iter().position(|t| {
                t.doc
                    .path()
                    .is_some_and(|p| whir_core::library::paths_equal(p, &path))
            }) {
                let mut tab = self.tabs.remove(i);
                tab.doc.discard();
                drop(tab);
                if self.tabs.is_empty() {
                    // Deleting the last open note never closes whir.
                    let doc = whir_notes::new_note(&self.open_cx);
                    self.push_tab(doc);
                }
                self.active = self.active.min(self.tabs.len() - 1);
            }
            self.pending.push(WindowCommand::Recycle(path));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, info: &FrameInfo, hit: &mut HitMap) {
        for tab in &mut self.tabs {
            tab.doc.poll();
        }
        self.poll_zombies();
        self.update_blink(ui);
        if self.close_flow.is_some() {
            self.run_close_flow(ui);
        }
        let mut drawer_out = drawer::Out::default();
        if self.close_flow.is_none() {
            let tab = self.tabs.get(self.active).map(|t| t.id);
            let mode = self.drawer.mode(tab, self.own_drawer());
            match mode {
                Some(drawer::Mode::Shots) => {
                    let shots = self.open_cx.captures.snapshot().entries;
                    let path_at = |i: usize| shots.get(i).map(|e| e.path.clone());
                    self.drawer
                        .keys(ui, mode, shots.len(), path_at, &mut drawer_out);
                }
                _ => {
                    let notes = self.open_cx.library.snapshot().entries;
                    let path_at = |i: usize| notes.get(i).map(|e| e.path.clone());
                    self.drawer
                        .keys(ui, mode, notes.len(), path_at, &mut drawer_out);
                }
            }
            self.handle_shortcuts(ui);
        }
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
                has_path: t.doc.path().is_some(),
                kind: t.doc.kind(),
                loading: t.doc.is_loading(),
                dirty: needs_attention(&*t.doc) && t.doc.save_error().is_none(),
                error: t.doc.save_error().is_some(),
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
            self.request_close_tab(i);
        }
        if let Some(i) = bar.close_others {
            let ids = self
                .tabs
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, t)| t.id)
                .collect();
            self.request_close_tabs(ids, close_flow::Kind::Tabs);
        }
        if let Some(i) = bar.close_to_right {
            let ids = self.tabs.iter().skip(i + 1).map(|t| t.id).collect();
            self.request_close_tabs(ids, close_flow::Kind::Tabs);
        }
        if bar.close_all {
            let ids = self.tabs.iter().map(|t| t.id).collect();
            self.request_close_tabs(ids, close_flow::Kind::AllTabs);
        }
        if let Some(p) = bar.copy_path.and_then(|i| self.tabs.get(i)?.doc.path()) {
            self.ctx.copy_text(p.display().to_string());
        }
        if let Some(p) = bar.reveal.and_then(|i| self.tabs.get(i)?.doc.path()) {
            self.pending.push(WindowCommand::Reveal(p.to_owned()));
        }
        if bar.open_dialog {
            self.pending.push(Self::open_dialog_command());
        }
        if bar.new_note {
            self.new_note();
        }
        if bar.new_table {
            self.new_table();
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

        // Find in all tabs, along the bottom (before the document takes the rest).
        let active_id = self.tabs.get(self.active).map(|t| t.id);
        let found = self.search.show(ui, &mut self.tabs, active_id, &theme);
        if let Some(q) = found.search {
            self.find_in_tabs(q);
        }
        if let Some(id) = found.activate {
            self.activate_tab(id);
        }
        // Its field or list has the keyboard: documents get pointer input only
        // (and Ctrl+F / F3 for their own find).
        if self.search.has_focus(&self.ctx) {
            ui.input_mut(|i| i.events.retain(search_panel::keep_for_documents));
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

        let open_cx = self.open_cx.clone();
        let active = self.active;
        let mut requests = Vec::new();

        // The toolbar: one surface with the active tab, which pours into it.
        let (mut dock, mut settings) = (false, false);
        Panel::top("toolbar")
            .exact_size(m.toolbar_height)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.fill(theme.content_bg))
            .show(ui, |ui| {
                let strip = ui.max_rect();
                // A hairline against the document below.
                let y = ui.painter().round_to_pixel_center(strip.bottom() - 0.5);
                ui.painter().line_segment(
                    [egui::pos2(strip.left(), y), egui::pos2(strip.right(), y)],
                    egui::Stroke::new(1.0, theme.separator),
                );
                let mut tools = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(strip.shrink2(egui::vec2(8.0, 0.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                if let Some(tab) = self.tabs.get_mut(active) {
                    let mut cx = DocCx {
                        textures: &open_cx.textures,
                        waker: &open_cx.waker,
                        just_activated,
                        requests: &mut requests,
                    };
                    tab.doc.toolbar(&mut tools, &mut cx);
                }
                // whir's own, at the far end: the capture dock.
                let mut ours = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(strip.shrink2(egui::vec2(8.0, 0.0)))
                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                );
                let camera = whir_core::toolbar::Glyph::Camera;
                let capture =
                    whir_core::toolbar::tool_button(&mut ours, camera, false, None, false, &theme);
                dock = capture.clicked;
                capture
                    .response
                    .on_hover_text("Screenshots: open the capture dock");
                let gear = whir_core::toolbar::Glyph::Gear;
                let on = self
                    .tabs
                    .get(active)
                    .is_some_and(|t| t.doc.kind() == DocKind::Settings);
                let open =
                    whir_core::toolbar::tool_button(&mut ours, gear, on, None, false, &theme);
                settings = open.clicked;
                open.response
                    .on_hover_text("Settings: the timesheet, timers, startup (Ctrl+,)");
            });
        if dock {
            self.open_dock();
        }
        if settings {
            self.open_settings("");
        }

        // The drawer along the bottom, for the kind of document that's open.
        let tab = self.tabs.get(self.active).map(|t| t.id);
        let mode = self.drawer.mode(tab, self.own_drawer());
        let active_path = self
            .tabs
            .get(self.active)
            .and_then(|t| t.doc.path().map(Path::to_path_buf));
        let out = self.drawer.show(
            ui,
            mode,
            &self.open_cx.library.snapshot(),
            &self.open_cx.captures.snapshot(),
            &self.open_cx,
            active_path.as_deref(),
            &theme,
        );
        drawer_out.open = drawer_out.open.or(out.open);
        drawer_out.capture |= out.capture;
        drawer_out.new_note |= out.new_note;
        drawer_out.reveal = drawer_out.reveal.or(out.reveal);
        drawer_out.delete = drawer_out.delete.or(out.delete);
        drawer_out.rescan = out.rescan;

        // Content.
        let mut open_requested = false;
        let content = CentralPanel::no_frame()
            .frame(Frame::NONE.fill(theme.content_bg))
            .show(ui, |ui| match self.tabs.get_mut(active) {
                Some(tab) => {
                    let mut cx = DocCx {
                        textures: &open_cx.textures,
                        waker: &open_cx.waker,
                        just_activated,
                        requests: &mut requests,
                    };
                    tab.doc.ui(ui, &mut cx);
                }
                None => open_requested = empty_state(ui, &theme),
            });
        if open_requested {
            self.pending.push(Self::open_dialog_command());
        }
        if let Some(id) = active_id
            && !requests.is_empty()
        {
            self.handle_requests(id, requests);
        }

        self.handle_drawer(drawer_out);

        paint_toast(ui, content.response.rect, &mut self.toast, &theme);

        // Popups over the caption (the "+" menu) must get clicks, not window drags.
        let caption_bottom = strip.response.rect.bottom();
        let popups: Vec<egui::Rect> = self.ctx.memory(|mem| {
            mem.areas()
                .visible_layer_ids()
                .into_iter()
                .filter(|l| l.order == egui::Order::Foreground || l.order == egui::Order::Tooltip)
                .filter_map(|l| mem.area_rect(l.id))
                .filter(|r| r.top() < caption_bottom)
                .collect()
        });
        hit.interactive
            .extend(popups.into_iter().map(|r| RectPx::from_points(r, ppp)));

        // Window title (taskbar, Alt+Tab).
        let title = match self.tabs.get(self.active) {
            Some(t) if needs_attention(&*t.doc) => format!("● {} - whir", t.doc.title()),
            Some(t) => format!("{} - whir", t.doc.title()),
            None => "whir".to_owned(),
        };
        if title != self.window_title {
            self.pending.push(WindowCommand::SetTitle(title.clone()));
            self.window_title = title;
        }

        // Closing is waiting for background saves.
        if self.close_when_idle {
            let saving = self
                .tabs
                .iter()
                .chain(&self.zombies)
                .any(|t| t.doc.is_saving());
            if !saving {
                self.close_when_idle = false;
                if self.tabs.iter().any(|t| t.doc.save_error().is_some()) {
                    // A save failed: stay open so nothing is lost.
                    self.close_confirmed = false;
                    self.show_toast("A document couldn't be saved, so whir stayed open.");
                } else {
                    self.pending.push(WindowCommand::Close);
                }
            } else {
                self.ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
    }

    /// GPU device lost: whir restarts with the same files. Save what we can
    /// first, asking with native message boxes (the GPU is gone).
    fn close_for_restart(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        for t in &mut self.tabs {
            let _ = t.doc.flush_blocking(deadline);
        }
        for t in &mut self.tabs {
            if !needs_attention(&*t.doc) {
                continue;
            }
            let ask = format!(
                "The graphics driver was reset, so whir needs to restart.\n\nSave changes to “{}” first?",
                t.doc.title()
            );
            if !crate::message_yes_no(&ask) {
                continue;
            }
            let mut result = t.doc.save();
            if let SaveResult::NeedsPath(spec) = result {
                let filters: Vec<(&str, &str)> = spec
                    .filters
                    .iter()
                    .map(|(a, b)| (a.as_str(), b.as_str()))
                    .collect();
                result = match whir_platform::dialogs::save_file(
                    windows::Win32::Foundation::HWND::default(),
                    &whir_platform::dialogs::SaveOptions {
                        title: &spec.title,
                        suggested_name: &spec.suggested_name,
                        filters: &filters,
                        default_ext: &spec.default_ext,
                        folder: spec.folder.as_deref(),
                    },
                ) {
                    Some(p) => t.doc.save_as(p),
                    None => SaveResult::Saved,
                };
            }
            if let SaveResult::Started = result {
                let until = Instant::now() + Duration::from_secs(30);
                while t.doc.is_saving() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(20));
                    t.doc.poll();
                }
            }
        }
        // Notes that were just saved for the first time now have paths.
        let paths = self
            .tabs
            .iter()
            .filter_map(|t| t.doc.path().map(Path::to_path_buf))
            .collect();
        *self.restart.borrow_mut() = Some(paths);
        true
    }
}

fn save_dialog_command(token: u64, spec: SaveAsSpec) -> WindowCommand {
    WindowCommand::SaveFileDialog {
        token,
        spec: SaveDialogSpec {
            title: spec.title,
            suggested_name: spec.suggested_name,
            filters: spec.filters,
            default_ext: spec.default_ext,
            folder: spec.folder,
        },
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
        ui.label(
            egui::RichText::new("Images, PDFs, CSV files and notes · Ctrl+N for a new note")
                .color(theme.text_dim),
        );
        ui.add_space(14.0);
        clicked = ui.button("  Open…  ").clicked();
    });
    clicked
}

fn paint_toast(ui: &egui::Ui, area: egui::Rect, toast: &mut Option<(String, f64)>, theme: &Theme) {
    let Some((text, until)) = toast else { return };
    let now = ui.input(|i| i.time);
    if now >= *until {
        *toast = None;
        return;
    }
    let alpha = ((*until - now) / 0.3).clamp(0.0, 1.0) as f32;
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("whir.toast"),
    ));
    let galley = painter.layout_no_wrap(
        text.clone(),
        egui::FontId::proportional(12.5),
        theme.text.gamma_multiply(alpha),
    );
    let pad = egui::vec2(12.0, 6.0);
    let size = galley.size() + pad * 2.0;
    let rect = egui::Rect::from_center_size(
        egui::pos2(area.center().x, area.bottom() - 28.0 - size.y / 2.0),
        size,
    );
    painter.rect_filled(rect, size.y / 2.0, theme.overlay_bg.gamma_multiply(alpha));
    painter.rect_stroke(
        rect,
        size.y / 2.0,
        egui::Stroke::new(1.0, theme.separator.gamma_multiply(alpha)),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + pad, galley, theme.text);
    ui.ctx().request_repaint_after(Duration::from_millis(50));
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
        if !self.rich_fonts_loaded && theme::rich_fonts_requested() {
            self.rich_fonts_loaded = true;
            whir_platform::fonts::add_family_fonts(&self.ctx, theme::LAZY_RICH_FONTS);
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
            match cmd {
                OutputCommand::CopyText(text) => out.copied_text = Some(text),
                OutputCommand::OpenUrl(open) => {
                    out.commands.push(WindowCommand::OpenUrl(open.url));
                }
                _ => {}
            }
        }
        let viewport = full.viewport_output.get(&ViewportId::ROOT);
        if viewport.is_some_and(|v| {
            v.commands
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::RequestPaste))
        }) {
            out.commands.push(WindowCommand::Paste);
        }
        let delay = viewport.map_or(Duration::MAX, |v| v.repaint_delay);
        if delay < Duration::from_secs(60 * 60) {
            out.repaint_after = Some(delay);
        }
        // Text needing CJK/emoji glyphs (or bold) appeared this frame: load fonts now.
        if (!self.fallback_fonts_loaded && theme::fallback_fonts_requested())
            || (!self.rich_fonts_loaded && theme::rich_fonts_requested())
        {
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
        } else if !self.library_scanned {
            // The notes list is read once the window is up (never on the startup path).
            self.library_scanned = true;
            self.open_cx.library.scan();
            // whir lives on in its dock (the tray icon, timers, todos) when this
            // window closes: make sure it is running.
            let _ = whir_capture::launch(whir_capture::Command::Start);
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

    fn bare_launch(&mut self) {
        self.new_note();
    }

    fn show_page(&mut self, page: &str) {
        self.open_settings(page);
    }

    fn shared_changed(&mut self, what: Changed) {
        match what {
            Changed::Time => whir_core::timesheet::changed_elsewhere(),
            Changed::Todos => {}
        }
        self.ctx.request_repaint();
    }

    fn close_requested(&mut self) -> bool {
        if self.restart.borrow().is_some() {
            return self.close_for_restart();
        }
        // "Close because empty" is void if files arrived in the meantime.
        if self.closing_because_empty && !self.tabs.is_empty() {
            self.closing_because_empty = false;
            return false;
        }
        self.closing_because_empty = false;
        if !self.close_confirmed {
            if self.close_flow.is_some() {
                return false;
            }
            for t in &mut self.tabs {
                t.doc.flush();
            }
            let queue: Vec<u64> = self
                .tabs
                .iter()
                .filter(|t| needs_attention(&*t.doc))
                .map(|t| t.id)
                .collect();
            if !queue.is_empty() {
                let focus = self.ctx.memory(|m| m.focused());
                self.close_flow = Some(CloseFlow::new(
                    close_flow::Kind::Window,
                    queue,
                    Vec::new(),
                    focus,
                ));
                self.ctx.request_repaint();
                return false;
            }
        }
        // Background saves must finish first (the process exits right after).
        if self
            .tabs
            .iter()
            .chain(&self.zombies)
            .any(|t| t.doc.is_saving())
        {
            self.close_confirmed = true;
            self.close_when_idle = true;
            self.ctx.request_repaint();
            return false;
        }
        true
    }

    fn activated(&mut self, active: bool) {
        for t in &mut self.tabs {
            t.doc.app_activated(active);
        }
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

    fn save_dialog_closed(&mut self, token: u64, path: Option<PathBuf>) {
        if let Some(flow) = &mut self.close_flow
            && flow.owns_dialog(token)
        {
            flow.dialog_closed(path, &mut self.tabs);
            return;
        }
        let Some(pos) = self.dialogs.iter().position(|(t, _)| *t == token) else {
            return;
        };
        let (_, purpose) = self.dialogs.remove(pos);
        let DialogFor::SaveAs(tab) = purpose;
        let Some(path) = path else { return };
        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab)
            && let SaveResult::Failed(e) = t.doc.save_as(path)
        {
            self.show_toast(e);
        }
    }

    fn recycled(&mut self, path: PathBuf, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.show_toast("Moved to the Recycle Bin");
                // A capture's highlights go with it.
                self.open_cx.captures.forget(path.clone());
            }
            Err(e) => self.show_toast(e),
        }
        self.open_cx.captures.refresh(vec![path.clone()]);
        self.open_cx.library.refresh(vec![path]);
    }

    fn query_end_session(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        for t in &mut self.tabs {
            let _ = t.doc.flush_blocking(deadline);
        }
        let queue: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| needs_attention(&*t.doc))
            .map(|t| t.id)
            .collect();
        if queue.is_empty() {
            return true;
        }
        // Unsaved work: ask Windows to wait, and ask the user.
        if self.close_flow.is_none() {
            self.close_flow = Some(CloseFlow::new(
                close_flow::Kind::Window,
                queue,
                Vec::new(),
                None,
            ));
        }
        self.ctx.request_repaint();
        false
    }

    fn end_session(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        for t in self.tabs.iter_mut().chain(self.zombies.iter_mut()) {
            let _ = t.doc.flush_blocking(deadline);
        }
        let _ = whir_notes::saver::wait_idle(deadline);
        trace::flush();
    }
}
