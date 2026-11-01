//! The menu bar: everything whir can do, in one place, with its shortcut beside
//! it. What belongs to the open document comes from [`Document::menu`].

use egui::containers::menu::{MenuBar, MenuButton, SubMenuButton, menu_style};
use egui::{Align, Button, Layout, Style, Ui, UiBuilder, vec2};
use whir_core::icons::{self, Icon};
use whir_core::kit;
use whir_core::theme::Mode;
use whir_core::{DocCx, Document, Menu};

/// Below this the menus fold into one button (points of window width).
const FULL_WIDTH: f32 = 640.0;
const MENU_WIDTH: f32 = 240.0;
/// What File, Edit, View, Tools and Help take up. egui's menu bar takes all the
/// width it's given, and the document's tools come after it.
const BAR_WIDTH: f32 = 222.0;

/// What a menu asks of the app. Shortcuts ask the same things.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NewNote,
    NewTable,
    Open,
    ReopenTab,
    Save,
    SaveAs,
    CloseTab,
    CloseWindow,
    FindInTabs,
    ToggleDrawer,
    NextTab,
    PreviousTab,
    Capture,
    Dock,
    Timesheet,
    Settings,
    Theme(Mode),
    Palette,
    Guide,
    About,
}

/// What can be done right now.
#[derive(Clone, Copy, Debug)]
pub struct Enabled {
    pub tab: bool,
    pub reopen: bool,
    pub save: bool,
    pub save_as: bool,
    pub several_tabs: bool,
    pub theme: Mode,
}

/// The active document, while its parts of the menus are drawn.
pub struct Active<'a, 'b> {
    pub doc: &'a mut dyn Document,
    pub cx: &'a mut DocCx<'b>,
}

/// Draw the menus. Returns what was picked.
pub fn bar(ui: &mut Ui, mut active: Option<Active<'_, '_>>, on: Enabled) -> Vec<Command> {
    let mut out = Vec::new();
    let full = ui.ctx().content_rect().width() >= FULL_WIDTH;
    if full {
        let slot = vec2(BAR_WIDTH, ui.available_height());
        ui.allocate_ui_with_layout(slot, Layout::left_to_right(Align::Center), |ui| {
            MenuBar::new().style(bar_style).ui(ui, |ui| {
                ui.menu_button("File", |ui| file(ui, on, &mut out));
                ui.menu_button("Edit", |ui| edit(ui, active.as_mut(), &mut out));
                ui.menu_button("View", |ui| view(ui, active.as_mut(), on, &mut out));
                ui.menu_button("Tools", |ui| tools(ui, &mut out));
                ui.menu_button("Help", |ui| help(ui, &mut out));
            });
        });
    } else {
        let button = Button::new(icons::text(Icon::Menu, 16.0)).frame_when_inactive(false);
        MenuButton::from_button(button).ui(ui, |ui| {
            submenu("File").ui(ui, |ui| file(ui, on, &mut out));
            submenu("Edit").ui(ui, |ui| edit(ui, active.as_mut(), &mut out));
            submenu("View").ui(ui, |ui| view(ui, active.as_mut(), on, &mut out));
            submenu("Tools").ui(ui, |ui| tools(ui, &mut out));
            submenu("Help").ui(ui, |ui| help(ui, &mut out));
        });
    }
    out
}

