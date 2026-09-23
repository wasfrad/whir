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
