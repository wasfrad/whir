//! Right-click menus for row numbers, column headers and cells.
//!
//! Items that can't run right now stay visible but disabled, with the reason
//! on hover ("Saving…", "Rows can be added once the whole file has been read").

use egui::Ui;
use whir_core::icons::Icon;
use whir_core::kit;

use crate::grid::{Action, Editable, Gate, Selection};
use crate::sort::{Direction, SortSpec};

/// What was right-clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuHit {
    /// Row numbers.
    Rows,
    Header(usize),
    Cells,
}

pub struct MenuCx {
    pub hit: MenuHit,
    /// The selection, limited to real cells.
    pub sel: Option<Selection>,
    pub rows: u64,
    pub cols: usize,
    pub sort: Option<SortSpec>,
    /// A custom order is shown.
    pub sorted: bool,
    pub editable: Editable,
    /// The active cell's value (a menu on cells).
    pub cursor_text: Option<String>,
}

fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        one.to_owned()
    } else {
        many.replace("{}", &n.to_string())
    }
}

fn item(
    ui: &mut Ui,
    label: &str,
    shortcut: &str,
    gate: Gate,
    action: Action,
    out: &mut Vec<Action>,
) {
    let icon = match action {
        Action::Cut => Some(Icon::Cut),
        Action::Copy => Some(Icon::Copy),
        Action::RequestPaste => Some(Icon::Paste),
        Action::Clear => Some(Icon::Delete),
        Action::FindInTabs(_) => Some(Icon::Search),
        _ => None,
    };
    entry(ui, icon, label, shortcut, gate, action, out);
}

fn entry(
    ui: &mut Ui,
    icon: Option<Icon>,
    label: &str,
    shortcut: &str,
    gate: Gate,
    action: Action,
    out: &mut Vec<Action>,
) {
    let resp = ui.add_enabled(gate.allowed(), kit::menu_entry(icon, label, shortcut));
    let resp = match gate {
        Gate::No(reason) => resp.on_disabled_hover_text(reason),
        Gate::Yes => resp,
    };
    if resp.clicked() {
        out.push(action);
        ui.close();
    }
}

/// "Find “2.4567” in all tabs" for a value worth searching for (one line).
fn find_label(v: &str) -> Option<String> {
    const MAX: usize = 24;
    let v = v.trim();
    if v.is_empty() || v.contains(['\n', '\r']) {
        return None;
    }
    let shown = match v.char_indices().nth(MAX) {
        Some((i, _)) => format!("{}…", &v[..i]),
        None => v.to_owned(),
    };
    Some(format!("Find “{shown}” in all tabs"))
}

pub fn show(ui: &mut Ui, cx: &MenuCx, out: &mut Vec<Action>) {
    ui.set_min_width(200.0);
    let (n_rows, n_cols) = match cx.sel {
        Some(s) => {
            let (a, b) = s.rows();
            let (c, d) = s.cols();
            (b - a + 1, (d - c + 1) as u64)
        }
        None => (1, 1),
    };
    let e = &cx.editable;
    let has_rows = cx.rows > 0 && cx.sel.is_some();
    let rows_gate = if has_rows {
        e.rows
    } else {
        Gate::No("No rows selected.")
    };
    match cx.hit {
        MenuHit::Rows => {
            item(
                ui,
                &count(n_rows, "Insert row above", "Insert {} rows above"),
                "Ctrl+Shift+=",
                e.rows,
                Action::InsertRows { above: true },
                out,
            );
            item(
                ui,
                &count(n_rows, "Insert row below", "Insert {} rows below"),
                "",
                e.rows,
                Action::InsertRows { above: false },
                out,
            );
            item(
                ui,
                &count(n_rows, "Delete row", "Delete {} rows"),
                "Ctrl+−",
                rows_gate,
                Action::DeleteRows,
                out,
            );
            ui.separator();
            item(ui, "Copy", "Ctrl+C", Gate::Yes, Action::Copy, out);
        }
        MenuHit::Header(col) => {
            let in_range = col < cx.cols;
            let cols_gate = if in_range {
                e.cols
            } else {
                Gate::No("No column here.")
            };
            item(
                ui,
                &count(n_cols, "Insert column left", "Insert {} columns left"),
                "",
                e.cols,
                Action::InsertCols { left: true },
                out,
            );
            item(
                ui,
                &count(n_cols, "Insert column right", "Insert {} columns right"),
                "",
                e.cols,
                Action::InsertCols { left: false },
                out,
            );
            let rename_gate = if in_range {
                e.cells
            } else {
                Gate::No("No column here.")
            };
            item(
                ui,
                "Rename column…",
                "",
                rename_gate,
                Action::Rename(col),
                out,
            );
            item(
                ui,
                &count(n_cols, "Delete column", "Delete {} columns"),
                "",
                cols_gate,
                Action::DeleteCols,
                out,
            );
            ui.separator();
            let current = cx.sort.filter(|s| s.column == col).map(|s| s.direction);
            let sort_gate = if in_range {
                Gate::Yes
            } else {
                Gate::No("No column here.")
            };
            for (direction, label) in [
                (Direction::Ascending, "Sort ascending"),
                (Direction::Descending, "Sort descending"),
            ] {
                let on = (current == Some(direction)).then_some(Icon::Check);
                entry(
                    ui,
                    on,
                    label,
                    "",
                    sort_gate,
                    Action::Sort(col, direction),
                    out,
                );
            }
            let original = if cx.sorted {
                Gate::Yes
            } else {
                Gate::No("Rows are already in file order.")
            };
            item(
                ui,
                "Original order",
                "",
                original,
                Action::OriginalOrder,
                out,
            );
        }
        MenuHit::Cells => {
            if let Some(label) = cx.cursor_text.as_deref().and_then(find_label) {
                let text = cx.cursor_text.clone().unwrap_or_default();
                item(ui, &label, "", Gate::Yes, Action::FindInTabs(text), out);
                ui.separator();
            }
            item(ui, "Cut", "Ctrl+X", e.cells, Action::Cut, out);
            item(ui, "Copy", "Ctrl+C", Gate::Yes, Action::Copy, out);
            item(ui, "Paste", "Ctrl+V", e.cells, Action::RequestPaste, out);
            item(ui, "Clear contents", "Delete", e.cells, Action::Clear, out);
            ui.separator();
            item(
                ui,
                &count(n_rows, "Insert row above", "Insert {} rows above"),
                "Ctrl+Shift+=",
                e.rows,
                Action::InsertRows { above: true },
                out,
            );
            item(
                ui,
                &count(n_rows, "Delete row", "Delete {} rows"),
                "Ctrl+−",
                rows_gate,
                Action::DeleteRows,
                out,
            );
        }
    }
}
