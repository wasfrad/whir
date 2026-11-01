use std::path::{Path, PathBuf};

use crate::theme::Theme;
use crate::{DocCx, DocKind, Document};

/// A tab that only shows a message: unsupported file, failed load, missing component.
pub struct MessageDoc {
    title: String,
    path: Option<PathBuf>,
    heading: String,
    detail: String,
}

impl MessageDoc {
    pub fn new(
        path: Option<PathBuf>,
        heading: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let title = path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Error".to_owned());
        Self {
            title,
            path,
            heading: heading.into(),
            detail: detail.into(),
        }
    }

    pub fn boxed(self) -> Box<dyn Document> {
        Box::new(self)
    }
}

impl Document for MessageDoc {
    fn title(&self) -> &str {
        &self.title
    }

    fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn kind(&self) -> DocKind {
        DocKind::Error
    }

    fn ui(&mut self, ui: &mut egui::Ui, _cx: &mut DocCx<'_>) {
        show_message(ui, &self.heading, &self.detail, self.path.as_deref());
    }
}

/// Centered heading + detail, used by error states across pillars.
pub fn show_message(ui: &mut egui::Ui, heading: &str, detail: &str, path: Option<&Path>) {
    let theme = Theme::current();
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, theme.content_bg);
    ui.scope_builder(egui::UiBuilder::new().max_rect(ui.max_rect()), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.38 - 40.0).max(12.0));
            ui.label(egui::RichText::new(heading).heading().color(theme.text));
            ui.add_space(8.0);
            if !detail.is_empty() {
                ui.label(egui::RichText::new(detail).color(theme.text_dim));
            }
            if let Some(p) = path {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(p.display().to_string())
                        .small()
                        .color(theme.text_dim),
                );
            }
        });
    });
}