/// The menus, read rather than drawn, with what can be done now in each; and
/// what came of pressing `press`, a label in a menu, if there is one.
pub fn read(
    ui: &mut Ui,
    mut active: Option<Active<'_, '_>>,
    on: Enabled,
    press: Option<(&str, &str)>,
) -> (Vec<(&'static str, kit::Entry)>, Vec<Command>) {
    let mut ui = ui.new_child(UiBuilder::new().invisible());
    let ui = &mut ui;
    let mut out = Vec::new();
    let mut found = Vec::new();
    let pressed = |menu: &str| press.filter(|(m, _)| *m == menu).map(|(_, label)| label);
    let mut take = |menu: &'static str, entries: Vec<kit::Entry>| {
        let enabled = entries.into_iter().filter(|e| e.enabled);
        found.extend(enabled.map(|e| (menu, e)));
    };
    take(
        "File",
        kit::read_menu(pressed("File"), || file(ui, on, &mut out)),
    );
    take(
        "Edit",
        kit::read_menu(pressed("Edit"), || edit(ui, active.as_mut(), &mut out)),
    );
    take(
        "View",
        kit::read_menu(pressed("View"), || view(ui, active.as_mut(), on, &mut out)),
    );
    take(
        "Tools",
        kit::read_menu(pressed("Tools"), || tools(ui, &mut out)),
    );
    take(
        "Help",
        kit::read_menu(pressed("Help"), || help(ui, &mut out)),
    );
    (found, out)
}

/// A menu inside a menu, in line with the entries around it.
fn submenu(label: &str) -> SubMenuButton<'_> {
    SubMenuButton::from_button(kit::submenu_entry(label))
}

fn bar_style(style: &mut Style) {
    menu_style(style);
    style.spacing.button_padding = vec2(8.0, 4.0);
    style.spacing.item_spacing.x = 0.0;
}

fn item(
    ui: &mut Ui,
    icon: Option<Icon>,
    label: &str,
    shortcut: &str,
    enabled: bool,
    command: Command,
    out: &mut Vec<Command>,
) {
    if kit::entry(ui, icon, label, shortcut, enabled) {
        out.push(command);
    }
}

fn file(ui: &mut Ui, on: Enabled, out: &mut Vec<Command>) {
    ui.set_min_width(MENU_WIDTH);
    let note = Some(Icon::Note);
    let table = Some(Icon::Table);
    item(ui, note, "New note", "Ctrl+N", true, Command::NewNote, out);
    item(
        ui,
        table,
        "New table",
        "Ctrl+Shift+N",
        true,
        Command::NewTable,
        out,
    );
    let open = Some(Icon::Folder);
    item(ui, open, "Open\u{2026}", "Ctrl+O", true, Command::Open, out);
    item(
        ui,
        None,
        "Reopen closed tab",
        "Ctrl+Shift+T",
        on.reopen,
        Command::ReopenTab,
        out,
    );
    ui.separator();
    let save = Some(Icon::Save);
    item(ui, save, "Save", "Ctrl+S", on.save, Command::Save, out);
    item(
        ui,
        Some(Icon::SaveAs),
        "Save as\u{2026}",
        "Ctrl+Shift+S",
        on.save_as,
        Command::SaveAs,
        out,
    );
    ui.separator();
    let close = Some(Icon::Close);
    item(
        ui,
        close,
        "Close tab",
        "Ctrl+W",
        on.tab,
        Command::CloseTab,
        out,
    );
    item(
        ui,
        None,
        "Close window",
        "",
        true,
        Command::CloseWindow,
        out,
    );
}

fn edit(ui: &mut Ui, active: Option<&mut Active<'_, '_>>, out: &mut Vec<Command>) {
    ui.set_min_width(MENU_WIDTH);
    if let Some(a) = active {
        a.doc.menu(Menu::Edit, ui, a.cx);
    }
    item(
        ui,
        None,
        "Find in all tabs",
        "Ctrl+Shift+F",
        true,
        Command::FindInTabs,
        out,
    );
}

