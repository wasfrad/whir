//! Editing the marks on one image (add, move, resize, remove, with undo), and
//! the tools' current ink and pen.

use std::sync::atomic::{AtomicU8, Ordering};

use whir_core::marks::{Ink, Mark, MarkSet, Pen};
use whir_core::settings;

#[derive(Clone, Debug)]
enum Op {
    Add {
        index: usize,
        mark: Mark,
    },
    Remove {
        index: usize,
        mark: Mark,
    },
    Change {
        index: usize,
        before: Mark,
        after: Mark,
    },
}

impl Op {
    /// The op that takes this one back.
    fn inverse(&self) -> Op {
        match self.clone() {
            Op::Add { index, mark } => Op::Remove { index, mark },
            Op::Remove { index, mark } => Op::Add { index, mark },
            Op::Change {
                index,
                before,
                after,
            } => Op::Change {
                index,
                before: after,
                after: before,
            },
        }
    }
}

#[derive(Default)]
pub struct Editor {
    /// Bottom to top.
    marks: Vec<Mark>,
    undo: Vec<Op>,
    redo: Vec<Op>,
    /// The mark that Delete and a drag act on.
    pub selected: Option<usize>,
    /// The image is in the screenshots folder: its marks are saved beside it.
    pub stored: bool,
    /// Pixel size of the image the marks belong to.
    pub size: [u32; 2],
    /// Lines of the marks file from a newer whir, written back as they were.
    other: Vec<String>,
    /// Counts edits; `saved` is the count last handed to the saver.
    revision: u64,
    saved: u64,
    /// The last save failed.
    pub failed: bool,
}

impl Editor {
    /// For an image in the screenshots folder, with what was saved for it.
    pub fn stored(saved: Option<MarkSet>) -> Self {
        let set = saved.unwrap_or_default();
        Self {
            marks: set.marks,
            other: set.other,
            size: set.size,
            stored: true,
            ..Self::default()
        }
    }

    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    /// The marks belong to an image of another size (it was replaced): they
    /// can't be placed, so they are let go.
    pub fn drop_loaded(&mut self) {
        self.marks.clear();
        self.other.clear();
        self.undo.clear();
        self.redo.clear();
        self.selected = None;
    }

    /// There are edits the saver hasn't been given (or couldn't write).
    pub fn unsaved(&self) -> bool {
        self.stored && (self.revision != self.saved || self.failed)
    }

    /// What to write, if anything changed since the last time this was asked.
    pub fn take_unsaved(&mut self) -> Option<MarkSet> {
        if !self.stored || self.revision == self.saved {
            return None;
        }
        self.saved = self.revision;
        self.failed = false;
        Some(MarkSet {
            size: self.size,
            marks: self.marks.clone(),
            other: self.other.clone(),
        })
    }

    /// Stop caring about unsaved edits (the image is being deleted).
    pub fn forget_unsaved(&mut self) {
        self.saved = self.revision;
        self.failed = false;
    }

    /// A new mark, on top. Returns its index.
    pub fn add(&mut self, mark: Mark) -> usize {
        let index = self.marks.len();
        self.apply(Op::Add { index, mark });
        index
    }

    pub fn remove(&mut self, index: usize) {
        if let Some(mark) = self.marks.get(index).cloned() {
            self.apply(Op::Remove { index, mark });
        }
    }

    /// Replace mark `index` (moved, resized, recoloured, retyped). The
    /// selection stays on it.
    pub fn change(&mut self, index: usize, after: Mark) {
        match self.marks.get(index) {
            Some(before) if *before != after => {
                let before = before.clone();
                let selected = self.selected;
                self.apply(Op::Change {
                    index,
                    before,
                    after,
                });
                self.selected = selected;
            }
            _ => {}
        }
    }

    fn apply(&mut self, op: Op) {
        self.perform(op.clone());
        self.undo.push(op);
        self.redo.clear();
    }

