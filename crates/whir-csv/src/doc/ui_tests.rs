//! Headless egui tests: real frames through `Document::ui`, real key and
//! pointer events, checking the editor's key handling and focus behaviour.

use std::sync::Arc;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, pos2, vec2};
use whir_core::{DocCx, DocRequest, Document, TextureRegistry, Waker};

use super::*;
use crate::edit::RowKey;
use crate::grid::{CellPos, Selection};
use crate::testutil::{open_cx, test_dir, wait_for};

pub(crate) struct Harness {
    pub ctx: egui::Context,
    pub doc: CsvDoc,
    pub requests: Vec<DocRequest>,
    /// Viewport commands of the last frame (e.g. RequestPaste).
    pub commands: Vec<egui::ViewportCommand>,
    time: f64,
    textures: Arc<dyn TextureRegistry>,
    _dir: PathBuf,
}

impl Harness {
    pub fn open(name: &str, data: &[u8]) -> Self {
        let dir = test_dir(name);
        let path = dir.join("t.csv");
        std::fs::write(&path, data).unwrap();
        let cx = open_cx(&dir);
        let mut doc = CsvDoc::open(path, &cx);
        wait_for(|| {
            doc.poll();
            doc.loaded().is_some_and(|l| l.table.progress().done)
        });
        let mut h = Self {
            ctx: egui::Context::default(),
            doc,
            requests: Vec::new(),
            commands: Vec::new(),
            time: 0.0,
            textures: cx.textures.clone(),
            _dir: dir,
        };
        h.frame(vec![]);
        h.frame(vec![]);
        h
    }

    pub fn blank(name: &str) -> Self {
        let dir = test_dir(name);
        let cx = open_cx(&dir);
        let mut h = Self {
            ctx: egui::Context::default(),
            doc: new_blank(&cx),
            requests: Vec::new(),
            commands: Vec::new(),
            time: 0.0,
            textures: cx.textures.clone(),
            _dir: dir,
        };
        h.frame(vec![]);
        h
    }

    pub fn frame(&mut self, events: Vec<Event>) {
        self.time += 0.05;
        let raw = RawInput {
            events,
            time: Some(self.time),
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0))),
            ..Default::default()
        };
        let Self {
            ctx,
            doc,
            requests,
            textures,
            commands,
            ..
        } = self;
        let waker = Waker::noop();
        let out = ctx.run_ui(raw, |ui| {
            let mut cx = DocCx {
                textures,
                waker: &waker,
                just_activated: false,
                requests,
            };
            doc.ui(ui, &mut cx);
        });
        *commands = out
            .viewport_output
            .values()
            .flat_map(|v| v.commands.clone())
            .collect();
        out.drop_without_applying_deltas();
    }

    pub fn l(&mut self) -> &mut Loaded {
        self.doc.loaded().unwrap()
    }

    pub fn select(&mut self, row: u64, col: usize) {
        self.l().grid.sel = Some(Selection::single(CellPos { row, col }));
    }

    pub fn cursor(&mut self) -> (u64, usize) {
        let c = self.l().grid.sel.unwrap().cursor;
        (c.row, c.col)
    }

    /// The shown value of display row `r`, column `c`.
    pub fn cell(&mut self, r: u64, c: usize) -> String {
        let l = self.l();
        let k = l.view().key(r).unwrap();
        let ck = l.edits.col_key(c);
        l.value(k, ck)
    }

    pub fn editing(&mut self) -> bool {
        self.l().editor.is_some()
    }

    pub fn editor_text(&mut self) -> String {
        self.l().editor.as_ref().unwrap().text.clone()
    }

    pub fn type_text(&mut self, s: &str) {
        self.frame(vec![Event::Text(s.into())]);
    }

    pub fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.frame(vec![
            Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
            Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers,
            },
        ]);
    }

    pub fn click(&mut self, p: Pos2) {
        self.frame(vec![Event::PointerMoved(p)]);
        self.frame(vec![Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }]);
        self.frame(vec![Event::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
    }

    pub fn cell_center(&mut self, r: u64, c: usize) -> Pos2 {
        self.l().grid.cell_rect(r, c).center()
    }

    /// Apply a grid action (as a menu would) in a frame of this context.
    pub fn act(&mut self, action: crate::grid::Action) {
        self.time += 0.05;
        let raw = RawInput {
            time: Some(self.time),
            ..Default::default()
        };
        let ctx = self.ctx.clone();
        let waker = Waker::noop();
        let out = ctx.run_ui(raw, |ui| {
            self.doc.loaded().unwrap().apply(action.clone(), ui, &waker)
        });
        out.drop_without_applying_deltas();
        self.frame(vec![]);
    }

    pub fn focused(&self) -> Option<egui::Id> {
        self.ctx.memory(|m| m.focused())
    }
}