fn view(ui: &mut Ui, active: Option<&mut Active<'_, '_>>, on: Enabled, out: &mut Vec<Command>) {
    ui.set_min_width(MENU_WIDTH);
    if let Some(a) = active {
        a.doc.menu(Menu::View, ui, a.cx);
    }
    item(
        ui,
        None,
        "Notes and images drawer",
        "Ctrl+E",
        true,
        Command::ToggleDrawer,
        out,
    );
    if kit::reading() {
        themes(ui, on, out);
    } else {
        submenu("Theme").ui(ui, |ui| themes(ui, on, out));
    }
    ui.separator();
    item(
        ui,
        None,
        "Next tab",
        "Ctrl+Tab",
        on.several_tabs,
        Command::NextTab,
        out,
    );
    item(
        ui,
        None,
        "Previous tab",
        "Ctrl+Shift+Tab",
        on.several_tabs,
        Command::PreviousTab,
        out,
    );
}

/// The theme submenu. Read for the palette, an entry stands on its own.
fn themes(ui: &mut Ui, on: Enabled, out: &mut Vec<Command>) {
    ui.set_min_width(140.0);
    for mode in Mode::ALL {
        let label = if kit::reading() {
            format!("Theme: {}", mode.label())
        } else {
            mode.label().to_owned()
        };
        if kit::check_entry(ui, on.theme == mode, &label, "") {
            out.push(Command::Theme(mode));
        }
    }
}

fn tools(ui: &mut Ui, out: &mut Vec<Command>) {
    ui.set_min_width(MENU_WIDTH);
    // Not from inside itself.
    if !kit::reading() {
        let search = Some(Icon::Search);
        let keys = "Ctrl+Shift+P";
        item(
            ui,
            search,
            "Command palette",
            keys,
            true,
            Command::Palette,
            out,
        );
        ui.separator();
    }
    item(
        ui,
        Some(Icon::Camera),
        "Capture screenshot",
        "Ctrl+PrtSc",
        true,
        Command::Capture,
        out,
    );
    item(ui, None, "Capture dock", "", true, Command::Dock, out);
    ui.separator();
    item(
        ui,
        Some(Icon::Stopwatch),
        "Timesheet",
        "",
        true,
        Command::Timesheet,
        out,
    );
    item(
        ui,
        Some(Icon::Settings),
        "Settings",
        "Ctrl+,",
        true,
        Command::Settings,
        out,
    );
}

