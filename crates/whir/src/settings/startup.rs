//! How whir starts, where its window opens, and where its files are.

use egui::RichText;
use whir_core::DocCx;
use whir_core::kit;
use whir_core::theme::Theme;
use whir_platform::{placement, shell};

use super::toast;

pub fn ui(ui: &mut egui::Ui, cx: &mut DocCx<'_>, theme: &Theme) {
    kit::page_header(ui, "Startup & window", "", theme, |_| {});

    kit::group(ui, "Startup", theme, |g| {
        let mut on = shell::autostart().is_some();
        if g.switch(
            "Start whir when you sign in to Windows",
            "This starts the capture dock, the part of whir that keeps running. Its icon in \
             the notification area has Quit.",
            &mut on,
        ) && let Err(e) = shell::set_autostart(on)
        {
            toast(cx, e);
        }
    });

    kit::group(ui, "Window", theme, |g| {
        if let Some(exact) = g.choice(
            "Open the window",
            "Either way it keeps its size, and opens maximized if it was.",
            placement::exact(),
            &[false, true],
            |exact| {
                if exact {
                    "Exactly where it was left".to_owned()
                } else {
                    "On the monitor the mouse is on".to_owned()
                }
            },
        ) {
            placement::set_exact(exact);
        }
    });

    kit::group(ui, "Folders", theme, |g| {
        let folders = [
            ("Notes", whir_core::library::resolve_dir()),
            ("Screenshots", whir_core::captures::resolve_dir()),
            ("Time", whir_core::timesheet::resolve_dir()),
        ];
        for (name, dir) in folders {
            let path = dir
                .as_ref()
                .map_or_else(|| "Not available".to_owned(), |d| d.display().to_string());
            g.setting(name, &path, 72.0, |ui| {
                if let Some(dir) = &dir
                    && kit::action(ui, "Open", theme).clicked()
                {
                    shell::open_folder(dir);
                }
            });
        }
    });

    ui.label(
        RichText::new(
            "whir's own settings are in %LOCALAPPDATA%\\whir: settings.ini, views.txt (where \
             each PDF was left), and dock.ini for the dock.",
        )
        .small()
        .color(theme.text_dim),
    );
}