const DATA: &[u8] = b"name,city,n\r\nAnn,Oslo,1\r\nBob,Rome,2\r\nCy,Lima,3\r\n";
const NONE: Modifiers = Modifiers::NONE;

#[test]
fn typing_then_enter_commits_and_moves_down() {
    let mut h = Harness::open("ui-type", DATA);
    h.select(0, 0);
    h.type_text("a");
    assert!(h.editing());
    assert_eq!(h.editor_text(), "a", "the typed text seeds the editor once");
    h.type_text("b");
    assert_eq!(h.editor_text(), "ab");
    h.key(Key::Enter, NONE);
    assert!(!h.editing());
    assert_eq!(h.cell(0, 0), "ab");
    assert_eq!(h.cursor(), (1, 0));
    assert!(h.doc.is_dirty());
    // Undo restores the file's value: clean again.
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.cell(0, 0), "Ann");
    assert!(!h.doc.is_dirty());
    assert_eq!(h.cursor(), (0, 0), "undo selects what it changed");
    h.key(Key::Y, Modifiers::CTRL);
    assert_eq!(h.cell(0, 0), "ab");
}

#[test]
fn escape_in_the_second_frame_cancels() {
    let mut h = Harness::open("ui-escape", DATA);
    h.select(1, 1);
    h.type_text("x");
    assert!(h.editing());
    h.key(Key::Escape, NONE);
    assert!(!h.editing());
    assert_eq!(h.cell(1, 1), "Rome");
    assert!(!h.doc.is_dirty());
    assert_eq!(h.cursor(), (1, 1));
    assert_eq!(h.focused(), None);
}

#[test]
fn alt_enter_is_a_line_break() {
    let mut h = Harness::open("ui-alt-enter", DATA);
    h.select(0, 1);
    h.type_text("a");
    h.key(Key::Enter, Modifiers::ALT);
    assert!(h.editing(), "Alt+Enter doesn't commit");
    h.type_text("b");
    assert_eq!(h.editor_text(), "a\nb");
    h.key(Key::Enter, NONE);
    assert_eq!(h.cell(0, 1), "a\nb");
}

#[test]
fn tab_moves_right_and_no_button_gets_focus() {
    let mut h = Harness::open("ui-tab", DATA);
    // A banner with buttons is on screen.
    h.doc.failure = Some(SaveFailure {
        message: "test".into(),
        changed_on_disk: true,
    });
    h.frame(vec![]);
    h.select(0, 0);
    h.type_text("x");
    h.key(Key::Tab, NONE);
    assert!(!h.editing());
    assert_eq!(h.cell(0, 0), "x");
    assert_eq!(h.cursor(), (0, 1));
    h.frame(vec![]);
    assert_eq!(h.focused(), None);
    // Not editing: Tab still just moves.
    h.key(Key::Tab, NONE);
    h.frame(vec![]);
    assert_eq!(h.cursor(), (0, 2));
    assert_eq!(h.focused(), None);
    // Enter goes back to the column the Tabs started from.
    h.type_text("y");
    h.key(Key::Enter, NONE);
    assert_eq!(h.cursor(), (1, 0));
    // Space on a focused button would "click" it: nothing is focused, so the
    // banner stays and the space starts an edit instead.
    h.frame(vec![Event::Text(" ".into())]);
    assert!(h.editing());
    assert!(h.doc.failure.is_some());
}

