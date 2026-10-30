//! Time tracking: customers and their projects, timers started and stopped
//! from the dock, and a timesheet per week (Monday to Sunday) that is closed
//! out to CSV when it is handed in.
//!
//! Kept in `Documents\whir\Time` (`WHIR_TIME_DIR` elsewhere): the data in a
//! hidden `.whir` folder, the closed weeks' CSVs beside it.

mod export;
mod format;
mod model;
mod store;

use std::path::PathBuf;

pub use export::{csv_paths, entries_csv, timesheet_csv, timesheet_tsv};
pub use model::{
    Customer, Entry, Grid, Options, Rounding, Running, State, Timer, Week, split_days, week_of,
};
pub use store::{Store, Txn};

/// The time folder's name inside the notes folder.
pub const FOLDER: &str = "Time";

/// Where the timesheet is kept: `WHIR_TIME_DIR`, else `Time` inside the notes
/// folder (`WHIR_NOTES_DIR`, else `Documents\whir`). `None` if the Documents
/// folder can't be resolved.
pub fn resolve_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("WHIR_TIME_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("WHIR_NOTES_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::library::documents_dir().map(|d| d.join("whir")))
        .map(|notes| notes.join(FOLDER))
}
