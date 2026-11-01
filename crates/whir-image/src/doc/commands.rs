//! An image's view and history tools, in the toolbar and the menus. What they
//! ask for waits in `ImageDoc::pending` and is carried out with the keys in
//! the next frame, so a button can't behave differently from its shortcut.

use egui::Ui;
use egui::containers::menu::MenuButton;
use whir_core::icons::Icon;
use whir_core::theme::Theme;
use whir_core::{Menu, kit, toolbar};

use super::ImageDoc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cmd {
    ZoomIn,
    ZoomOut,
    Fit,
    ActualSize,
    /// Through the folder.
    Go(isize),
    /// Clockwise (true) or back.
    Turn(bool),
    Undo,
    Redo,
    Copy,
}

impl ImageDoc {
    /// Through the folder, zoom and rotate: before the tools that mark it.
    pub(super) fn view_tools(&mut self, ui: &mut Ui, theme: &Theme) {
        if self.shown.is_none() {
            return;
        }
        if let (Some(list), Some(i)) = (&self.folder, self.index_in_folder())
            && list.len() > 1
        {
            let (count, first, last) = (list.len(), i > 0, i + 1 < list.len());
            if toolbar::tool(ui, Icon::ChevronLeft, "Previous image (Left)", first, theme) {
                self.pending.push(Cmd::Go(-1));
            }
            let place = format!("{} / {count}", i + 1);
            toolbar::readout(ui, &place, 64.0, theme);
            if toolbar::tool(ui, Icon::ChevronRight, "Next image (Right)", last, theme) {
                self.pending.push(Cmd::Go(1));
            }
            kit::bar_separator(ui, theme);
        }
        if toolbar::tool(ui, Icon::ZoomOut, "Zoom out (-)", true, theme) {
            self.pending.push(Cmd::ZoomOut);
        }
        let percent = self
            .shown_percent
            .map_or_else(|| "\u{2013}".to_owned(), |p| format!("{p:.0}%"));
        MenuButton::from_button(toolbar::readout_button(&percent, 56.0, theme))
            .ui(ui, |ui| self.fits(ui));
        if toolbar::tool(ui, Icon::ZoomIn, "Zoom in (+)", true, theme) {
            self.pending.push(Cmd::ZoomIn);
        }
        if toolbar::tool(ui, Icon::Rotate, "Rotate right (R)", true, theme) {
            self.pending.push(Cmd::Turn(true));
        }
        kit::bar_separator(ui, theme);
    }

    /// Undo, redo and copy: after the tools that mark it.
    pub(super) fn history_tools(&mut self, ui: &mut Ui, theme: &Theme) {
        if self.shown.is_none() || ui.available_width() < 120.0 {
            return;
        }
        kit::bar_separator(ui, theme);
        let (undo, redo) = self.history();
        if toolbar::tool(ui, Icon::Undo, "Undo (Ctrl+Z)", undo, theme) {
            self.pending.push(Cmd::Undo);
        }
        if toolbar::tool(ui, Icon::Redo, "Redo (Ctrl+Y)", redo, theme) {
            self.pending.push(Cmd::Redo);
        }
        if toolbar::tool(ui, Icon::Copy, "Copy image (Ctrl+C)", true, theme) {
            self.pending.push(Cmd::Copy);
        }
    }

    /// Is there a mark to undo, and to redo?
    fn history(&self) -> (bool, bool) {
        self.editor()
            .map_or((false, false), |e| (e.can_undo(), e.can_redo()))
    }

    fn fits(&mut self, ui: &mut Ui) {
        ui.set_min_width(170.0);
        if kit::entry(ui, Some(Icon::FitPage), "Fit to window", "0", true) {
            self.pending.push(Cmd::Fit);
        }
        if kit::entry(ui, None, "Actual size", "1", true) {
            self.pending.push(Cmd::ActualSize);
        }
    }

    pub(super) fn entries(&mut self, menu: Menu, ui: &mut Ui) {
        if self.shown.is_none() {
            return;
        }
        match menu {
            Menu::Edit => {
                let (undo, redo) = self.history();
                if kit::entry(ui, Some(Icon::Undo), "Undo", "Ctrl+Z", undo) {
                    self.pending.push(Cmd::Undo);
                }
                if kit::entry(ui, Some(Icon::Redo), "Redo", "Ctrl+Y", redo) {
                    self.pending.push(Cmd::Redo);
                }
                ui.separator();
                if kit::entry(ui, Some(Icon::Copy), "Copy image", "Ctrl+C", true) {
                    self.pending.push(Cmd::Copy);
                }
                ui.separator();
            }
            Menu::View => {
                if kit::entry(ui, Some(Icon::ZoomIn), "Zoom in", "+", true) {
                    self.pending.push(Cmd::ZoomIn);
                }
                if kit::entry(ui, Some(Icon::ZoomOut), "Zoom out", "-", true) {
                    self.pending.push(Cmd::ZoomOut);
                }
                self.fits(ui);
                ui.separator();
                if kit::entry(ui, Some(Icon::Rotate), "Rotate right", "R", true) {
                    self.pending.push(Cmd::Turn(true));
                }
                if kit::entry(ui, None, "Rotate left", "Shift+R", true) {
                    self.pending.push(Cmd::Turn(false));
                }
                ui.separator();
                if kit::entry(ui, None, "Previous image", "Left", true) {
                    self.pending.push(Cmd::Go(-1));
                }
                if kit::entry(ui, None, "Next image", "Right", true) {
                    self.pending.push(Cmd::Go(1));
                }
                ui.separator();
            }
        }
    }

    pub(super) fn status_line(&self) -> String {
        self.shown.as_ref().map_or_else(String::new, |shown| {
            let size = shown.full_size();
            format!("{:.0} \u{d7} {:.0}", size.x, size.y)
        })
    }
}