#[test]
fn f2_and_backspace_modes() {
    let mut h = Harness::open("ui-modes", DATA);
    h.select(0, 0);
    h.key(Key::F2, NONE);
    assert!(h.editing());
    assert_eq!(h.editor_text(), "Ann");
    // Edit mode: arrows move the caret instead of committing.
    h.key(Key::ArrowLeft, NONE);
    assert!(h.editing());
    h.type_text("X");
    assert_eq!(h.editor_text(), "AnXn");
    h.key(Key::Enter, NONE);
    assert_eq!(h.cell(0, 0), "AnXn");

    // Backspace: start empty in Enter mode, where arrows commit and move.
    h.select(1, 0);
    h.key(Key::Backspace, NONE);
    assert!(h.editing());
    assert_eq!(h.editor_text(), "");
    h.type_text("z");
    h.key(Key::ArrowDown, NONE);
    assert!(!h.editing());
    assert_eq!(h.cell(1, 0), "z");
    assert_eq!(h.cursor(), (2, 0));
}

#[test]
fn clicking_elsewhere_commits() {
    let mut h = Harness::open("ui-click", DATA);
    h.select(0, 0);
    h.type_text("q");
    let p = h.cell_center(2, 1);
    h.click(p);
    assert!(!h.editing());
    assert_eq!(h.cell(0, 0), "q");
    assert_eq!(h.cursor(), (2, 1));
}

#[test]
fn double_click_edits_and_ghost_row_adds() {
    let mut h = Harness::open("ui-ghost", DATA);
    // Type into the "+" row: a new row.
    let rows = h.l().rows();
    h.select(rows, 1);
    h.type_text("New");
    h.key(Key::Enter, NONE);
    assert_eq!(h.l().rows(), rows + 1);
    assert_eq!(h.cell(rows, 1), "New");
    assert_eq!(h.cursor(), (rows + 1, 1), "on the new \"+\" row");
    let k = h.l().view().key(rows).unwrap();
    assert!(k.is_added());
    // Undo removes the row again.
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.l().rows(), rows);
    assert!(!h.doc.is_dirty());
    let ck = h.l().edits.col_key(1);
    assert!(h.l().edits.cells().get(k, ck).is_none());
    let _ = RowKey::HEADER;
}

#[test]
fn ctrl_enter_fills_the_selection() {
    let mut h = Harness::open("ui-fill", DATA);
    h.l().grid.sel = Some(Selection {
        anchor: CellPos { row: 0, col: 2 },
        cursor: CellPos { row: 2, col: 2 },
    });
    h.type_text("9");
    h.key(Key::Enter, Modifiers::CTRL);
    assert!(!h.editing());
    for r in 0..3 {
        assert_eq!(h.cell(r, 2), "9");
    }
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.cell(1, 2), "2");
    let _ = pos2(0.0, 0.0);
}

fn paste(h: &mut Harness, text: &str) {
    h.frame(vec![Event::Paste(text.into())]);
}

fn range(h: &mut Harness, r0: u64, c0: usize, r1: u64, c1: usize) {
    h.l().grid.sel = Some(Selection {
        anchor: CellPos { row: r0, col: c0 },
        cursor: CellPos { row: r1, col: c1 },
    });
}

