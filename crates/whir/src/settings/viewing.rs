//! How PDFs and images open: at which zoom, and each PDF where it was left.

use whir_core::DocCx;
use whir_core::kit;
use whir_core::theme::Theme;
use whir_core::views;
use whir_image::prefs as image;
use whir_pdf::prefs as pdf;

pub fn ui(ui: &mut egui::Ui, _cx: &mut DocCx<'_>, theme: &Theme) {
    kit::page_header(ui, "PDFs & images", "", theme, |_| {});

    kit::group(ui, "PDFs", theme, |g| {
        let opening = pdf::opening();
        let about = match opening {
            pdf::Opening::Last => format!(
                "The one last picked on any PDF, now {}.",
                pdf::last().label().to_lowercase()
            ),
            _ => String::new(),
        };
        if let Some(o) = g.choice(
            "Open a PDF at",
            &about,
            opening,
            &pdf::Opening::ALL,
            |o: pdf::Opening| o.label().to_owned(),
        ) {
            pdf::set_opening(o);
        }

        let mut remember = pdf::remember();
        if g.switch(
            "Reopen each PDF where you left it",
            "Its page and its zoom.",
            &mut remember,
        ) {
            pdf::set_remember(remember);
        }
        let n = views::len();
        if n > 0 {
            let what = match n {
                1 => "1 PDF remembered".to_owned(),
                n => format!("{n} PDFs remembered"),
            };
            g.setting("Where PDFs were left", &what, 72.0, |ui| {
                if kit::action(ui, "Forget", theme).clicked() {
                    views::forget_all();
                }
            });
        }

        let mut minimap = pdf::minimap();
        if g.switch(
            "Show the minimap",
            "Pages beside the scrollbar. Ctrl+M switches it in a PDF.",
            &mut minimap,
        ) {
            pdf::set_minimap(minimap);
        }
    });

    kit::group(ui, "Images", theme, |g| {
        let opening = image::opening();
        let about = match opening {
            image::Opening::Last => format!(
                "The one last picked on any image, now {}.",
                image::last().label().to_lowercase()
            ),
            image::Opening::Fit => {
                "A large image shrinks to the window; a small one stays at 100%.".to_owned()
            }
            image::Opening::Actual => String::new(),
        };
        if let Some(o) = g.choice(
            "Open an image at",
            &about,
            opening,
            &image::Opening::ALL,
            |o: image::Opening| o.label().to_owned(),
        ) {
            image::set_opening(o);
        }
    });
}
