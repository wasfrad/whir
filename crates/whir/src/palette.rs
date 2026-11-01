//! The command palette (Ctrl+Shift+P): everything in the menus, the notes and
//! the pages of Settings, found by a few letters.
//!
//! It keeps no list of commands. It reads the menus, the open document's parts
//! among them, the way they're drawn (see [`kit::read_menu`]), and runs one by
//! reading them again with that entry pressed: so it can't fall behind them, and
//! a document has nothing to do to be in it.

use std::path::PathBuf;

use egui::{
    Align2, Context, Event, FontId, Id, Key, Margin, Modal, Modifiers, ScrollArea, Stroke,
    TextEdit, vec2,
};
use whir_core::icons::{self, Icon};
use whir_core::kit;
use whir_core::library::LibraryEntry;
use whir_core::theme::Theme;

use crate::fuzzy::{self, Match};
use crate::keys::consume_exact;
use crate::settings::Section;

const WIDTH: f32 = 560.0;
const LIST_HEIGHT: f32 = 340.0;
/// Rows drawn: past this, typing more is the way to find it.
const MOST_ROWS: usize = 60;
/// How far it hangs below the tabs.
const DROP: f32 = 44.0;
/// How many picks to keep at the top of the list.
const RECENT: usize = 6;
/// Frames from the last one the palette is drawn in until a widget under it can
/// have the keyboard: egui takes it from anything below a modal, and goes by
/// which modals the frame before drew.
const MODAL_GONE: u8 = 2;

/// What a row does when it's picked.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// The entry with this label, in this menu.
    Menu(&'static str, String),
    Note(PathBuf),
    Page(Section),
}

struct Item {
    icon: Option<Icon>,
    label: String,
    /// `label`, then words it could be found by.
    search: String,
    /// On the right: a shortcut, or what it is.
    side: String,
    pick: Pick,
}

#[derive(Default)]
pub struct Palette {
    open: bool,
    /// The menus are to be read this frame.
    wants_menus: bool,
    /// An entry to press when they next are.
    press: Option<(&'static str, String)>,
    query: String,
    items: Vec<Item>,
    /// Where the arrow keys have got to in what's found.
    selected: usize,
    /// The field still needs the keyboard.
    focus: bool,
    /// What had the keyboard.
    restore: Option<Id>,
    /// It has closed, and what comes next is waiting for that to take.
    closing: Option<Closing>,
    /// Latest first: what shows first until something's typed.
    recent: Vec<Pick>,
}

/// What happens once the palette has really gone.
struct Closing {
    frames: u8,
    /// What gets the keyboard back.
    restore: Option<Id>,
    /// A menu entry to press, with the keyboard back where it was: a command on
    /// a note's selection needs the note to have it.
    press: Option<(&'static str, String)>,
}

impl Palette {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open it over `focused`. `notes` and the pages of Settings go in now, and
    /// the menus' entries once they've been read.
    pub fn open(&mut self, focused: Option<Id>, notes: &[LibraryEntry]) {
        // Opened again before it had quite gone: the same thing gets it back.
        let waiting = self.closing.take().and_then(|c| c.restore);
        self.open = true;
        self.wants_menus = true;
        self.focus = true;
        self.restore = focused.or(waiting);
        self.query.clear();
        self.selected = 0;
        self.items.clear();
        for section in Section::all() {
            let label = section.label().to_owned();
            self.items.push(Item {
                icon: Some(section.icon()),
                search: format!("{label} settings {}", section.keywords()),
                label,
                side: "Settings".to_owned(),
                pick: Pick::Page(section),
            });
        }
        for note in notes {
            self.items.push(Item {
                icon: Some(Icon::Note),
                label: note.title.clone(),
                search: note.title.clone(),
                side: "Note".to_owned(),
                pick: Pick::Note(note.path.clone()),
            });
        }
    }

    /// The menus are to be read this frame, for what's in them or to press one.
    pub fn wants_menus(&self) -> bool {
        self.wants_menus || self.press.is_some()
    }

    /// The entry to press, once.
    pub fn take_press(&mut self) -> Option<(&'static str, String)> {
        self.press.take()
    }