#[test]
fn paste_blocks_grow_the_table() {
    let mut h = Harness::open("ui-paste", DATA);
    h.select(0, 1);
    paste(&mut h, "x\ty\n\"two\nlines\"\tz\n");
    assert_eq!(h.cell(0, 1), "x");
    assert_eq!(h.cell(0, 2), "y");
    assert_eq!(h.cell(1, 1), "two\nlines");
    assert_eq!(h.cursor(), (1, 2));
    // Past the last row and column: rows and a "Column 4" are added.
    let rows = h.l().rows();
    h.select(rows - 1, 2);
    paste(&mut h, "a\tb\nc\td\n");
    assert_eq!(h.l().rows(), rows + 1);
    assert_eq!(h.l().grid.cols(), 4);
    assert_eq!(h.cell(rows, 3), "d");
    let head = h.l().view().header().cell(3).text.to_owned();
    assert_eq!(head, "Column 4");
    // One undo step removes both again.
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.l().rows(), rows);
    assert_eq!(h.l().grid.cols(), 3);
    h.key(Key::Z, Modifiers::CTRL);
    assert!(!h.doc.is_dirty());
}

#[test]
fn paste_one_value_fills_and_commas_stay() {
    let mut h = Harness::open("ui-paste-fill", DATA);
    range(&mut h, 0, 2, 2, 2);
    paste(&mut h, "1,234\n");
    for r in 0..3 {
        assert_eq!(h.cell(r, 2), "1,234");
    }
    // Pasting the file's own values back leaves nothing to save.
    range(&mut h, 0, 2, 0, 2);
    paste(&mut h, "1\n2\n3\n");
    assert!(!h.doc.is_dirty());
}

#[test]
fn cut_clear_and_fill() {
    let mut h = Harness::open("ui-cut", DATA);
    range(&mut h, 0, 0, 1, 0);
    h.frame(vec![Event::Cut]);
    assert_eq!(h.cell(0, 0), "");
    assert_eq!(h.cell(1, 0), "");
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.cell(0, 0), "Ann");
    // Delete clears; clearing an empty cell changes nothing.
    h.select(2, 1);
    h.key(Key::Delete, NONE);
    assert_eq!(h.cell(2, 1), "");
    h.key(Key::Delete, NONE);
    h.key(Key::Z, Modifiers::CTRL);
    assert!(!h.doc.is_dirty(), "one step: the second Delete did nothing");
    // Ctrl+D: the top row down; one row: from the row above.
    range(&mut h, 0, 1, 2, 2);
    h.key(Key::D, Modifiers::CTRL);
    assert_eq!(h.cell(2, 1), "Oslo");
    assert_eq!(h.cell(1, 2), "1");
    h.select(1, 0);
    h.key(Key::D, Modifiers::CTRL);
    assert_eq!(h.cell(1, 0), "Ann");
    // Ctrl+R: the left column right.
    range(&mut h, 2, 0, 2, 2);
    h.key(Key::R, Modifiers::CTRL);
    assert_eq!(h.cell(2, 2), "Cy");
}

#[test]
fn menu_paste_asks_the_shell() {
    let mut h = Harness::open("ui-menu-paste", DATA);
    h.select(0, 0);
    let ctx = h.ctx.clone();
    let out = ctx.run_ui(RawInput::default(), |ui| {
        h.l()
            .apply(crate::grid::Action::RequestPaste, ui, &Waker::noop());
    });
    let asked = out.viewport_output.values().any(|v| {
        v.commands
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::RequestPaste))
    });
    out.drop_without_applying_deltas();
    assert!(asked);
    // The shell's answer arrives as a paste event.
    paste(&mut h, "p");
    assert_eq!(h.cell(0, 0), "p");
}

#[test]
fn find_results_follow_edits() {
    let mut h = Harness::open("ui-find", DATA);
    let waker = Waker::noop();
    h.l().start_find("zz".into(), &waker);
    wait_for(|| {
        h.doc.poll();
        !h.l().find.running
    });
    assert!(h.l().find.display.is_empty());
    h.select(1, 1);
    h.type_text("xzzx");
    h.key(Key::Enter, NONE);
    assert_eq!(h.l().find.display, [(1, 1)]);
    h.select(1, 1);
    h.type_text("no");
    h.key(Key::Enter, NONE);
    assert!(h.l().find.display.is_empty());
}

