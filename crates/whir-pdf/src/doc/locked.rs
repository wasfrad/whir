//! A PDF that needs a password: the tab asks for it. Each one tried goes to the
//! PDF thread for that one attempt to open the file. None is saved, so the file
//! asks again the next time.

use std::path::Path;

use egui::text_edit::TextEditState;
use egui::{Key, Modifiers, RichText, TextEdit, Ui};
use whir_core::kit;
use whir_core::message_doc::show_message_above;
use whir_core::theme::Theme;

const FIELD_WIDTH: f32 = 240.0;

pub(super) struct Locked {
    /// What's been typed so far.
    password: String,
    /// The last one tried wasn't it.
    pub(super) wrong: bool,
    /// One is with the PDF thread now.
    pub(super) checking: bool,
    focus_pending: bool,
}

impl Locked {
    pub(super) fn new(wrong: bool) -> Self {
        Self {
            password: String::new(),
            wrong,
            checking: false,
            focus_pending: true,
        }
    }

    /// The prompt. Returns a password to try, when one is given.
    pub(super) fn show(&mut self, ui: &mut Ui, path: &Path, activated: bool) -> Option<String> {
        let theme = Theme::current();
        let mut submit = false;
        show_message_above(
            ui,
            "This PDF is password-protected",
            "Enter its password to open it.",
            Some(path),
            |ui| {
                // Its own per file: egui keeps a field's undo history under this.
                let id = ui.id().with(("pdf.password", path));
                ui.add_space(16.0);
                let field = ui.add(
                    TextEdit::singleline(&mut self.password)
                        .id(id)
                        .password(true)
                        .hint_text("Password")
                        // Enter tries it, and leaves the keyboard here for another go.
                        .return_key(None)
                        .desired_width(FIELD_WIDTH),
                );
                // A tab comes up on the press of a click, and egui takes the keyboard
                // from a field when a click ends elsewhere. So: once the click is
                // over, and after the field is made, which is when egui checks.
                self.focus_pending |= activated;
                if self.focus_pending && !ui.input(|i| i.pointer.any_down()) {
                    self.focus_pending = false;
                    field.request_focus();
                    ui.ctx().request_repaint();
                }
                submit |= field.has_focus()
                    && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));

                // Always there, so nothing moves when it says something.
                let message = if self.wrong {
                    "That password isn't right."
                } else {
                    " "
                };
                ui.add_space(6.0);
                ui.label(RichText::new(message).color(theme.error));
                ui.add_space(6.0);

                let ready = !self.checking && !self.password.is_empty();
                submit |= ui
                    .add_enabled_ui(ready, |ui| kit::primary(ui, "Open", &theme))
                    .inner
                    .clicked();
                submit &= ready;
                if submit {
                    // Its undo history would be a copy of it.
                    ui.ctx().data_mut(|d| d.remove::<TextEditState>(id));
                }
            },
        );
        submit.then(|| {
            self.checking = true;
            std::mem::take(&mut self.password)
        })
    }
}
