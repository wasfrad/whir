//! The in-cell editor (see the module docs added with the editor itself).

/// How arrow keys behave while editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Started by typing: arrows commit and move (Excel's "Enter" mode).
    Enter,
    /// F2 / double-click: arrows move the caret.
    Edit,
}