    fn perform(&mut self, op: Op) {
        self.revision += 1;
        // Indices shift under the selection: simplest to drop it.
        self.selected = None;
        match op {
            Op::Add { index, mark } => self.marks.insert(index, mark),
            Op::Remove { index, .. } => {
                self.marks.remove(index);
            }
            Op::Change { index, after, .. } => self.marks[index] = after,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Returns whether there was anything to undo.
    pub fn undo(&mut self) -> bool {
        let Some(op) = self.undo.pop() else {
            return false;
        };
        self.perform(op.inverse());
        self.redo.push(op);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(op) = self.redo.pop() else {
            return false;
        };
        self.perform(op.clone());
        self.undo.push(op);
        true
    }

    /// The topmost mark over image pixel (`x`, `y`).
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.marks.iter().rposition(|m| m.contains(x, y))
    }
}

// --- the ink and the pen ------------------------------------------------------

const INK_KEY: &str = "image.highlight.ink";
const PEN_KEY: &str = "image.text.pen";
const UNSET: u8 = u8::MAX;
/// Shared by every image tab, and remembered across runs.
static INK: AtomicU8 = AtomicU8::new(UNSET);
static PEN: AtomicU8 = AtomicU8::new(UNSET);

pub fn current_ink() -> Ink {
    Ink::ALL
        .get(INK.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or_default()
}

pub fn set_ink(ink: Ink) {
    INK.store(ink as u8, Ordering::Relaxed);
    settings::set_str(INK_KEY, ink.name());
}

pub fn current_pen() -> Pen {
    Pen::ALL
        .get(PEN.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or_default()
}

pub fn set_pen(pen: Pen) {
    PEN.store(pen as u8, Ordering::Relaxed);
    settings::set_str(PEN_KEY, pen.name());
}

/// Read the remembered ink and pen. Reads the settings file the first time,
/// so this is called from a decode worker, never from the UI's first frame.
pub fn load_choices() {
    load(&INK, INK_KEY, |name| Ink::from_name(name).map(|i| i as u8));
    load(&PEN, PEN_KEY, |name| Pen::from_name(name).map(|p| p as u8));
}

fn load(slot: &AtomicU8, key: &str, index: impl FnOnce(&str) -> Option<u8>) {
    if slot.load(Ordering::Relaxed) != UNSET {
        return;
    }
    let remembered = settings::get_str(key).and_then(|s| index(&s)).unwrap_or(0);
    // Unless the user picked one meanwhile.
    let _ = slot.compare_exchange(UNSET, remembered, Ordering::Relaxed, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use whir_core::marks::{Kind, Text};

    use super::*;

    fn mark(x: u32) -> Mark {
        Mark::highlight(x, 0, 10, 10, Ink::Yellow)
    }

    #[test]
    fn undo_and_redo_walk_through_add_move_and_remove() {
        let mut e = Editor::default();
        e.add(mark(0));
        e.add(mark(100));
        e.change(0, mark(50));
        e.remove(1);
        assert_eq!(e.marks(), [mark(50)]);

        assert!(e.undo());
        assert_eq!(e.marks(), [mark(50), mark(100)]);
        assert!(e.undo());
        assert_eq!(e.marks(), [mark(0), mark(100)]);
        assert!(e.undo());
        assert!(e.undo());
        assert!(e.marks().is_empty());
        assert!(!e.undo());

        for _ in 0..4 {
            assert!(e.redo());
        }
        assert_eq!(e.marks(), [mark(50)]);
        assert!(!e.redo());
    }

    #[test]
    fn text_is_undone_like_any_other_mark() {
        let text = |body: &str| Mark {
            x: 5,
            y: 5,
            w: 40,
            h: 20,
            kind: Kind::Text(Text {
                body: body.to_owned(),
                size: 16,
                pen: Pen::Red,
                turn: 0,
            }),
        };
        let mut e = Editor::default();
        assert_eq!(e.add(mark(0)), 0);
        assert_eq!(e.add(text("Hi")), 1);
        e.change(1, text("Hello"));
        assert_eq!(e.marks()[1], text("Hello"));
        e.undo();
        assert_eq!(e.marks()[1], text("Hi"));
        e.undo();
        assert_eq!(e.marks(), [mark(0)]);
        e.redo();
        e.redo();
        assert_eq!(e.marks(), [mark(0), text("Hello")]);
    }

    #[test]
    fn a_new_edit_forgets_what_was_undone() {
        let mut e = Editor::default();
        e.add(mark(0));
        e.add(mark(100));
        e.undo();
        e.add(mark(200));
        assert!(!e.redo());
        assert_eq!(e.marks(), [mark(0), mark(200)]);
    }

    #[test]
    fn a_change_to_the_same_mark_is_not_an_edit() {
        let mut e = Editor::default();
        e.add(mark(0));
        e.change(0, mark(0));
        e.change(7, mark(1));
        e.remove(7);
        assert!(e.undo());
        assert!(!e.undo());
    }

    #[test]
    fn moving_keeps_the_selection_and_other_edits_drop_it() {
        let mut e = Editor::default();
        e.add(mark(0));
        e.selected = Some(0);
        e.change(0, mark(5));
        assert_eq!(e.selected, Some(0));
        e.undo();
        assert_eq!(e.selected, None);
        e.selected = Some(0);
        e.remove(0);
        assert_eq!(e.selected, None);
    }

    #[test]
    fn stored_marks_are_handed_to_the_saver_once_per_change() {
        let saved = MarkSet {
            size: [800, 600],
            marks: vec![mark(0)],
            other: vec!["arrow 1 2 3 4".to_owned()],
        };
        let mut e = Editor::stored(Some(saved.clone()));
        assert_eq!(e.marks(), [mark(0)]);
        // Just loaded: nothing to write.
        assert!(!e.unsaved());
        assert_eq!(e.take_unsaved(), None);

        e.add(mark(50));
        assert!(e.unsaved());
        let set = e.take_unsaved().unwrap();
        assert_eq!(set.marks, [mark(0), mark(50)]);
        // What a newer whir wrote goes back out untouched.
        assert_eq!((set.size, set.other), (saved.size, saved.other));
        assert!(!e.unsaved());
        assert_eq!(e.take_unsaved(), None);

        // Undo is a change too; a failed write keeps it unsaved.
        e.undo();
        assert_eq!(e.take_unsaved().unwrap().marks, [mark(0)]);
        e.failed = true;
        assert!(e.unsaved());
        assert_eq!(
            e.take_unsaved(),
            None,
            "not retried until something changes"
        );
        e.forget_unsaved();
        assert!(!e.unsaved());

        // An image elsewhere: marks are never saved.
        let mut e = Editor::default();
        e.add(mark(0));
        assert!(!e.unsaved());
        assert_eq!(e.take_unsaved(), None);
    }

    #[test]
    fn hit_finds_the_topmost_mark() {
        let mut e = Editor::default();
        e.add(mark(0));
        e.add(mark(5));
        assert_eq!(e.hit(2.0, 2.0), Some(0));
        assert_eq!(e.hit(7.0, 2.0), Some(1));
        assert_eq!(e.hit(14.0, 2.0), Some(1));
        assert_eq!(e.hit(15.0, 2.0), None);
    }
}
