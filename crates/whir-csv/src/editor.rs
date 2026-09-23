//! The in-cell editor: a frameless multi-line `TextEdit` laid exactly over the
//! cell (same font, padding and baseline as the grid's text, so nothing jumps
//! when editing starts). It grows to the right with the text up to the edge
//! of the view, then wraps and grows downward, like Excel.
//!
//! egui details this relies on (0.36):
//! - keys the editor acts on are consumed *before* the `TextEdit` runs (it
//!   reads events without consuming them), with exact modifiers so Alt+Enter
//!   stays a newline;
//! - focus is re-asserted every frame and the event filter keeps Tab, Escape
//!   and arrows (otherwise egui's focus pass moves or drops focus on them);
//! - after Tab, egui's pending focus move is cancelled so no button gets it.

use egui::text::{CCursor, CCursorRange};
use egui::{
    Context, EventFilter, FontId, Frame, Key, KeyboardShortcut, Margin, Modifiers, Rect, Shape,
    Stroke, TextEdit, Ui, UiBuilder, vec2,
};
use whir_core::theme::Theme;

use crate::edit::{ColKey, RowKey};
use crate::grid::{PAD, Selection};
use crate::keys::{CTRL, NONE, SHIFT, consume_exact};

/// How arrow keys behave while editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Started by typing: arrows commit and move (Excel's "Enter" mode).
    Enter,
    /// F2 / double-click: arrows move the caret.
    Edit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowTarget {
    Key(RowKey),
    /// Renaming a column.
    Header,
    /// The "+" row: committing adds a row.
    GhostRow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColTarget {
    Key(ColKey),
    /// The "+" column: committing adds a column.
    GhostCol,
}

/// Where the cursor goes after a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    /// Keep the selection as it is (the user just clicked elsewhere).
    Leave,
    /// Select the edited cell.
    Stay,
    Up,
    Down,
    Left,
    Right,
    /// Enter: down, back to the column Tab started from (Excel).
    Enter,
    /// Tab: right, remembering where the Tabs started.
    Tab,
    BackTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorAction {
    Commit(Move),
    /// Ctrl+Enter: the value goes into every selected cell.
    CommitFill,
    Cancel,
}

pub struct Editor {
    pub row: RowTarget,
    pub col: ColTarget,
    pub text: String,
    /// The value when editing started (unchanged text commits nothing).
    pub original: String,
    pub mode: Mode,
    pub id: egui::Id,
    /// Screen rect last frame, clipped to the view (a press outside it
    /// commits — also when the cell has scrolled out of view).
    pub rect: Rect,
    /// Drawn at least once (a press in the frame that started it doesn't count).
    pub shown: bool,
    /// Selection when editing started (restored by undo).
    pub sel_before: Option<Selection>,
}

impl Editor {
    pub fn is_header(&self) -> bool {
        self.row == RowTarget::Header
    }

    /// Store a fresh text state: caret at the end, or everything selected
    /// (renaming a column), and take keyboard focus.
    pub fn prime(&self, ctx: &Context, select_all: bool) {
        let n = self.text.chars().count();
        let mut state = egui::text_edit::TextEditState::default();
        let range = if select_all {
            CCursorRange::two(CCursor::new(0), CCursor::new(n))
        } else {
            CCursorRange::one(CCursor::new(n))
        };
        state.cursor.set_char_range(Some(range));
        state.store(ctx, self.id);
        ctx.memory_mut(|m| m.request_focus(self.id));
    }

    /// Forget the text state and give up focus.
    pub fn close(&self, ctx: &Context) {
        forget(ctx, self.id);
    }

    /// Keys the editor acts on, consumed before the `TextEdit` sees them.
    /// Everything else (typing, Delete, Home/End, Ctrl+A/C/X/V/Z/Y) is the
    /// text field's.
    pub fn consume_keys(&mut self, ui: &mut Ui) -> Option<EditorAction> {
        let header = self.is_header();
        let enter_mode = self.mode == Mode::Enter && !header;
        let mut toggle = false;
        let action = ui.input_mut(|i| {
            if consume_exact(i, NONE, Key::Escape) {
                return Some(EditorAction::Cancel);
            }
            if !header && consume_exact(i, CTRL, Key::Enter) {
                return Some(EditorAction::CommitFill);
            }
            let stay = |m: Move| if header { Move::Stay } else { m };
            for (mods, key, m) in [
                (SHIFT, Key::Enter, Move::Up),
                (NONE, Key::Enter, Move::Enter),
                (SHIFT, Key::Tab, Move::BackTab),
                (NONE, Key::Tab, Move::Tab),
            ] {
                if consume_exact(i, mods, key) {
                    return Some(EditorAction::Commit(stay(m)));
                }
            }
            if enter_mode {
                for (key, m) in [
                    (Key::ArrowUp, Move::Up),
                    (Key::ArrowDown, Move::Down),
                    (Key::ArrowLeft, Move::Left),
                    (Key::ArrowRight, Move::Right),
                ] {
                    if consume_exact(i, NONE, key) {
                        return Some(EditorAction::Commit(m));
                    }
                }
            }
            toggle = consume_exact(i, NONE, Key::F2);
            None
        });
        // Whatever egui's focus pass planned for Tab/arrows this frame
        // (moving focus to some other widget) is cancelled.
        ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        if toggle {
            self.mode = match self.mode {
                Mode::Enter => Mode::Edit,
                Mode::Edit => Mode::Enter,
            };
        }
        action
    }

    /// Draw the editor over `cell`, clipped to `clip`. Keeps focus even when
    /// the cell has scrolled out of view.
    pub fn show(&mut self, ui: &mut Ui, cell: Rect, clip: Rect, theme: &Theme) {
        let font = FontId::proportional(theme.metrics.body_text_size);
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        let vpad = ((cell.height() - row_h) / 2.0).round().max(0.0);
        // Wider with the text, up to the visible right edge; then it wraps.
        let max_w = (clip.right() - cell.left()).max(cell.width());
        let one_line = ui
            .painter()
            .layout_no_wrap(self.text.clone(), font.clone(), theme.text)
            .size()
            .x;
        let w = (one_line + 2.0 * PAD + 6.0).clamp(cell.width(), max_w);
        let wrapped = ui.painter().layout(
            self.text.clone(),
            font.clone(),
            theme.text,
            (w - 2.0 * PAD).max(1.0),
        );
        let h = (wrapped.size().y.max(row_h) + 2.0 * vpad).max(cell.height());
        let rect = Rect::from_min_size(cell.min, vec2(w, h));
        self.rect = rect.intersect(clip);
        self.shown = true;

        if !ui.memory(|m| m.has_focus(self.id)) {
            ui.memory_mut(|m| m.request_focus(self.id));
        }
        let painter = ui.painter().with_clip_rect(clip);
        let bg = painter.add(Shape::Noop);
        let margin = Margin {
            left: PAD as i8,
            right: PAD as i8,
            top: vpad as i8,
            bottom: vpad as i8,
        };
        let return_key = if self.is_header() {
            None
        } else {
            // Alt+Enter is a line break inside the cell, as in Excel.
            Some(KeyboardShortcut::new(Modifiers::ALT, Key::Enter))
        };
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ui.set_clip_rect(clip);
            TextEdit::multiline(&mut self.text)
                .id(self.id)
                .frame(Frame::NONE.inner_margin(margin))
                .font(font)
                .text_color(theme.text)
                .desired_rows(1)
                .desired_width(w)
                .min_size(vec2(w, h))
                .return_key(return_key)
                .event_filter(EventFilter {
                    tab: true,
                    escape: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                })
                .show(ui);
        });
        painter.set(
            bg,
            egui::epaint::RectShape::new(
                rect,
                0.0,
                theme.content_bg,
                Stroke::new(2.0, theme.selection_stroke),
                egui::StrokeKind::Inside,
            ),
        );
    }
}

/// Drop an editor's text state and focus (it may already be gone).
pub fn forget(ctx: &Context, id: egui::Id) {
    ctx.data_mut(|d| d.remove::<egui::text_edit::TextEditState>(id));
    ctx.memory_mut(|m| m.surrender_focus(id));
}
