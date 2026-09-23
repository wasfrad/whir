//! "Save changes?" when closing tabs or the window with unsaved work.
//!
//! One in-app dialog for one document or a combined one for several. Saving
//! can be asynchronous (large tables) or need a location first (Save dialog),
//! so this is a small state machine the shell drives every frame.

use std::path::PathBuf;

use egui::{Align, Key, Layout, Modifiers, RichText};
use whir_core::theme::Theme;
use whir_core::{Persistence, SaveAsSpec, SaveResult, needs_attention};

use crate::app::Tab;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Closing specific tabs.
    Tabs,
    /// Closing whir.
    Window,
}

enum Step {
    Ask,
    /// Background saves running for these tabs.
    Saving(Vec<u64>),
    /// Waiting for the Save dialog for `tab`.
    Dialog {
        tab: u64,
        token: u64,
    },
}

pub enum Outcome {
    Pending,
    /// Everything settled: close these tabs (and the window, for [`Kind::Window`]).
    Finished {
        close: Vec<u64>,
    },
    Aborted,
}

pub struct SaveDialog {
    pub token: u64,
    pub spec: SaveAsSpec,
}

pub struct CloseFlow {
    pub kind: Kind,
    /// Tabs still waiting for a decision or a save.
    queue: Vec<u64>,
    /// Tabs to close at the end (saved, discarded, or clean).
    close: Vec<u64>,
    step: Step,
    error: Option<String>,
    /// Keyboard focus to give back if the user cancels.
    pub restore_focus: Option<egui::Id>,
}

fn find(tabs: &mut [Tab], id: u64) -> Option<&mut Tab> {
    tabs.iter_mut().find(|t| t.id == id)
}

impl CloseFlow {
    /// `queue`: tabs needing a decision; `also_close`: tabs to close regardless.
    pub fn new(
        kind: Kind,
        queue: Vec<u64>,
        also_close: Vec<u64>,
        restore_focus: Option<egui::Id>,
    ) -> Self {
        Self {
            kind,
            queue,
            close: also_close,
            step: Step::Ask,
            error: None,
            restore_focus,
        }
    }

    /// Is this flow waiting for the Save dialog with `token`?
    pub fn owns_dialog(&self, token: u64) -> bool {
        matches!(self.step, Step::Dialog { token: t, .. } if t == token)
    }

    pub fn dialog_closed(&mut self, path: Option<PathBuf>, tabs: &mut [Tab]) {
        let Step::Dialog { tab, .. } = self.step else {
            return;
        };
        self.step = Step::Ask;
        let Some(path) = path else { return };
        let Some(t) = find(tabs, tab) else {
            self.queue.retain(|&q| q != tab);
            return;
        };
        match t.doc.save_as(path) {
            SaveResult::Started => self.step = Step::Saving(vec![tab]),
            SaveResult::Saved => self.settle(tab),
            SaveResult::Failed(e) => self.error = Some(e),
            SaveResult::NeedsPath(_) | SaveResult::Unsupported => {}
        }
    }

    fn settle(&mut self, id: u64) {
        self.queue.retain(|&q| q != id);
        if !self.close.contains(&id) {
            self.close.push(id);
        }
    }

    /// Save everything in the queue. May ask for a Save dialog.
    fn save_all(&mut self, tabs: &mut [Tab], next_token: &mut u64) -> Option<SaveDialog> {
        self.error = None;
        let mut saving = Vec::new();
        let mut need_path = None;
        for id in self.queue.clone() {
            let Some(t) = find(tabs, id) else {
                self.queue.retain(|&q| q != id);
                continue;
            };
            match t.doc.save() {
                SaveResult::Saved if !needs_attention(&*t.doc) => self.settle(id),
                SaveResult::Saved => {
                    self.error = t.doc.save_error().map(str::to_owned);
                }
                SaveResult::Started => saving.push(id),
                SaveResult::NeedsPath(spec) => {
                    if need_path.is_none() {
                        need_path = Some((id, spec));
                    }
                }
                SaveResult::Failed(e) => self.error = Some(e),
                SaveResult::Unsupported => self.settle(id),
            }
        }
        if let Some((tab, spec)) = need_path {
            *next_token += 1;
            let token = *next_token;
            // Saves already started keep running; the dialog comes first.
            self.step = Step::Dialog { tab, token };
            return Some(SaveDialog { token, spec });
        }
        self.step = if saving.is_empty() {
            Step::Ask
        } else {
            Step::Saving(saving)
        };
        None
    }

    fn discard_all(&mut self, tabs: &mut [Tab]) {
        for id in std::mem::take(&mut self.queue) {
            if let Some(t) = find(tabs, id) {
                t.doc.discard();
            }
            if !self.close.contains(&id) {
                self.close.push(id);
            }
        }
    }

    fn finished(&self) -> bool {
        self.queue.is_empty() && matches!(self.step, Step::Ask)
    }

