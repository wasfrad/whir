//! The shell: tabs, shortcuts, saving and closing, the drawer along the
//! bottom, first-frame logic, and rendering.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{CentralPanel, Event, Frame, Key, Modifiers, OutputCommand, Panel, ViewportId};
use whir_core::icons::Icon;
use whir_core::kit;
use whir_core::theme::{self, Mode, NoteSize, Theme};
use whir_core::{
    DocCx, DocKind, DocRequest, Document, FindQuery, OpenCx, Persistence, SaveAsSpec, SaveResult,
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
use crate::menus::{self, Command, Enabled};
use crate::palette::{self, Palette, Pick};
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
    /// The first frame's own cost has been marked in the startup trace.
    first_frame_traced: bool,
    /// Help > About is open.
    about: bool,
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
    /// A note flagged before it had a file: flagged once it's saved.
    flag_when_saved: Option<u64>,
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
    palette: Palette,
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
        let theme = Theme::chosen(dark);
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
            first_frame_traced: false,
            about: false,
            window_title: String::new(),
            fallback_fonts_loaded: false,
            rich_fonts_loaded: false,
            pending: Vec::new(),
            restart,
            closing_because_empty: false,
            zombies: Vec::new(),
            flag_when_saved: None,
            close_flow: None,
            close_confirmed: false,
            close_when_idle: false,
            dialogs: Vec::new(),
            next_token: 0,
            drawer: Drawer::default(),
            search: SearchPanel::default(),
            palette: Palette::default(),
            library_scanned: false,
            toast: None,
            blink_on: true,
            last_input: 0.0,
        }
    }

    pub fn set_renderer(&mut self, renderer: Renderer) {
        self.renderer = Some(renderer);
    }

    /// What the last frame drew, for the gallery (see `crate::gallery`).
    #[cfg(test)]
    pub fn read_pixels(&self) -> Option<Vec<u8>> {
        self.renderer.as_ref()?.read_pixels()
    }

    /// Any tab still loading, for the gallery to wait on.
    #[cfg(test)]
    pub fn loading(&self) -> bool {
        self.tabs.iter().any(|t| t.doc.is_loading())
    }

    /// The active tab's title, what it says of itself, and whether it has
    /// changes, for the tests that drive the whole app.
    #[cfg(test)]
    pub fn active_tab(&self) -> (String, String, bool) {
        self.tabs
            .get(self.active)
            .map_or_else(Default::default, |t| {
                (t.doc.title().to_owned(), t.doc.status(), t.doc.is_dirty())
            })
    }

    /// What has the keyboard.
    #[cfg(test)]
    pub fn focused(&self) -> Option<egui::Id> {
        self.ctx.memory(|m| m.focused())
    }

    #[cfg(test)]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    #[cfg(test)]
    pub fn palette_is_open(&self) -> bool {
        self.palette.is_open()
    }

    /// Do what the theme tiles and the note size drop-down do.
    #[cfg(test)]
    pub fn change_look(&mut self, mode: Mode, size: NoteSize) {
        self.set_note_size(size);
        self.run(Command::Theme(mode));
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
            // Captures arrive after their PNG is saved, even when the Images
            // pane is already visible and its last scan is still recent.
            self.open_cx.captures.refresh(vec![path.clone()]);
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

    /// Settings, at `page` ("timesheet", "milestones", "startup"): a tab of its
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

    /// Flag the note at `path` as a todo, mark it done, or not.
    fn set_flag(&mut self, path: PathBuf, flag: drawer::Flag) {
        use drawer::Flag;
        use whir_core::todos::State;
        let now = whir_core::time::Stamp::now();
        let current = self
            .open_cx
            .library
            .snapshot()
            .entries
            .iter()
            .find(|e| whir_core::library::paths_equal(&e.path, &path))
            .and_then(|e| e.todo);
        let next = match (flag, current) {
            (Flag::Clear, _) => None,
            (Flag::Open, _) | (Flag::Toggle, None | Some(State::Done { .. })) => {
                Some(State::Open { since: now })
            }
            (Flag::Done | Flag::Toggle, Some(State::Open { since })) => {
                Some(State::Done { since, done: now })
            }
            (Flag::Done, other) => other,
        };
        match whir_core::todos::set(&path, next) {
            Ok(()) => {
                self.open_cx.library.todos_changed();
                let name = path.file_stem().unwrap_or_default().to_string_lossy();
                match next {
                    Some(State::Open { .. }) => self.show_toast(format!("To do: {name}")),
                    Some(State::Done { .. }) => self.show_toast(format!("Done: {name}")),
                    None => {}
                }
            }
            Err(e) => self.show_toast(e),
        }
    }

    /// The flag button and Ctrl+Shift+D: the active note's todo flag, on or
    /// done. A note not saved yet is flagged as soon as it is.
    fn flag_active(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        match tab.doc.path() {
            Some(p) if self.open_cx.library.contains(p) => {
                let path = p.to_owned();
                self.set_flag(path, drawer::Flag::Toggle);
            }
            Some(_) => self.show_toast("Only notes in the notes folder can be todos."),
            None if tab.doc.is_pristine() => {
                self.show_toast("Write something first: then it can be a todo.")
            }
            None => {
                self.flag_when_saved = Some(tab.id);
                tab.doc.flush();
            }
        }
    }

    /// The todo flag of the active tab's note (`None`: it can't have one).
    fn active_flag(&self) -> Option<Option<whir_core::todos::State>> {
        let tab = self.tabs.get(self.active)?;
        if !(tab.doc.kind() == DocKind::Note
            || (tab.doc.kind() == DocKind::Csv
                && tab
                    .doc
                    .path()
                    .is_some_and(|p| self.open_cx.library.contains(p))))
        {
            return None;
        }
        let Some(path) = tab.doc.path() else {
            return Some(None);
        };
        Some(
            self.open_cx
                .library
                .snapshot()
                .entries
                .iter()
                .find(|e| whir_core::library::paths_equal(&e.path, path))
                .and_then(|e| e.todo),
        )
    }

    fn sync_drawer(&mut self) {
        let document = self.tabs.get(self.active).map(|t| (t.id, t.doc.kind()));
        self.drawer.sync_document(document);
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
        let (mut settings, mut flag) = (false, false);
        let (mut find_in_tabs, mut result_step) = (false, None);
        let mut command_palette = false;
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
            flag = consume_exact(i, ctrl_shift, Key::D);
            find_in_tabs = consume_exact(i, ctrl_shift, Key::F);
            command_palette = consume_exact(i, ctrl_shift, Key::P);
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
        for (asked, command) in [
            (open, Command::Open),
            (reopen, Command::ReopenTab),
            (new_note, Command::NewNote),
            (new_table, Command::NewTable),
            (save, Command::Save),
            (save_as, Command::SaveAs),
            (drawer, Command::ToggleDrawer),
            (find_in_tabs, Command::FindInTabs),
            (settings, Command::Settings),
            (command_palette, Command::Palette),
        ] {
            if asked {
                self.run(command);
            }
        }
        if flag {
            self.flag_active();
        }
        if let Some(forward) = result_step
            && let Some(id) = self.search.step(&mut self.tabs, forward)
        {
            self.activate_tab(id);
        }
        let n = self.tabs.len();
        if n > 0 {
            match step {
                1 => self.run(Command::NextTab),
                -1 => self.run(Command::PreviousTab),
                _ => {}
            }
            if let Some(g) = goto
                && g < n
            {
                self.active = g;
            }
            if close {
                self.run(Command::CloseTab);
            }
        }
    }

    /// Do what a menu or a shortcut asked for.
    fn run(&mut self, command: Command) {
        let n = self.tabs.len() as i32;
        match command {
            Command::NewNote => self.new_note(),
            Command::NewTable => self.new_table(),
            Command::Open => self.pending.push(Self::open_dialog_command()),
            Command::ReopenTab => {
                if let Some(p) = self.recently_closed.pop() {
                    self.open_paths(vec![p]);
                }
            }
            Command::Save => self.save_active(),
            Command::SaveAs => self.save_active_as(),
            Command::CloseTab => {
                if n > 0 {
                    self.request_close_tab(self.active);
                }
            }
            Command::CloseWindow => self.pending.push(WindowCommand::Close),
            Command::FindInTabs => self.search.focus(),
            Command::ToggleDrawer => {
                self.sync_drawer();
                self.drawer.toggle();
            }
            Command::NextTab if n > 0 => {
                self.active = (self.active as i32 + 1).rem_euclid(n) as usize;
            }
            Command::PreviousTab if n > 0 => {
                self.active = (self.active as i32 - 1).rem_euclid(n) as usize;
            }
            Command::NextTab | Command::PreviousTab => {}
            Command::Capture => self.capture(),
            Command::Dock => self.open_dock(),
            Command::Timesheet => self.open_settings("timesheet"),
            Command::Settings => self.open_settings(""),
            Command::Theme(mode) => self.set_theme(mode),
            Command::Palette => {
                let focused = self.ctx.memory(|m| m.focused());
                let notes = self.open_cx.library.snapshot().entries;
                self.palette.open(focused, &notes);
                self.ctx.request_repaint();
            }
            Command::Guide => self.open_guide(),
            Command::About => self.about = true,
        }
    }

    /// Draw in `mode` from now on, and tell the dock once it can read that.
    fn set_theme(&mut self, mode: Mode) {
        mode.save(|| whir_platform::ipc::notify_changed(Changed::Theme));
        self.look_again();
    }

    fn set_note_size(&mut self, size: NoteSize) {
        size.save();
        self.look_again();
    }

    /// The theme or the note text size may be different now: everything is
    /// drawn from these, so that's all there is to it.
    fn look_again(&mut self) {
        let dark = whir_platform::os::prefers_dark();
        let theme = Theme::chosen(dark);
        if dark == self.theme.dark && theme.metrics == self.theme.metrics {
            return;
        }
        if dark != self.theme.dark {
            self.pending.push(WindowCommand::SetDarkMode(dark));
        }
        theme.apply(&self.ctx);
        Theme::set_current(theme.clone());
        self.theme = theme;
        self.ctx.request_repaint();
    }

    fn about_dialog(&mut self, theme: &Theme) {
        if !self.about {
            return;
        }
        let mut close = false;
        let modal = kit::dialog(&self.ctx, egui::Id::new("whir.about"), 340.0, theme, |ui| {
            kit::dialog_title(ui, concat!("whir ", env!("CARGO_PKG_VERSION")), theme);
            ui.label(egui::RichText::new(env!("CARGO_PKG_DESCRIPTION")).color(theme.text_dim));
            kit::dialog_actions(ui, |ui| {
                close = kit::action(ui, "Close", theme).clicked();
            });
        });
        if close || modal.should_close() {
            self.about = false;
        }
    }

    /// Take a screenshot (the dock does that).
    fn capture(&mut self) {
        if let Err(e) = whir_capture::launch(whir_capture::Command::Capture) {
            self.show_toast(format!("The capture couldn't start: {e}"));
        }
    }

    /// The guide travels beside whir.exe, as README.md.
    fn open_guide(&mut self) {
        let guide = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join("README.md")));
        match guide {
            Some(path) if path.is_file() => self.open_paths(vec![path]),
            _ => self.show_toast("The guide isn't beside whir.exe."),
        }
    }

    /// The menus' idea of what can be done now.
    fn enabled(&self) -> Enabled {
        let doc = self.tabs.get(self.active).map(|t| &t.doc);
        let save = doc.is_some_and(|d| d.persistence() != Persistence::None);
        Enabled {
            tab: doc.is_some(),
            reopen: !self.recently_closed.is_empty(),
            theme: Mode::saved(),
            save,
            save_as: save || doc.is_some_and(|d| d.kind() == DocKind::Image),
            several_tabs: self.tabs.len() > 1,
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
            ui.input_mut(|i| i.events.retain(|e| !palette::is_keyboard(e)));
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
                DocRequest::Theme(mode) => self.run(Command::Theme(mode)),
                DocRequest::NoteSize(size) => self.set_note_size(size),
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
            self.run(Command::Capture);
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
        if let Some((path, flag)) = out.flag {
            self.set_flag(path, flag);
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
        // The palette goes first, so it has the keys before anything else does;
        // it's drawn over everything wherever it comes in the frame.
        self.palette.tick(&self.ctx);
        let palette_up = self.palette.is_open();
        if palette_up {
            let theme = self.theme.clone();
            match self.palette.show(&self.ctx, &theme) {
                Some(Pick::Note(path)) => self.open_paths(vec![path]),
                Some(Pick::Page(section)) => self.open_settings(section.name()),
                Some(Pick::Menu(..)) | None => {}
            }
            ui.input_mut(|i| i.events.retain(|e| !palette::is_keyboard(e)));
        }
        let mut drawer_out = drawer::Out::default();
        let every_key = self
            .tabs
            .get(self.active)
            .is_some_and(|t| t.doc.wants_every_key());
        if self.close_flow.is_none() && !every_key && !palette_up {
            self.sync_drawer();
            match self.drawer.mode() {
                drawer::Mode::Shots => {
                    let shots = self.open_cx.captures.snapshot().entries;
                    let path_at = |i: usize| shots.get(i).map(|e| e.path.clone());
                    self.drawer.keys(ui, shots.len(), path_at, &mut drawer_out);
                }
                _ => {
                    let notes = self.open_cx.library.snapshot().entries;
                    let path_at = |i: usize| notes.get(i).map(|e| e.path.clone());
                    self.drawer.keys(ui, notes.len(), path_at, &mut drawer_out);
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

        // The menus and the document's tools: one surface with the active tab,
        // which pours into it.
        let mut flag = false;
        let todo = self.active_flag();
        let enabled = self.enabled();
        let mut commands = Vec::new();
        let read_menus = self.palette.wants_menus();
        let press = self.palette.take_press();
        let press = press.as_ref().map(|(menu, label)| (*menu, label.as_str()));
        let mut read = None;
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
                        .max_rect(strip.shrink2(egui::vec2(6.0, 0.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                tools.spacing_mut().item_spacing.x = 2.0;
                let mut cx = DocCx {
                    textures: &open_cx.textures,
                    waker: &open_cx.waker,
                    just_activated,
                    requests: &mut requests,
                };
                let tab = self.tabs.get_mut(active);
                match tab {
                    Some(tab) => {
                        if read_menus {
                            let active = menus::Active {
                                doc: &mut *tab.doc,
                                cx: &mut cx,
                            };
                            read = Some(menus::read(&mut tools, Some(active), enabled, press));
                        }
                        let active = menus::Active {
                            doc: &mut *tab.doc,
                            cx: &mut cx,
                        };
                        commands = menus::bar(&mut tools, Some(active), enabled);
                        if !matches!(tab.doc.kind(), DocKind::Settings | DocKind::Error) {
                            kit::bar_separator(&mut tools, &theme);
                            tab.doc.toolbar(&mut tools, &mut cx);
                        }
                    }
                    None => {
                        if read_menus {
                            read = Some(menus::read(&mut tools, None, enabled, press));
                        }
                        commands = menus::bar(&mut tools, None, enabled);
                    }
                }
                // A note: its todo flag, among its own tools.
                if let Some(todo) = todo {
                    let on = todo.is_some_and(|t| t.is_open());
                    let icon = if on { Icon::FlagFilled } else { Icon::Flag };
                    let label = if on { "Todo" } else { "Mark as todo" };
                    kit::bar_separator(&mut tools, &theme);
                    let b =
                        whir_core::toolbar::labelled_button(&mut tools, icon, label, on, &theme);
                    flag = b.clicked();
                    b.on_hover_text(match todo {
                        Some(t) if t.is_open() => "Mark it done (Ctrl+Shift+D)",
                        _ => "First in the notes, and on the dock (Ctrl+Shift+D)",
                    });
                }
            });
        if let Some((entries, pressed)) = read {
            self.palette.menus_read(entries);
            commands.extend(pressed);
        }
        for command in commands {
            self.run(command);
        }
        if flag {
            self.flag_active();
        }
        // A note flagged before it had a file has one now.
        if let Some(id) = self.flag_when_saved {
            match self.tabs.iter().find(|t| t.id == id).map(|t| t.doc.path()) {
                Some(Some(p)) => {
                    let path = p.to_owned();
                    self.flag_when_saved = None;
                    self.set_flag(path, drawer::Flag::Open);
                }
                Some(None) => {}
                None => self.flag_when_saved = None,
            }
        }

        // The drawer is app-wide; only its default fold follows the document.
        self.sync_drawer();
        let active_path = self
            .tabs
            .get(self.active)
            .and_then(|t| t.doc.path().map(Path::to_path_buf));
        let status = self
            .tabs
            .get(self.active)
            .map_or_else(String::new, |t| t.doc.status());
        let out = self.drawer.show(
            ui,
            &self.open_cx.library.snapshot(),
            &self.open_cx.captures.snapshot(),
            &self.open_cx,
            active_path.as_deref(),
            &status,
            &theme,
        );
        drawer_out.open = drawer_out.open.or(out.open);
        drawer_out.capture |= out.capture;
        drawer_out.new_note |= out.new_note;
        drawer_out.reveal = drawer_out.reveal.or(out.reveal);
        drawer_out.delete = drawer_out.delete.or(out.delete);
        drawer_out.flag = drawer_out.flag.or(out.flag);
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

        kit::toast(ui, content.response.rect, &mut self.toast, &theme);
        self.about_dialog(&theme);

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
        ui.add_space((ui.available_height() * 0.4 - 60.0).max(16.0));
        kit::empty_state(
            ui,
            Icon::Folder,
            "Drop a file here",
            "Images, PDFs, CSV files and notes \u{b7} Ctrl+N for a new note",
            theme,
        );
        ui.add_space(14.0);
        clicked = kit::primary(ui, "Open\u{2026}", theme).clicked();
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
        let first_frame = !self.first_frame_traced;
        self.first_frame_traced = true;
        if first_frame {
            trace::mark("ui pass");
        }

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
        if first_frame {
            trace::mark("presented");
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
            // Populate both drawer tabs once the window is up, not on the startup path.
            self.library_scanned = true;
            self.open_cx.library.scan();
            self.open_cx.captures.scan();
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
            Changed::Todos => self.open_cx.library.todos_changed(),
            // Ours: the window is what writes it.
            Changed::Theme => {}
            // Settings reads it again with the timesheet.
            Changed::Dock => whir_core::timesheet::changed_elsewhere(),
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
        self.look_again();
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
                // A capture's highlights go with it, as does a note's todo flag.
                self.open_cx.captures.forget(path.clone());
                whir_core::todos::removed(&path);
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
        let _ = whir_core::settings::wait_idle(deadline);
        trace::flush();
    }

    fn placement_changed(&mut self, placement: whir_platform::placement::Placement) {
        whir_platform::placement::save(&placement);
    }
}