fn column(h: &mut Harness, c: usize) -> Vec<String> {
    let rows = h.l().rows();
    (0..rows).map(|r| h.cell(r, c)).collect()
}

fn header(h: &mut Harness) -> Vec<String> {
    let cols = h.l().grid.cols();
    let mut v = h.l().view();
    let head = v.header();
    (0..cols).map(|c| head.cell(c).text.to_owned()).collect()
}

fn sort_by(h: &mut Harness, col: usize, dir: crate::sort::Direction) {
    let waker = Waker::noop();
    h.l().sort(col, Some(dir), &waker, 0.0);
    wait_for(|| {
        h.doc.poll();
        h.l().sorting.is_none()
    });
    h.frame(vec![]);
}

const SHIFT_CTRL: Modifiers = Modifiers {
    ctrl: true,
    shift: true,
    ..Modifiers::NONE
};

#[test]
fn insert_and_delete_rows() {
    let mut h = Harness::open("ui-rows", DATA);
    range(&mut h, 1, 0, 2, 0);
    h.key(Key::Equals, SHIFT_CTRL); // two rows above row 1
    assert_eq!(column(&mut h, 0), ["Ann", "", "", "Bob", "Cy"]);
    assert_eq!(h.l().grid.sel.unwrap().rows(), (1, 2));
    h.key(Key::Minus, Modifiers::CTRL); // and away again
    assert_eq!(column(&mut h, 0), ["Ann", "Bob", "Cy"]);
    assert!(!h.doc.is_dirty(), "insert then delete of the same rows");
    h.select(0, 1);
    h.key(Key::Minus, Modifiers::CTRL);
    assert_eq!(column(&mut h, 0), ["Bob", "Cy"]);
    assert!(h.doc.is_dirty());
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(column(&mut h, 0), ["Ann", "Bob", "Cy"]);
    // Rows below; the gutter numbers follow the file order.
    h.select(0, 0);
    h.act(crate::grid::Action::InsertRows { above: false });
    assert_eq!(column(&mut h, 0), ["Ann", "", "Bob", "Cy"]);
}

#[test]
fn rows_in_a_sorted_view() {
    use crate::sort::Direction;
    let mut h = Harness::open("ui-sorted-rows", DATA);
    sort_by(&mut h, 0, Direction::Descending); // Cy, Bob, Ann
    assert_eq!(column(&mut h, 0), ["Cy", "Bob", "Ann"]);
    // Insert above display row 1 (Bob): shown there, placed before Bob in the file.
    h.select(1, 0);
    h.act(crate::grid::Action::InsertRows { above: true });
    assert_eq!(column(&mut h, 0), ["Cy", "", "Bob", "Ann"]);
    h.select(1, 0);
    h.type_text("Dee");
    h.key(Key::Enter, NONE);
    assert_eq!(
        column(&mut h, 0),
        ["Cy", "Dee", "Bob", "Ann"],
        "edits don't re-sort"
    );
    assert!(h.l().order.as_ref().unwrap().stale);
    // Delete Bob, then undo: back at the same place.
    h.select(2, 0);
    h.key(Key::Minus, Modifiers::CTRL);
    assert_eq!(column(&mut h, 0), ["Cy", "Dee", "Ann"]);
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(column(&mut h, 0), ["Cy", "Dee", "Bob", "Ann"]);
    // Save: the file gets the new row before Bob; the view keeps its order.
    h.doc.save();
    wait_for(|| {
        h.doc.poll();
        !h.doc.is_saving()
    });
    assert_eq!(h.doc.save_error(), None);
    h.frame(vec![]);
    let path = h.doc.path().unwrap().to_path_buf();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"name,city,n\r\nAnn,Oslo,1\r\nDee,,\r\nBob,Rome,2\r\nCy,Lima,3\r\n"
    );
    assert!(!h.doc.is_dirty());
    assert_eq!(column(&mut h, 0), ["Cy", "Dee", "Bob", "Ann"]);
    // Original order is the new file's order.
    h.act(crate::grid::Action::OriginalOrder);
    assert_eq!(column(&mut h, 0), ["Ann", "Dee", "Bob", "Cy"]);
}