    /// Show the dialog (call before the documents) and advance.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        tabs: &mut [Tab],
        theme: &Theme,
        next_token: &mut u64,
    ) -> (Outcome, Option<SaveDialog>) {
        // Background saves finishing?
        if let Step::Saving(ids) = &mut self.step {
            let mut still = Vec::new();
            let mut failed = None;
            for &id in ids.iter() {
                match tabs.iter().find(|t| t.id == id) {
                    None => {}
                    Some(t) if t.doc.is_saving() => still.push(id),
                    Some(t) if t.doc.save_error().is_some() || needs_attention(&*t.doc) => {
                        failed = Some(
                            t.doc
                                .save_error()
                                .unwrap_or("The file couldn't be saved.")
                                .to_owned(),
                        );
                    }
                    Some(_) => {}
                }
            }
            let done: Vec<u64> = ids
                .iter()
                .copied()
                .filter(|id| !still.contains(id))
                .collect();
            *ids = still;
            for id in done {
                let ok = tabs
                    .iter()
                    .find(|t| t.id == id)
                    .is_none_or(|t| t.doc.save_error().is_none() && !needs_attention(&*t.doc));
                if ok {
                    self.settle(id);
                }
            }
            if let Some(e) = failed {
                self.error = Some(e);
            }
            if matches!(&self.step, Step::Saving(ids) if ids.is_empty()) {
                self.step = Step::Ask;
                // More to save (e.g. one that needed a location)? Carry on.
                if self.error.is_none() && !self.queue.is_empty() {
                    let dialog = self.save_all(tabs, next_token);
                    if dialog.is_some() {
                        return (Outcome::Pending, dialog);
                    }
                }
            }
        }
        if self.finished() {
            return (
                Outcome::Finished {
                    close: std::mem::take(&mut self.close),
                },
                None,
            );
        }

        let names: Vec<String> = self
            .queue
            .iter()
            .filter_map(|id| tabs.iter().find(|t| t.id == *id))
            .map(|t| t.doc.title().to_owned())
            .collect();
        let autosave_failed = self.queue.len() == 1
            && tabs
                .iter()
                .find(|t| t.id == self.queue[0])
                .is_some_and(|t| t.doc.persistence() == Persistence::Auto);
        let busy = !matches!(self.step, Step::Ask);

        #[derive(PartialEq)]
        enum Choice {
            Save,
            SaveAs,
            Discard,
            Cancel,
        }
        let mut choice = None;
        let modal = egui::Modal::new(egui::Id::new("whir.close"))
            .backdrop_color(theme.modal_backdrop)
            .frame(
                egui::Frame::popup(&ctx.global_style())
                    .inner_margin(20.0)
                    .corner_radius(10.0),
            )
            .show(ctx, |ui| {
                ui.set_width(380.0);
                let heading = match (names.len(), autosave_failed) {
                    (1, true) => format!("“{}” couldn't be saved", names[0]),
                    (1, false) => format!("Save changes to “{}”?", names[0]),
                    (n, _) => format!("Save changes to {n} documents?"),
                };
                ui.label(RichText::new(heading).size(16.0).color(theme.text));
                ui.add_space(6.0);
                if names.len() > 1 {
                    for n in names.iter().take(6) {
                        ui.label(RichText::new(format!("•  {n}")).color(theme.text_dim));
                    }
                    if names.len() > 6 {
                        ui.label(
                            RichText::new(format!("and {} more", names.len() - 6))
                                .color(theme.text_dim),
                        );
                    }
                    ui.add_space(4.0);
                }
                let detail = if autosave_failed {
                    "Try again, save it somewhere else, or discard the changes."
                } else {
                    "Your changes will be lost if you don't save them."
                };
                ui.label(RichText::new(detail).color(theme.text_dim));
                if let Some(e) = &self.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(e).color(theme.danger));
                }
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("  Cancel  ").clicked() {
                        choice = Some(Choice::Cancel);
                    }
                    if busy {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(RichText::new("Saving…").color(theme.text_dim));
                        return;
                    }
                    let discard = if autosave_failed {
                        "Discard"
                    } else {
                        "Don't save"
                    };
                    if ui.button(format!("  {discard}  ")).clicked() {
                        choice = Some(Choice::Discard);
                    }
                    if autosave_failed && ui.button("  Save as…  ").clicked() {
                        choice = Some(Choice::SaveAs);
                    }
                    let primary = match (names.len(), autosave_failed) {
                        (_, true) => "Try again",
                        (1, false) => "Save",
                        _ => "Save all",
                    };
                    let save = ui.add(
                        egui::Button::new(
                            RichText::new(format!("  {primary}  ")).color(egui::Color32::WHITE),
                        )
                        .fill(theme.accent),
                    );
                    if save.clicked() {
                        choice = Some(Choice::Save);
                    }
                });
                // Enter saves (the dialog has no focusable default button).
                if !busy && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                    choice = Some(Choice::Save);
                }
            });
        if modal.should_close() && choice.is_none() {
            choice = Some(Choice::Cancel);
        }
        let mut dialog = None;
        match choice {
            Some(Choice::Cancel) => return (Outcome::Aborted, None),
            Some(Choice::Discard) => self.discard_all(tabs),
            Some(Choice::Save) => dialog = self.save_all(tabs, next_token),
            Some(Choice::SaveAs) => {
                if let Some(&id) = self.queue.first()
                    && let Some(spec) = tabs
                        .iter()
                        .find(|t| t.id == id)
                        .and_then(|t| t.doc.save_as_spec())
                {
                    *next_token += 1;
                    self.step = Step::Dialog {
                        tab: id,
                        token: *next_token,
                    };
                    dialog = Some(SaveDialog {
                        token: *next_token,
                        spec,
                    });
                }
            }
            None => {}
        }
        if self.finished() && dialog.is_none() {
            return (
                Outcome::Finished {
                    close: std::mem::take(&mut self.close),
                },
                None,
            );
        }
        if matches!(self.step, Step::Saving(_)) {
            // Poll the background saves.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        (Outcome::Pending, dialog)
    }
}
