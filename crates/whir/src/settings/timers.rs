//! Customers and their projects (what time is tracked against), and the
//! time-tracking options.

use std::collections::HashSet;

use egui::{Id, RichText, vec2};
use whir_core::DocCx;
use whir_core::theme::Theme;
use whir_core::timesheet::{Rounding, Store, Txn};

use super::{edit_line, heading, note, primary};

/// A change to make to the timesheet, decided while drawing.
type Change = Box<dyn FnOnce(&mut Txn)>;

#[derive(Default)]
pub struct View {
    new_customer: String,
    /// The customer a project is being added to, its name and code, and
    /// whether the name field still needs the keyboard.
    new_project: Option<(u32, String, String, bool)>,
    /// Timers with time in any week (those can be archived, not deleted).
    used: Option<HashSet<u32>>,
    error: Option<String>,
}

impl View {
    /// The timesheet changed on disk: what was read from it goes.
    pub fn forget(&mut self) {
        self.used = None;
    }

    fn change(&mut self, store: &mut Store, change: impl FnOnce(&mut Txn)) {
        match store.update(change) {
            Ok(()) => self.error = None,
            Err(e) => self.error = Some(e),
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        store: Option<&mut Store>,
        _cx: &mut DocCx<'_>,
        theme: &Theme,
    ) {
        heading(ui, "Customers & projects", theme);
        let Some(store) = store else {
            note(
                ui,
                "The Documents folder isn't available, so there is nowhere to keep time.",
                theme,
            );
            return;
        };
        note(
            ui,
            "Each project is a timer in the dock's time drawer. The code (a project or \
             assignment ID) goes on the timesheet beside it.",
            theme,
        );
        ui.add_space(10.0);
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(theme.danger));
            ui.add_space(6.0);
        }

