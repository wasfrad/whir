//! How whir starts, where its window opens, the capture dock, and where its
//! files are.

use egui::RichText;
use whir_core::DocCx;
use whir_core::kit;
use whir_core::theme::Theme;
use whir_platform::{placement, shell};

use super::{heading, note, toast};

pub fn ui(ui: &mut egui::Ui, cx: &mut DocCx<'_>, theme: &Theme) {
    heading(ui, "Startup", theme);
    let mut on = shell::autostart().is_some();
    if kit::checkbox(ui, &mut on, "Start whir when you sign in to Windows", theme).changed() {
        if let Err(e) = shell::set_autostart(on) {
            toast(cx, e);
        }
    }
    note(
        ui,
        "It starts the capture dock, the part of whir that keeps running: screenshots, \
         timers and todos. Its icon in the notification area has Quit.",
        theme,
    );
    ui.add_space(8.0);
    if ui.button("Show the capture dock").clicked()
        && let Err(e) = whir_capture::launch(whir_capture::Command::Reveal)
    {
        toast(cx, format!("The capture dock couldn't be started: {e}"));
    }

    ui.add_space(24.0);
    heading(ui, "Window", theme);
    let mut exact = placement::exact();
    let before = exact;
    ui.radio_value(
        &mut exact,
        false,
        "Open where you left it, or on the monitor the mouse is on if that's another one",
    );
    ui.radio_value(&mut exact, true, "Always open exactly where you left it");
    if exact != before {
        placement::set_exact(exact);
    }
    note(
        ui,
        "Either way it keeps its size, and opens maximized if it was.",
        theme,
    );

    ui.add_space(24.0);
    heading(ui, "Folders", theme);
    let folders = [
        ("Notes", whir_core::library::resolve_dir()),
        ("Screenshots", whir_core::captures::resolve_dir()),
        ("Time", whir_core::timesheet::resolve_dir()),
    ];
    egui::Grid::new("whir.folders")
        .spacing(egui::vec2(14.0, 6.0))
        .show(ui, |ui| {
            for (name, dir) in folders {
                ui.label(RichText::new(name).color(theme.text));
                match dir {
                    Some(dir) => {
                        ui.label(RichText::new(dir.display().to_string()).color(theme.text_dim));
                        if ui.button("Open").clicked() {
                            shell::open_folder(&dir);
                        }
                    }
                    None => {
                        ui.label(RichText::new("not available").color(theme.text_dim));
                    }
                }
                ui.end_row();
            }
        });
    ui.add_space(4.0);
    note(
        ui,
        "Settings of their own are kept in %LOCALAPPDATA%\\whir: settings.ini, views.txt \
         (where each PDF was left), and dock.ini for the dock (its place, delay and hotkey).",
        theme,
    );
}
