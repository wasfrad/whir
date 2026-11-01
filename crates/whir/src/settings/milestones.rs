//! Projects and their milestones, what time is tracked against: a card for each
//! project, and everything on it edited where it stands.

use std::collections::HashSet;

use egui::{Align, Button, FontId, Id, Key, Layout, RichText, TextEdit, Ui, vec2};
use whir_core::icons::{self, Icon};
use whir_core::kit::{self, Card, Inline};
use whir_core::theme::{Metrics, Theme};
use whir_core::timesheet::{Milestone, Project, Store, Txn};
use whir_core::{DocCx, toolbar};

use super::no_folder;

/// A change to make to the timesheet, decided while drawing.
type Change = Box<dyn FnOnce(&mut Txn)>;

/// Between the columns of a row.
const GAP: f32 = 8.0;
/// Between cards.
const CARD_GAP: f32 = 12.0;
/// What "+ Milestone" takes up on a project's row.
const ADD_WIDTH: f32 = 84.0;

#[derive(Default)]
pub struct View {
    /// A new project, while it's being named.
    new_project: Option<Naming>,
    /// The project a milestone is being added to, and the milestone.
    new_milestone: Option<(u32, Naming)>,
    /// Projects whose archived milestones are showing.
    archived: HashSet<u32>,
    /// Milestones with time in any week (those can be archived, not deleted).
    used: Option<HashSet<u32>>,
    error: Option<String>,
}

/// Something new, while it's being named.
struct Naming {
    name: String,
    code: String,
    /// The name field still needs the keyboard.
    focus: bool,
}

impl Naming {
    fn new() -> Naming {
        Naming {
            name: String::new(),
            code: String::new(),
            focus: true,
        }
    }
}

/// How naming something went this frame.
enum Named {
    Waiting,
    Done,
    Cancelled,
}

/// How wide a milestone's name and code are, in a row `width` wide with its ⋯.
fn columns(width: f32, m: Metrics) -> (f32, f32) {
    let code = (width * 0.3).clamp(90.0, 170.0);
    (width - code - m.control_height - 2.0 * GAP, code)
}