    /// What the menus have in them.
    pub fn menus_read(&mut self, entries: Vec<(&'static str, kit::Entry)>) {
        if !std::mem::take(&mut self.wants_menus) {
            return;
        }
        // Commands before the rest, as the menus have them.
        let items = entries.into_iter().map(|(menu, entry)| Item {
            icon: entry.icon,
            search: format!("{} {menu}", entry.label),
            side: entry.shortcut,
            pick: Pick::Menu(menu, entry.label.clone()),
            label: entry.label,
        });
        self.items.splice(0..0, items);
    }

    /// What the query finds, best first. Nothing typed: recent picks, then the
    /// commands and pages in their order, and not every note there is.
    fn found(&self) -> Vec<(usize, Match)> {
        if !self.query.trim().is_empty() {
            return fuzzy::rank(&self.query, self.items.iter().map(|i| i.search.as_str()));
        }
        let mut shown: Vec<usize> = (0..self.items.len())
            .filter(|&i| !matches!(self.items[i].pick, Pick::Note(_)))
            .collect();
        shown.sort_by_key(|&i| {
            let pick = &self.items[i].pick;
            self.recent.iter().position(|p| p == pick).unwrap_or(RECENT)
        });
        let none = || Match {
            score: 0,
            at: Vec::new(),
        };
        shown.into_iter().map(|i| (i, none())).collect()
    }

    fn close(&mut self, ctx: &Context, press: Option<(&'static str, String)>) {
        self.open = false;
        self.wants_menus = false;
        self.items.clear();
        self.query.clear();
        self.closing = Some(Closing {
            frames: MODAL_GONE,
            restore: self.restore.take(),
            press,
        });
        ctx.request_repaint();
    }

    /// Once a frame, before anything else: see out its closing.
    pub fn tick(&mut self, ctx: &Context) {
        let Some(closing) = &mut self.closing else {
            return;
        };
        closing.frames -= 1;
        ctx.request_repaint();
        if closing.frames > 0 {
            return;
        }
        if let Some(closing) = self.closing.take() {
            if let Some(id) = closing.restore {
                ctx.memory_mut(|m| m.request_focus(id));
            }
            self.press = closing.press;
        }
    }

    /// Draw it. It has the keyboard: the caller keeps everything else from
    /// seeing keys afterwards. What was picked, if anything.
    pub fn show(&mut self, ctx: &Context, theme: &Theme) -> Option<Pick> {
        let shift_ctrl = Modifiers::CTRL | Modifiers::SHIFT;
        let (up, down, enter, esc, again) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                consume_exact(i, shift_ctrl, Key::P),
            )
        });
        let found = self.found();
        let last = found.len().min(MOST_ROWS).saturating_sub(1);
        self.selected = match (up, down) {
            (true, _) if self.selected == 0 => last,
            (true, _) => self.selected - 1,
            (_, true) if self.selected >= last => 0,
            (_, true) => self.selected + 1,
            _ => self.selected.min(last),
        };

        let m = theme.metrics;
        let id = Id::new("whir.palette");
        let width = (ctx.content_rect().width() - 32.0).min(WIDTH);
        let area = Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, DROP));
        let frame = kit::floating(theme).inner_margin(Margin::same(6));
        let font = FontId::proportional(m.body_text_size);
        let mut clicked = None;
        let mut typed = false;
        let modal = Modal::new(id)
            .area(area)
            .backdrop_color(theme.modal_backdrop.gamma_multiply(0.45))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    let color = theme.text_dim;
                    ui.label(icons::text(Icon::Search, 14.0).color(color));
                    let field = TextEdit::singleline(&mut self.query)
                        .hint_text("Find a command, a note or a setting")
                        .frame(egui::Frame::NONE.inner_margin(Margin::symmetric(0, 5)))
                        .desired_width(f32::INFINITY)
                        .return_key(None);
                    let field = ui.add(field);
                    if std::mem::take(&mut self.focus) {
                        field.request_focus();
                    }
                    typed = field.changed();
                });
                let rule = Stroke::new(1.0, theme.separator);
                let x = ui.max_rect().x_range();
                let y = ui.painter().round_to_pixel_center(ui.cursor().top() + 2.0);
                ui.painter().hline(x, y, rule);
                ui.add_space(6.0);
                if found.is_empty() {
                    let color = theme.text_dim;
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("  Nothing by that name").color(color));
                    ui.add_space(8.0);
                    return;
                }
                ScrollArea::vertical()
                    .max_height(LIST_HEIGHT)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (row, (i, hit)) in found.iter().take(MOST_ROWS).enumerate() {
                            let item = &self.items[*i];
                            let shown = item.label.chars().count();
                            let at: Vec<usize> =
                                hit.at.iter().copied().filter(|&a| a < shown).collect();
                            let colors = (theme.text, theme.accent);
                            let words = fuzzy::job(&item.label, &at, &font, colors, width);
                            let on = row == self.selected;
                            let side = &item.side;
                            let response = kit::result_row(ui, item.icon, words, side, on, theme);
                            if on && (up || down) {
                                response.scroll_to_me(None);
                            }
                            if response.clicked() {
                                clicked = Some(item.pick.clone());
                            }
                        }
                    });
            });
        if typed {
            self.selected = 0;
        }
        let selected = found.get(self.selected).map(|(i, _)| *i);
        let entered = selected.filter(|_| enter && !typed);
        let picked = clicked.or_else(|| entered.map(|i| self.items[i].pick.clone()));
        let mut press = None;
        if let Some(pick) = &picked {
            self.recent.retain(|p| p != pick);
            self.recent.insert(0, pick.clone());
            self.recent.truncate(RECENT);
            match pick {
                Pick::Menu(menu, label) => press = Some((*menu, label.clone())),
                // Something else is about to be on screen.
                Pick::Note(_) | Pick::Page(_) => self.restore = None,
            }
        }
        if esc || again || modal.should_close() || picked.is_some() {
            self.close(ctx, press);
        }
        picked
    }
}