#[test]
fn columns_insert_delete_rename() {
    use crate::sort::Direction;
    let mut h = Harness::open("ui-cols", DATA);
    h.select(0, 1);
    h.act(crate::grid::Action::InsertCols { left: true });
    assert_eq!(header(&mut h), ["name", "Column 2", "city", "n"]);
    assert_eq!(h.cell(0, 2), "Oslo");
    // Rename it from the header menu.
    h.act(crate::grid::Action::Rename(1));
    assert!(h.editing());
    h.type_text("zip"); // the old name is selected: typing replaces it
    h.key(Key::Enter, NONE);
    assert_eq!(header(&mut h), ["name", "zip", "city", "n"]);
    // Sort by city, then delete that column: the order stays, as a custom one.
    sort_by(&mut h, 2, Direction::Ascending);
    assert_eq!(column(&mut h, 0), ["Cy", "Ann", "Bob"]);
    h.select(0, 2);
    h.act(crate::grid::Action::DeleteCols);
    assert_eq!(header(&mut h), ["name", "zip", "n"]);
    assert_eq!(column(&mut h, 0), ["Cy", "Ann", "Bob"]);
    assert!(h.l().order.as_ref().unwrap().spec.is_none());
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(header(&mut h), ["name", "zip", "city", "n"]);
    // Deleting every column is refused.
    range(&mut h, 0, 0, 0, 3);
    h.act(crate::grid::Action::DeleteCols);
    assert_eq!(h.l().grid.cols(), 4);
}

#[test]
fn append_column_and_type_into_the_plus_column() {
    let mut h = Harness::open("ui-append", DATA);
    h.act(crate::grid::Action::AppendColumn);
    assert!(h.editing(), "renaming the new column right away");
    assert_eq!(h.editor_text(), "Column 4");
    h.type_text("Notes");
    h.key(Key::Enter, NONE);
    assert_eq!(header(&mut h), ["name", "city", "n", "Notes"]);
    // Typing into the "+" column adds another.
    let cols = h.l().grid.cols();
    h.select(0, cols);
    h.type_text("x");
    h.key(Key::Enter, NONE);
    assert_eq!(header(&mut h), ["name", "city", "n", "Notes", "Column 5"]);
    assert_eq!(h.cell(0, 4), "x");
    // Three steps (add, rename, add by typing): undo them all, back to the file.
    for _ in 0..3 {
        h.key(Key::Z, Modifiers::CTRL);
    }
    assert!(!h.doc.is_dirty());
    assert_eq!(header(&mut h), ["name", "city", "n"]);
}

#[test]
fn nothing_changes_while_saving() {
    let mut h = Harness::open("ui-gates", DATA);
    let (_tx, rx) = std::sync::mpsc::channel();
    h.l().saving = Some(crate::loaded::SaveState {
        rx,
        cancel: Default::default(),
        progress: Default::default(),
        total: 1,
        kind: SaveKind::Own,
    });
    h.select(0, 0);
    h.type_text("x");
    assert!(!h.editing());
    h.key(Key::Minus, Modifiers::CTRL);
    assert_eq!(h.l().rows(), 3);
    assert!(h.l().status.as_ref().unwrap().0.starts_with("Saving"));
    // Navigation still works.
    h.key(Key::ArrowDown, NONE);
    assert_eq!(h.cursor(), (1, 0));
    h.l().saving = None;
}