impl View {
    /// The timesheet changed on disk: what was read from it goes.
    pub fn forget(&mut self) {
        self.used = None;
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        store: Option<&mut Store>,
        _cx: &mut DocCx<'_>,
        theme: &Theme,
    ) {
        let Some(store) = store else {
            no_folder(ui, "Projects & milestones", theme);
            return;
        };
        kit::page_header(
            ui,
            "Projects & milestones",
            "Each milestone can be timed from the capture dock, or have its hours typed into \
             the timesheet. A code is the milestone or assignment ID that goes on the timesheet.",
            theme,
            |ui| {
                if kit::primary(ui, "New project", theme).clicked() {
                    self.new_project = Some(Naming::new());
                }
            },
        );
        if let Some(e) = &self.error {
            kit::banner(ui, theme.danger, theme, |ui| {
                ui.label(RichText::new(e).color(theme.text));
            });
            ui.add_space(CARD_GAP);
        }

        let used = self.used.get_or_insert_with(|| {
            let mut used: HashSet<u32> =
                store.state().running.iter().map(|r| r.milestone).collect();
            for m in store.mondays() {
                used.extend(store.week(m).entries.iter().map(|e| e.milestone));
            }
            used
        });
        let used = used.clone();
        let state = store.state().clone();
        let mut change: Option<Change> = None;

        if self.new_project.is_some() {
            kit::card(ui, theme, |c| self.naming_project(c, &mut change));
            ui.add_space(CARD_GAP);
        } else if state.projects.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                kit::empty_state(
                    ui,
                    Icon::People,
                    "No projects yet",
                    "Add one, then give it the milestones you'd like to time.",
                    theme,
                );
            });
        }
        for (project, milestones) in state.by_project(true, true) {
            kit::card(ui, theme, |c| {
                self.project(c, project, &milestones, &used, &mut change);
            });
            ui.add_space(CARD_GAP);
        }
        if let Some(change) = change {
            match store.update(change) {
                Ok(()) => self.error = None,
                Err(e) => self.error = Some(e),
            }
        }
    }

    fn naming_project(&mut self, c: &mut Card<'_>, change: &mut Option<Change>) {
        let theme = c.theme;
        let Some(n) = &mut self.new_project else {
            return;
        };
        let named = c.field_row(|ui| name_fields(ui, n, "New project", false, theme));
        match named {
            Named::Waiting => {}
            Named::Cancelled => self.new_project = None,
            Named::Done => {
                let name = std::mem::take(&mut n.name);
                self.new_project = None;
                *change = Some(Box::new(move |tx| {
                    tx.state.add_project(&name);
                }));
            }
        }
    }

    /// A project's card: its name, its milestones, then the archived ones if
    /// they've been asked for.
    fn project(
        &mut self,
        c: &mut Card<'_>,
        project: &Project,
        milestones: &[&Milestone],
        used: &HashSet<u32>,
        change: &mut Option<Change>,
    ) {
        let theme = c.theme;
        let m = theme.metrics;
        let cid = project.id;
        c.field_row(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            ui.horizontal(|ui| {
                let width = ui.available_width() - m.control_height - ADD_WIDTH - 2.0 * GAP;
                let field = Inline {
                    id: Id::new(("whir.project", cid)),
                    current: &project.name,
                    hint: "Project",
                    width,
                    font: kit::strong(m.body_text_size),
                    color: theme.text,
                };
                if let Some(name) = kit::inline_text(ui, field, theme)
                    && !name.is_empty()
                {
                    *change = Some(Box::new(move |tx| {
                        if let Some(c) = tx.state.project_mut(cid) {
                            c.name = name;
                        }
                    }));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    kit::more_button(ui, theme, |ui| {
                        let delete = kit::menu_entry(Some(Icon::Delete), "Delete project", "");
                        let delete = ui
                            .add_enabled(milestones.is_empty(), delete)
                            .on_disabled_hover_text("It still has milestones");
                        if delete.clicked() {
                            *change = Some(Box::new(move |tx| {
                                tx.state.projects.retain(|c| c.id != cid)
                            }));
                            ui.close();
                        }
                    });
                    if kit::icon_action(ui, Icon::Plus, "Milestone", theme).clicked() {
                        self.new_milestone = Some((cid, Naming::new()));
                    }
                });
            });
        });

        let (old, current): (Vec<&Milestone>, Vec<&Milestone>) =
            milestones.iter().copied().partition(|t| t.archived);
        for t in &current {
            milestone(c, t, used.contains(&t.id), change);
        }
        if let Some((_, n)) = self.new_milestone.as_mut().filter(|(on, _)| *on == cid) {
            let named = c.field_row(|ui| name_fields(ui, n, "New milestone", true, theme));
            match named {
                Named::Waiting => {}
                Named::Cancelled => self.new_milestone = None,
                Named::Done => {
                    let (name, code) = (std::mem::take(&mut n.name), std::mem::take(&mut n.code));
                    self.new_milestone = None;
                    *change = Some(Box::new(move |tx| {
                        tx.state.add_milestone(cid, &name, &code);
                    }));
                }
            }
        } else if current.is_empty() && old.is_empty() {
            c.row(|ui| {
                ui.label(RichText::new("No milestones yet").color(theme.text_dim));
            });
        }
        if old.is_empty() {
            return;
        }
        let open = self.archived.contains(&cid);
        let chevron = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let toggled = c.row(|ui| {
            let words = format!("Archived ({})", old.len());
            let words = RichText::new(words).color(theme.text_dim);
            let icon = icons::text(chevron, 10.0).color(theme.text_dim);
            ui.add(Button::new((icon, words)).frame(false)).clicked()
        });
        if toggled && !self.archived.remove(&cid) {
            self.archived.insert(cid);
        }
        if open {
            for t in old {
                milestone(c, t, used.contains(&t.id), change);
            }
        }
    }
}