        let used = self.used.get_or_insert_with(|| {
            let mut used: HashSet<u32> = store.state().running.iter().map(|r| r.timer).collect();
            for m in store.mondays() {
                used.extend(store.week(m).entries.iter().map(|e| e.timer));
            }
            used
        });
        let used = used.clone();
        let state = store.state().clone();
        let mut change: Option<Change> = None;
        for (customer, timers) in state.by_customer(true, true) {
            let cid = customer.id;
            ui.horizontal(|ui| {
                let id = Id::new(("whir.customer", cid));
                if let Some(name) = edit_line(ui, id, &customer.name, 240.0, "Customer")
                    && !name.is_empty()
                {
                    change = Some(Box::new(move |tx| {
                        if let Some(c) = tx.state.customer_mut(cid) {
                            c.name = name;
                        }
                    }));
                }
                if ui.button("+ Project").clicked() {
                    self.new_project = Some((cid, String::new(), String::new(), true));
                }
                if timers.is_empty()
                    && ui
                        .button("Delete")
                        .on_hover_text("No projects: it can go")
                        .clicked()
                {
                    change = Some(Box::new(move |tx| {
                        tx.state.customers.retain(|c| c.id != cid)
                    }));
                }
            });
            ui.indent(("whir.projects", cid), |ui| {
                egui::Grid::new(("whir.projects.grid", cid))
                    .spacing(vec2(8.0, 4.0))
                    .show(ui, |ui| {
                        for t in timers {
                            let tid = t.id;
                            let id = |part: &str| Id::new(("whir.timer", tid, part.to_owned()));
                            let shade = if t.archived {
                                theme.text_dim
                            } else {
                                theme.text
                            };
                            if let Some(name) = edit_line(ui, id("name"), &t.name, 220.0, "Project")
                                && !name.is_empty()
                            {
                                change = Some(Box::new(move |tx| {
                                    if let Some(t) = tx.state.timer_mut(tid) {
                                        t.name = name;
                                    }
                                }));
                            }
                            if let Some(code) = edit_line(ui, id("code"), &t.code, 130.0, "Code") {
                                change = Some(Box::new(move |tx| {
                                    if let Some(t) = tx.state.timer_mut(tid) {
                                        t.code = code;
                                    }
                                }));
                            }
                            let archived = t.archived;
                            let toggle = if archived { "Restore" } else { "Archive" };
                            if ui
                                .button(toggle)
                                .on_hover_text(if archived {
                                    "Offer it in the dock again"
                                } else {
                                    "No longer offered in the dock; its time stays"
                                })
                                .clicked()
                            {
                                change = Some(Box::new(move |tx| {
                                    if let Some(t) = tx.state.timer_mut(tid) {
                                        t.archived = !archived;
                                    }
                                }));
                            }
                            if !used.contains(&tid) && ui.button("Delete").clicked() {
                                change = Some(Box::new(move |tx| {
                                    tx.state.timers.retain(|t| t.id != tid);
                                    if tx.state.last == Some(tid) {
                                        tx.state.last = None;
                                    }
                                }));
                            }
                            if archived {
                                ui.label(RichText::new("archived").color(shade));
                            }
                            ui.end_row();
                        }
                    });
                if let Some((on, name, code, focus)) = &mut self.new_project
                    && *on == cid
                {
                    let (mut add, mut cancel) = (false, false);
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(name)
                                .desired_width(220.0)
                                .hint_text("Project"),
                        );
                        if std::mem::take(focus) {
                            r.request_focus();
                        }
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.add(
                            egui::TextEdit::singleline(code)
                                .desired_width(130.0)
                                .hint_text("Code (optional)"),
                        );
                        add = primary(ui, "Add", theme) || enter;
                        cancel = ui.button("Cancel").clicked();
                    });
                    if cancel {
                        self.new_project = None;
                    }
                    if add
                        && let Some((cid, name, code, _)) = self.new_project.take()
                        && !name.trim().is_empty()
                    {
                        change = Some(Box::new(move |tx| {
                            tx.state.add_timer(cid, &name, &code);
                        }));
                    }
                }
            });
            ui.add_space(8.0);
        }

        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.new_customer)
                    .desired_width(240.0)
                    .hint_text("New customer"),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("+ Customer").clicked() || enter) && !self.new_customer.trim().is_empty()
            {
                let name = std::mem::take(&mut self.new_customer);
                change = Some(Box::new(move |tx| {
                    tx.state.add_customer(&name);
                }));
            }
        });
        if let Some(c) = change {
            self.change(store, c);
        }

        ui.add_space(24.0);
        self.options(ui, store, theme);
    }

    fn options(&mut self, ui: &mut egui::Ui, store: &mut Store, theme: &Theme) {
        heading(ui, "Options", theme);
        let o = store.state().options;
        let mut rounding = o.rounding;
        let mut away = o.away_minutes;
        let mut keep_out = o.keep_out;
        ui.horizontal(|ui| {
            ui.label("Round each project's day to");
            egui::ComboBox::from_id_salt("whir.rounding")
                .selected_text(rounding.label())
                .show_ui(ui, |ui| {
                    for r in Rounding::ALL {
                        ui.selectable_value(&mut rounding, r, r.label());
                    }
                });
        });
        note(
            ui,
            "For the timesheet only: entries keep the exact time.",
            theme,
        );
        ui.add_space(6.0);
        let minutes = |m: u32| match m {
            0 => "Never".to_owned(),
            m => format!("{m} minutes"),
        };
        ui.horizontal(|ui| {
            ui.label("Ask whether time away counts after");
            egui::ComboBox::from_id_salt("whir.away")
                .selected_text(minutes(away))
                .show_ui(ui, |ui| {
                    for m in [5, 10, 15, 20, 30, 45, 60, 0] {
                        ui.selectable_value(&mut away, m, minutes(m));
                    }
                });
        });
        note(
            ui,
            "Away: no keyboard or mouse, a locked screen, or sleep.",
            theme,
        );
        ui.add_space(6.0);
        ui.checkbox(&mut keep_out, "Keep the dock out while a timer runs");
        if (rounding, away, keep_out) != (o.rounding, o.away_minutes, o.keep_out) {
            self.change(store, |tx| {
                tx.state.options.rounding = rounding;
                tx.state.options.away_minutes = away;
                tx.state.options.keep_out = keep_out;
            });
        }
    }
}