#[test]
fn right_click_selects_like_excel() {
    let mut h = Harness::open("ui-context", DATA);
    range(&mut h, 0, 0, 2, 1);
    // Inside the selection: kept.
    let p = h.cell_center(1, 1);
    h.frame(vec![Event::PointerMoved(p)]);
    let press = |pressed| Event::PointerButton {
        pos: p,
        button: PointerButton::Secondary,
        pressed,
        modifiers: NONE,
    };
    h.frame(vec![press(true)]);
    h.frame(vec![press(false)]);
    assert_eq!(h.l().grid.sel.unwrap().rows(), (0, 2));
    assert!(egui::Popup::is_any_open(&h.ctx), "the menu opened");
    h.key(Key::Escape, NONE);
    // Outside it: the clicked cell is selected.
    let p = h.cell_center(2, 2);
    h.frame(vec![Event::PointerMoved(p)]);
    let press = |pressed| Event::PointerButton {
        pos: p,
        button: PointerButton::Secondary,
        pressed,
        modifiers: NONE,
    };
    h.frame(vec![press(true)]);
    h.frame(vec![press(false)]);
    assert_eq!(h.cursor(), (2, 2));
    assert_eq!(h.l().grid.sel.unwrap().cell_count(), 1);
}

#[test]
fn a_new_table_saves_into_the_library() {
    let mut h = Harness::blank("ui-blank");
    assert_eq!(h.doc.title(), "Untitled table");
    assert!(h.doc.path().is_none());
    assert!(h.doc.is_pristine());
    assert_eq!(header(&mut h), ["Column 1", "Column 2", "Column 3"]);
    // Nothing typed: saving writes nothing.
    assert!(matches!(h.doc.save(), SaveResult::Saved));
    assert!(std::fs::read_dir(&h._dir).unwrap().next().is_none());
    // Type straight away: the cursor is on the "+" row.
    h.type_text("hello");
    h.key(Key::Tab, NONE);
    h.type_text("world");
    h.key(Key::Enter, NONE);
    assert!(!h.doc.is_pristine());
    assert!(h.doc.is_dirty());
    assert_eq!(h.l().rows(), 1);
    assert_eq!(
        h.cursor(),
        (1, 0),
        "Enter: the next \"+\" row, first column"
    );
    assert!(matches!(h.doc.save(), SaveResult::Started));
    wait_for(|| {
        h.doc.poll();
        !h.doc.is_saving()
    });
    assert_eq!(h.doc.save_error(), None);
    let path = h.doc.path().unwrap().to_path_buf();
    assert_eq!(path.parent().unwrap(), h._dir.as_path());
    let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
    assert!(whir_core::library::is_untitled_stem(&stem), "{stem}");
    assert_eq!(h.doc.title(), path.file_name().unwrap().to_string_lossy());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        "\u{feff}Column 1,Column 2,Column 3\r\nhello,world,\r\n".as_bytes()
    );
    assert!(!h.doc.is_dirty());
    assert!(!h.doc.is_pristine());
    // Now it is an ordinary file in the library: Save As exports a copy.
    let copy = h._dir.join("sub");
    std::fs::create_dir_all(&copy).unwrap();
    let copy = copy.join("copy.tsv");
    h.doc.save_as(copy.clone());
    wait_for(|| {
        h.doc.poll();
        !h.doc.is_saving()
    });
    assert_eq!(h.doc.path(), Some(path.as_path()));
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        "\u{feff}Column 1\tColumn 2\tColumn 3\r\nhello\tworld\t\r\n".as_bytes()
    );
}

#[test]
fn an_empty_file_grows_a_header() {
    let mut h = Harness::open("ui-empty", b"");
    assert_eq!(h.l().grid.cols(), 0);
    h.act(crate::grid::Action::AppendColumn);
    h.type_text("Name");
    h.key(Key::Enter, NONE);
    h.select(0, 0); // the "+" row
    h.type_text("Ann");
    h.key(Key::Enter, NONE);
    h.doc.save();
    wait_for(|| {
        h.doc.poll();
        !h.doc.is_saving()
    });
    assert_eq!(h.doc.save_error(), None);
    let path = h.doc.path().unwrap().to_path_buf();
    assert_eq!(std::fs::read(&path).unwrap(), b"Name\r\nAnn\r\n");
}
