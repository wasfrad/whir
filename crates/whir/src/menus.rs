//! The menu bar: everything whir can do, in one place, with its shortcut beside
//! it. What belongs to the open document comes from [`Document::menu`].

use egui::containers::menu::{MenuBar, MenuButton, SubMenuButton, menu_style};
use egui::{Align, Button, Layout, Style, Ui, vec2};
use whir_core::icons::{self, Icon};
use whir_core::kit;
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
            SubMenuButton::new("File").ui(ui, |ui| file(ui, on, &mut out));
            SubMenuButton::new("Edit").ui(ui, |ui| edit(ui, active.as_mut(), &mut out));
            SubMenuButton::new("View").ui(ui, |ui| view(ui, active.as_mut(), on, &mut out));
            SubMenuButton::new("Tools").ui(ui, |ui| tools(ui, &mut out));
            SubMenuButton::new("Help").ui(ui, |ui| help(ui, &mut out));
        });
    }
    out
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

fn tools(ui: &mut Ui, out: &mut Vec<Command>) {
    ui.set_min_width(MENU_WIDTH);
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
