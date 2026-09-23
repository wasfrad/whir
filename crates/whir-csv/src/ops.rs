//! Applying what the user asked for (grid [`Action`]s) to a loaded table, and
//! following the overlay's [`Effect`]s in the view state.

use whir_core::Waker;
use whir_core::find_bar::fmt_int;

use crate::edit::{Edits, MAX_EDIT_ROWS};
use crate::grid::{Action, Editable, Gate};
use crate::loaded::Loaded;

impl Loaded {
    pub fn is_dirty(&self) -> bool {
        self.edits.is_dirty()
    }

    /// Why this table can't be edited at all, if it can't.
    pub fn read_only_reason(&self) -> Option<&'static str> {
        (self.base_rows() >= MAX_EDIT_ROWS)
            .then_some("This file has too many rows to edit (over 2 billion).")
    }

    /// What editing is allowed right now.
    pub fn editable(&self) -> Editable {
        if let Some(r) = self.read_only_reason() {
            return Editable {
                cells: Gate::No(r),
                rows: Gate::No(r),
                cols: Gate::No(r),
                ghost_row: false,
                ghost_col: false,
            };
        }
        // Editing arrives with the cell editor.
        Editable::READ_ONLY
    }

    /// Finish anything half-done before the document is saved or hidden.
    pub fn commit_pending(&mut self) {}

    /// The indexer turned quoting off (unbalanced quotes): rows were numbered
    /// differently, so everything keyed by row is void.
    pub fn on_reparse(&mut self, now: f64) {
        self.measured_rows = 0;
        self.grid.sel = None;
        if self.order.is_some() {
            self.install_order(None);
        }
        if self.edits.is_dirty() {
            self.edits = Edits::default();
            self.status = Some((
                "This file has an unbalanced quote, so it is now shown line by line. \
                 Your unsaved edits were dropped."
                    .into(),
                f64::INFINITY,
            ));
        } else {
            self.edits.clear_history();
            let _ = now;
        }
    }

    pub fn apply(&mut self, action: Action, ui: &mut egui::Ui, waker: &Waker) {
        let now = self.now;
        match action {
            Action::ToggleSort(col) => self.toggle_sort(col, waker, now),
            Action::Sort(col, dir) => self.sort(col, Some(dir), waker, now),
            Action::OriginalOrder => self.sort(0, None, waker, now),
            Action::Copy => self.copy(ui),
            _ => {}
        }
    }

    fn copy(&mut self, ui: &egui::Ui) {
        let Some((text, cells, truncated)) = self.copy_text() else {
            return;
        };
        ui.ctx().copy_text(text);
        let msg = if truncated {
            format!(
                "Copied the first {} cells (selection too large)",
                fmt_int(cells)
            )
        } else {
            format!(
                "Copied {} cell{}",
                fmt_int(cells),
                if cells == 1 { "" } else { "s" }
            )
        };
        self.set_status(msg, self.now);
    }
}