/// A milestone's row: its name and code, and what else can be done with it.
fn milestone(c: &mut Card<'_>, t: &Milestone, used: bool, change: &mut Option<Change>) {
    let theme = c.theme;
    let m = theme.metrics;
    let tid = t.id;
    let archived = t.archived;
    c.field_row(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        ui.horizontal(|ui| {
            let (name_width, code_width) = columns(ui.available_width(), m);
            let font = FontId::proportional(m.body_text_size);
            let color = if archived { theme.text_dim } else { theme.text };
            let name = Inline {
                id: Id::new(("whir.milestone", tid, "name")),
                current: &t.name,
                hint: "Milestone",
                width: name_width,
                font: font.clone(),
                color,
            };
            if let Some(name) = kit::inline_text(ui, name, theme)
                && !name.is_empty()
            {
                *change = Some(Box::new(move |tx| {
                    if let Some(t) = tx.state.milestone_mut(tid) {
                        t.name = name;
                    }
                }));
            }
            let code = Inline {
                id: Id::new(("whir.milestone", tid, "code")),
                current: &t.code,
                hint: "Add code",
                width: code_width,
                font,
                color: theme.text_dim,
            };
            if let Some(code) = kit::inline_text(ui, code, theme) {
                *change = Some(Box::new(move |tx| {
                    if let Some(t) = tx.state.milestone_mut(tid) {
                        t.code = code;
                    }
                }));
            }
            kit::more_button(ui, theme, |ui| {
                let (label, why) = if archived {
                    ("Restore", "Offer it in the dock again")
                } else {
                    ("Archive", "No longer offered in the dock; its time stays")
                };
                if ui
                    .add(kit::menu_entry(None, label, ""))
                    .on_hover_text(why)
                    .clicked()
                {
                    *change = Some(Box::new(move |tx| {
                        if let Some(t) = tx.state.milestone_mut(tid) {
                            t.archived = !archived;
                        }
                    }));
                    ui.close();
                }
                let delete = kit::menu_entry(Some(Icon::Delete), "Delete", "");
                let delete = ui
                    .add_enabled(!used, delete)
                    .on_disabled_hover_text("It has time on it: archive it instead");
                if delete.clicked() {
                    *change = Some(Box::new(move |tx| {
                        tx.state.milestones.retain(|t| t.id != tid);
                        if tx.state.last == Some(tid) {
                            tx.state.last = None;
                        }
                    }));
                    ui.close();
                }
            });
        });
    });
}

/// The fields something new is named in, in the same columns as the rows
/// around them. Enter or the tick adds it; Esc, or clicking away from it empty,
/// doesn't.
fn name_fields(ui: &mut Ui, n: &mut Naming, hint: &str, with_code: bool, theme: &Theme) -> Named {
    let m = theme.metrics;
    ui.spacing_mut().item_spacing.x = GAP;
    ui.horizontal(|ui| {
        // The tick is a tool, and takes up more than a row's ⋯.
        let room = ui.available_width() - (m.tool_size - m.control_height);
        let (name_width, code_width) = columns(room, m);
        let name_width = if with_code {
            name_width
        } else {
            name_width + code_width + GAP
        };
        let height = m.control_height;
        let name = ui.add_sized(
            vec2(name_width, height),
            TextEdit::singleline(&mut n.name).hint_text(hint),
        );
        if std::mem::take(&mut n.focus) {
            name.request_focus();
        }
        let code = with_code.then(|| {
            ui.add_sized(
                vec2(code_width, height),
                TextEdit::singleline(&mut n.code).hint_text("Code (optional)"),
            )
        });
        let ready = !n.name.trim().is_empty();
        let tick = toolbar::tool(ui, Icon::Check, "Add it (Enter)", ready, theme);
        let left = name.lost_focus() || code.as_ref().is_some_and(|c| c.lost_focus());
        let (enter, esc, pressed) = ui.input(|i| {
            (
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Escape),
                i.pointer.any_pressed(),
            )
        });
        if tick || (left && enter && ready) {
            Named::Done
        } else if left && (esc || (pressed && !ready)) {
            Named::Cancelled
        } else {
            Named::Waiting
        }
    })
    .inner
}