/// Is this for whoever has the keyboard?
pub fn is_keyboard(event: &Event) -> bool {
    matches!(
        event,
        Event::Key { .. } | Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use whir_core::library::EntryKind;

    fn note(title: &str) -> LibraryEntry {
        LibraryEntry {
            path: PathBuf::from(format!("{title}.md")),
            kind: EntryKind::Note,
            title: title.to_owned(),
            preview: String::new(),
            modified: std::time::SystemTime::UNIX_EPOCH,
            size: 0,
            cloud_only: false,
            todo: None,
        }
    }

    fn entry(label: &str, shortcut: &str) -> kit::Entry {
        kit::Entry {
            icon: None,
            label: label.to_owned(),
            shortcut: shortcut.to_owned(),
            enabled: true,
        }
    }

    fn opened() -> Palette {
        let mut p = Palette::default();
        p.open(None, &[note("Rack review"), note("Hedge questions")]);
        p.menus_read(vec![
            ("File", entry("New note", "Ctrl+N")),
            ("Edit", entry("Find in all tabs", "Ctrl+Shift+F")),
        ]);
        p
    }

    fn labels(p: &Palette) -> Vec<&str> {
        p.found()
            .iter()
            .map(|(i, _)| p.items[*i].label.as_str())
            .collect()
    }

    #[test]
    fn nothing_typed_shows_the_commands_first_and_no_notes() {
        let p = opened();
        let shown = labels(&p);
        assert_eq!(&shown[..2], ["New note", "Find in all tabs"]);
        assert!(shown.contains(&"Appearance"));
        assert!(!shown.contains(&"Rack review"));
    }

    #[test]
    fn typing_finds_commands_notes_and_settings() {
        let mut p = opened();
        p.query = "rack".to_owned();
        assert_eq!(labels(&p)[0], "Rack review");
        p.query = "new".to_owned();
        assert_eq!(labels(&p)[0], "New note");
        // By a word the row doesn't show.
        p.query = "minimap".to_owned();
        assert_eq!(labels(&p), ["PDFs & images"]);
        p.query = "rounding".to_owned();
        assert_eq!(labels(&p), ["Time tracking"]);
        p.query = "zzzz".to_owned();
        assert!(labels(&p).is_empty());
    }

    #[test]
    fn what_was_picked_last_comes_first() {
        let mut p = opened();
        p.recent = vec![Pick::Page(Section::Appearance)];
        assert_eq!(labels(&p)[0], "Appearance");
        assert_eq!(labels(&p)[1], "New note");
    }

    #[test]
    fn it_waits_for_the_modal_to_go_before_it_lets_a_press_through() {
        let ctx = Context::default();
        let mut p = opened();
        p.close(&ctx, Some(("File", "New note".to_owned())));
        assert!(!p.is_open());
        for _ in 1..MODAL_GONE {
            p.tick(&ctx);
            assert!(!p.wants_menus(), "too soon");
        }
        p.tick(&ctx);
        assert_eq!(p.take_press(), Some(("File", "New note".to_owned())));
        assert!(p.closing.is_none());
        // And it's done.
        p.tick(&ctx);
        assert_eq!(p.take_press(), None);
    }

    #[test]
    fn opened_again_while_closing_it_still_knows_what_had_the_keyboard() {
        let ctx = Context::default();
        let note = Id::new("note");
        let mut p = Palette::default();
        p.open(Some(note), &[]);
        p.close(&ctx, None);
        p.open(None, &[]);
        assert_eq!(p.restore, Some(note));
        assert!(p.closing.is_none());
    }

    #[test]
    fn the_menus_are_read_once_and_a_press_once() {
        let mut p = opened();
        assert!(!p.wants_menus());
        assert_eq!(p.take_press(), None);
        let items = p.items.len();
        p.menus_read(vec![("File", entry("Again", ""))]);
        assert_eq!(p.items.len(), items);

        p.press = Some(("File", "New note".to_owned()));
        assert!(p.wants_menus());
        assert_eq!(p.take_press(), Some(("File", "New note".to_owned())));
        assert!(!p.wants_menus());
    }
}
