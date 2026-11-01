//! A PDF's tools, in the toolbar and the menus. What they ask for waits in
//! [`PdfDoc::pending`] and is carried out with the keys in the next `show`, so
//! a button can't behave differently from its shortcut.

use egui::Ui;
use egui::containers::menu::MenuButton;
use whir_core::icons::Icon;
use whir_core::theme::Theme;
use whir_core::{Menu, kit, toolbar};

use super::{PdfDoc, State, ZOOM_100, Zoom, minimap};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Cmd {
    ZoomIn,
    ZoomOut,
    Zoom(Zoom),
    /// Next (true) or previous page.
    Step(bool),
    GoTo,
    ToggleMinimap,
    Copy,
    SelectAll,
    Find,
}

impl PdfDoc {
    /// The page most in view (from 0), and how many there are.
    fn place_in_document(&self) -> Option<(usize, usize)> {
        let State::Ready(l) = &self.state else {
            return None;
        };
        if l.is_empty() {
            return None;
        }
        let zoom = self.shown_zoom.unwrap_or(1.0);
        let top = self.sy.offset / zoom;
        let bottom = (self.sy.offset + self.shown_vp.height() as f64) / zoom;
        Some((l.most_visible(top, bottom), l.len()))
    }

    pub(super) fn tools(&mut self, ui: &mut Ui) {
        let Some((page, pages)) = self.place_in_document() else {
            return;
        };
        let theme = Theme::current();
        let first = page > 0;
        if toolbar::tool(
            ui,
            Icon::ChevronUp,
            "Previous page (Ctrl+Up)",
            first,
            &theme,
        ) {
            self.pending.push(Cmd::Step(false));
        }
        let place = format!("{} / {pages}", page + 1);
        if toolbar::readout(ui, &place, 72.0, &theme)
            .on_hover_text("Go to page (Ctrl+G)")
            .clicked()
        {
            self.pending.push(Cmd::GoTo);
        }
        let last = page + 1 < pages;
        if toolbar::tool(ui, Icon::ChevronDown, "Next page (Ctrl+Down)", last, &theme) {
            self.pending.push(Cmd::Step(true));
        }
        if ui.available_width() < 150.0 {
            return;
        }
        kit::bar_separator(ui, &theme);
        if toolbar::tool(ui, Icon::ZoomOut, "Zoom out (Ctrl+-)", true, &theme) {
            self.pending.push(Cmd::ZoomOut);
        }
        let zoom = self.shown_zoom.unwrap_or(ZOOM_100);
        let percent = format!("{:.0}%", zoom / ZOOM_100 * 100.0);
        MenuButton::from_button(toolbar::readout_button(&percent, 56.0, &theme))
            .ui(ui, |ui| self.fits(ui));
        if toolbar::tool(ui, Icon::ZoomIn, "Zoom in (Ctrl+=)", true, &theme) {
            self.pending.push(Cmd::ZoomIn);
        }
        if ui.available_width() < 80.0 {
            return;
        }
        kit::bar_separator(ui, &theme);
        if toolbar::tool(ui, Icon::Search, "Find (Ctrl+F)", true, &theme) {
            self.pending.push(Cmd::Find);
        }
        if toolbar::toggle(
            ui,
            Icon::Minimap,
            minimap::enabled(),
            "Minimap (Ctrl+M)",
            &theme,
        ) {
            self.pending.push(Cmd::ToggleMinimap);
        }
    }

    /// The fits, in a menu.
    fn fits(&mut self, ui: &mut Ui) {
        ui.set_min_width(170.0);
        for (icon, zoom, label, key) in [
            (Some(Icon::FitPage), Zoom::FitPage, "Fit page", "0"),
            (Some(Icon::FitWidth), Zoom::FitWidth, "Fit width", "2"),
            (None, Zoom::Manual(ZOOM_100), "Actual size", "1"),
        ] {
            if ui
                .add(kit::menu_entry(icon, label, key).selected(self.zoom == zoom))
                .clicked()
            {
                self.pending.push(Cmd::Zoom(zoom));
                ui.close();
            }
        }
    }

    pub(super) fn entries(&mut self, menu: Menu, ui: &mut Ui) {
        if !matches!(self.state, State::Ready(_)) {
            return;
        }
        match menu {
            Menu::Edit => {
                let selected = self.sel.is_some_and(|s| !s.is_empty());
                if self.text.available {
                    let copy = Some(Icon::Copy);
                    if kit::entry(ui, copy, "Copy", "Ctrl+C", selected) {
                        self.pending.push(Cmd::Copy);
                    }
                    if kit::entry(ui, None, "Select all", "Ctrl+A", true) {
                        self.pending.push(Cmd::SelectAll);
                    }
                    ui.separator();
                }
                if kit::entry(ui, Some(Icon::Search), "Find", "Ctrl+F", true) {
                    self.pending.push(Cmd::Find);
                }
            }
            Menu::View => {
                if kit::entry(ui, Some(Icon::ZoomIn), "Zoom in", "Ctrl+=", true) {
                    self.pending.push(Cmd::ZoomIn);
                }
                if kit::entry(ui, Some(Icon::ZoomOut), "Zoom out", "Ctrl+-", true) {
                    self.pending.push(Cmd::ZoomOut);
                }
                self.fits(ui);
                ui.separator();
                if kit::entry(ui, None, "Previous page", "Ctrl+Up", true) {
                    self.pending.push(Cmd::Step(false));
                }
                if kit::entry(ui, None, "Next page", "Ctrl+Down", true) {
                    self.pending.push(Cmd::Step(true));
                }
                if kit::entry(ui, None, "Go to page\u{2026}", "Ctrl+G", true) {
                    self.pending.push(Cmd::GoTo);
                }
                ui.separator();
                if kit::check_entry(ui, minimap::enabled(), "Minimap", "Ctrl+M") {
                    self.pending.push(Cmd::ToggleMinimap);
                }
                ui.separator();
            }
        }
    }

    pub(super) fn status_line(&self) -> String {
        match self.place_in_document() {
            Some((_, 1)) => "1 page".to_owned(),
            Some((_, pages)) => format!("{pages} pages"),
            None => String::new(),
        }
    }
}
