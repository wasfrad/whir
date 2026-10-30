//! How PDFs and images open: at which zoom, and each PDF where it was left.

use egui::RichText;
use whir_core::DocCx;
use whir_core::theme::Theme;
use whir_core::views;
use whir_image::prefs as image;
use whir_pdf::prefs as pdf;

use super::{heading, note};

pub fn ui(ui: &mut egui::Ui, _cx: &mut DocCx<'_>, theme: &Theme) {
    heading(ui, "PDFs", theme);
    let label = |o: pdf::Opening| match o {
        pdf::Opening::Last => format!("{} (now: {})", o.label(), pdf::last().label()),
        o => o.label().to_owned(),
    };
    let current = pdf::opening();
    if let Some(o) = choice(
        ui,
        "whir.settings.pdf.zoom",
        "Open a PDF at",
        current,
        &pdf::Opening::ALL,
        label,
        theme,
    ) {
        pdf::set_opening(o);
    }
    note(
        ui,
        "The zoom you used last is the one you last picked on any PDF: 0 fit page, 2 fit width, \
         1 actual size (also on the right-click menu), or zooming in and out. Pick Fit page \
         once, and every PDF after opens with the whole page in view.",
        theme,
    );
    ui.add_space(10.0);
    let mut remember = pdf::remember();
    if ui
        .checkbox(
            &mut remember,
            "Reopen each PDF where you left it (its page and zoom)",
        )
        .changed()
    {
        pdf::set_remember(remember);
    }
    let n = views::len();
    if n > 0 {
        ui.add_space(2.0);
        let what = if n == 1 {
            "Forget where 1 PDF was left".to_owned()
        } else {
            format!("Forget where {n} PDFs were left")
        };
        if ui.button(what).clicked() {
            views::forget_all();
        }
    }
    ui.add_space(6.0);
    let mut minimap = pdf::minimap();
    if ui
        .checkbox(
            &mut minimap,
            "Show the minimap of pages beside the scrollbar (Ctrl+M)",
        )
        .changed()
    {
        pdf::set_minimap(minimap);
    }

    ui.add_space(24.0);
    heading(ui, "Images", theme);
    let label = |o: image::Opening| match o {
        image::Opening::Last => format!("{} (now: {})", o.label(), image::last().label()),
        o => o.label().to_owned(),
    };
    let current = image::opening();
    if let Some(o) = choice(
        ui,
        "whir.settings.image.zoom",
        "Open an image at",
        current,
        &image::Opening::ALL,
        label,
        theme,
    ) {
        image::set_opening(o);
    }
    note(
        ui,
        "Fitted, a large image shrinks to the window and a small one stays at 100%. ← → \
         through a folder opens each image the same way.",
        theme,
    );
}

/// A labelled drop-down of `options`; the one picked, if it changed.
fn choice<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    text: &str,
    current: T,
    options: &[T],
    label: impl Fn(T) -> String,
    theme: &Theme,
) -> Option<T> {
    let mut picked = current;
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).color(theme.text));
        egui::ComboBox::from_id_salt(id)
            .selected_text(label(current))
            .width(280.0)
            .show_ui(ui, |ui| {
                for &o in options {
                    ui.selectable_value(&mut picked, o, label(o));
                }
            });
    });
    (picked != current).then_some(picked)
}