fn help(ui: &mut Ui, out: &mut Vec<Command>) {
    ui.set_min_width(MENU_WIDTH);
    let guide = Some(Icon::Help);
    item(ui, guide, "User guide", "", true, Command::Guide, out);
    let about = Some(Icon::Info);
    item(ui, about, "About whir", "", true, Command::About, out);
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::Path;
    use std::sync::Arc;

    use whir_core::{DocCx, TextureRegistry, Waker};

    use super::*;

    /// A document with two entries of its own, one of them greyed out.
    #[derive(Default)]
    struct Doc {
        pressed: Cell<u32>,
        drawn: Cell<u32>,
    }

    impl Document for Doc {
        fn title(&self) -> &str {
            "Doc"
        }

        fn path(&self) -> Option<&Path> {
            None
        }

        fn kind(&self) -> whir_core::DocKind {
            whir_core::DocKind::Note
        }

        fn ui(&mut self, _: &mut Ui, _: &mut DocCx<'_>) {}

        fn menu(&mut self, menu: Menu, ui: &mut Ui, _: &mut DocCx<'_>) {
            if menu == Menu::Edit {
                self.drawn.set(self.drawn.get() + 1);
                if kit::entry(ui, None, "Frob", "Ctrl+Q", true) {
                    self.pressed.set(self.pressed.get() + 1);
                }
                if kit::entry(ui, None, "Greyed", "", false) {
                    self.pressed.set(self.pressed.get() + 100);
                }
            }
        }
    }

    struct NoTextures;

    impl TextureRegistry for NoTextures {
        fn create(
            self: Arc<Self>,
            _: &whir_core::TextureDesc,
            _: &[u8],
            _: usize,
        ) -> Result<whir_core::UserTexture, String> {
            Err("no textures in tests".into())
        }

        fn max_texture_side(&self) -> u32 {
            4096
        }

        fn release(&self, _: u64) {}
    }

    const ON: Enabled = Enabled {
        tab: true,
        reopen: false,
        save: true,
        save_as: true,
        several_tabs: true,
        theme: Mode::Dark,
    };

    /// One frame of reading the menus, with `doc` the active document.
    fn read_with(
        doc: Option<&mut Doc>,
        press: Option<(&str, &str)>,
    ) -> (Vec<(&'static str, kit::Entry)>, Vec<Command>) {
        let ctx = whir_core::theme::test_context();
        let textures: Arc<dyn TextureRegistry> = Arc::new(NoTextures);
        let waker = Waker::noop();
        let mut requests = Vec::new();
        let mut out = None;
        let mut doc = doc;
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut cx = DocCx {
                textures: &textures,
                waker: &waker,
                just_activated: false,
                requests: &mut requests,
            };
            let active = doc.as_mut().map(|d| Active {
                doc: &mut **d,
                cx: &mut cx,
            });
            out = Some(read(ui, active, ON, press));
        });
        frame.textures_delta.clear();
        out.expect("the frame ran")
    }

    fn has(found: &[(&'static str, kit::Entry)], menu: &str, label: &str) -> bool {
        found.iter().any(|(m, e)| *m == menu && e.label == label)
    }

    #[test]
    fn reading_finds_every_menu_and_the_documents_entries() {
        let mut doc = Doc::default();
        let (found, ran) = read_with(Some(&mut doc), None);
        assert!(ran.is_empty());
        assert_eq!(doc.pressed.get(), 0);
        assert!(has(&found, "File", "New note"));
        assert!(has(&found, "Edit", "Frob"));
        assert!(has(&found, "Edit", "Find in all tabs"));
        assert!(has(&found, "View", "Theme: Dark"));
        assert!(has(&found, "Tools", "Settings"));
        assert!(has(&found, "Help", "About whir"));
        let (_, frob) = found.iter().find(|(_, e)| e.label == "Frob").unwrap();
        assert_eq!(frob.shortcut, "Ctrl+Q");
    }

    #[test]
    fn what_cant_be_done_now_isnt_listed() {
        let (found, _) = read_with(None, None);
        // Nothing has been closed yet.
        assert!(!has(&found, "File", "Reopen closed tab"));
        let mut doc = Doc::default();
        let (found, _) = read_with(Some(&mut doc), None);
        assert!(!has(&found, "Edit", "Greyed"));
    }

    #[test]
    fn it_doesnt_list_itself() {
        let (found, _) = read_with(None, None);
        assert!(!found.iter().any(|(_, e)| e.label == "Command palette"));
    }

    #[test]
    fn pressing_runs_that_entry_once_in_that_menu_alone() {
        let mut doc = Doc::default();
        let (_, ran) = read_with(Some(&mut doc), Some(("Edit", "Frob")));
        assert_eq!(doc.pressed.get(), 1);
        assert!(ran.is_empty());

        let mut doc = Doc::default();
        let (_, ran) = read_with(Some(&mut doc), Some(("Tools", "Settings")));
        assert_eq!(doc.pressed.get(), 0);
        assert_eq!(ran, [Command::Settings]);

        // In another menu, it isn't that entry.
        let (_, ran) = read_with(None, Some(("File", "Settings")));
        assert!(ran.is_empty());

        // Greyed out, it can't be pressed.
        let mut doc = Doc::default();
        read_with(Some(&mut doc), Some(("Edit", "Greyed")));
        assert_eq!(doc.pressed.get(), 0);
    }

    #[test]
    fn a_theme_is_picked_by_its_full_name_and_stays_out_of_the_way_otherwise() {
        let (_, ran) = read_with(None, Some(("View", "Theme: Light")));
        assert_eq!(ran, [Command::Theme(Mode::Light)]);
    }

    #[test]
    fn reading_ends_with_the_read() {
        read_with(None, None);
        assert!(!kit::reading());
    }
}
