use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::find_bar::FindQuery;
use crate::library::Library;
use crate::{TextureRegistry, Waker};

/// What kind of content a tab shows; drives the tab glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Image,
    Pdf,
    Csv,
    Note,
    Error,
}

/// How a document's changes reach the disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Persistence {
    /// A viewer: nothing to save.
    #[default]
    None,
    /// Saves itself (notes in the library): no dirty dot, no close prompt.
    /// [`Document::flush`] writes now.
    Auto,
    /// The user saves (Ctrl+S): dirty dot and a close prompt.
    Explicit,
}

/// Defaults for a Save As dialog.
#[derive(Clone, Debug, Default)]
pub struct SaveAsSpec {
    pub title: String,
    pub suggested_name: String,
    /// `(label, "*.csv;*.tsv")` pairs.
    pub filters: Vec<(String, String)>,
    /// Without the dot.
    pub default_ext: String,
    pub folder: Option<PathBuf>,
}

#[derive(Debug)]
pub enum SaveResult {
    /// Written (or nothing to write).
    Saved,
    /// A background write is running; watch [`Document::is_saving`] / [`Document::save_error`].
    Started,
    /// No destination yet: the shell shows a Save dialog, then calls [`Document::save_as`].
    NeedsPath(SaveAsSpec),
    Failed(String),
    Unsupported,
}

/// Things a document asks the shell to do (collected in [`DocCx::requests`]).
#[derive(Debug)]
pub enum DocRequest {
    Open(Vec<PathBuf>),
    Save,
    SaveAs,
    /// Close this document's tab.
    Close,
    /// Search every open tab for this (Find in all tabs).
    FindInTabs(FindQuery),
}

/// Where a document's search stands, for the Find in all tabs panel.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchState {
    /// The query the results belong to (the document's own find may have
    /// moved on to another one since the panel searched).
    pub query: FindQuery,
    /// Matches found so far.
    pub hits: usize,
    pub running: bool,
    /// 0..=1 while running, when known.
    pub progress: Option<f32>,
    /// The count was capped: there are more matches than `hits`.
    pub truncated: bool,
    /// The document's current match (where F3 continues from).
    pub current: Option<usize>,
}

/// One match, described for the results panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchHit {
    /// Where it is ("Row 1,234").
    pub place: String,
    /// What it's in (a column name).
    pub field: String,
    /// The text that matched (a cell's value), on one line.
    pub text: String,
    /// What's around it (the rest of the row), on one line.
    pub context: String,
}

/// Everything a pillar needs to start loading a document (possibly before the
/// window or GPU device exist).
#[derive(Clone)]
pub struct OpenCx {
    pub textures: Arc<dyn TextureRegistry>,
    pub waker: Waker,
    /// The notes folder (`Documents\whir`).
    pub library: Arc<Library>,
}

/// Per-frame context handed to [`Document::ui`].
pub struct DocCx<'a> {
    pub textures: &'a Arc<dyn TextureRegistry>,
    pub waker: &'a Waker,
    /// True for the frame in which this document became the active tab.
    pub just_activated: bool,
    pub requests: &'a mut Vec<DocRequest>,
}

/// One open file, shown in one tab.
pub trait Document {
    /// Tab label.
    fn title(&self) -> &str;

    /// Backing file, for dedupe and the tab tooltip.
    fn path(&self) -> Option<&Path>;

    fn kind(&self) -> DocKind;

    /// Still loading its first displayable content. The shell briefly holds the
    /// first frame for this, so a double-clicked file appears together with the window.
    fn is_loading(&self) -> bool {
        false
    }

    /// Absorb finished background work. Called every frame for *every* tab (so
    /// background tabs finish loading), before `ui` for the active one. Must be cheap.
    fn poll(&mut self) {}

    /// Draw into `ui` (the area below the tab strip). Only called for the active tab.
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut DocCx<'_>);

    /// The tab became visible / hidden. Hook for later texture eviction.
    fn on_visibility(&mut self, _visible: bool) {}

    // --- saving -------------------------------------------------------------

    fn persistence(&self) -> Persistence {
        Persistence::None
    }

    /// Unsaved changes (either persistence kind).
    fn is_dirty(&self) -> bool {
        false
    }

    /// A background write is in flight.
    fn is_saving(&self) -> bool {
        false
    }

    /// The last save failed (cleared by a successful one).
    fn save_error(&self) -> Option<&str> {
        None
    }

    /// Nothing typed and no file: the tab can be reused or dropped silently.
    fn is_pristine(&self) -> bool {
        false
    }

    /// Ctrl+S.
    fn save(&mut self) -> SaveResult {
        SaveResult::Unsupported
    }

    /// Defaults for Ctrl+Shift+S; `None` if the document can't be saved elsewhere.
    fn save_as_spec(&self) -> Option<SaveAsSpec> {
        None
    }

    /// Save to `path` (after a Save dialog). Library documents export a copy;
    /// others rebind to the new path.
    fn save_as(&mut self, _path: PathBuf) -> SaveResult {
        SaveResult::Unsupported
    }

    /// Autosave now, quietly and without blocking (tab close, deactivation).
    fn flush(&mut self) {}

    /// Autosave now and wait until written or `deadline` (session end, exit).
    fn flush_blocking(&mut self, _deadline: Instant) -> Result<(), String> {
        Ok(())
    }

    /// About to be dropped without saving: cancel queued writes.
    fn discard(&mut self) {}

    /// whir became active (`true`) or inactive (`false`).
    fn app_activated(&mut self, _active: bool) {}

    // --- find in all tabs ---------------------------------------------------

    /// Takes part in Find in all tabs.
    fn searchable(&self) -> bool {
        false
    }

    /// Search for `query` in the background, replacing the document's own
    /// find results: its find bar shows the query and F3 steps through them.
    fn search(&mut self, _query: &FindQuery) {}

    /// Where the document's search stands (`None`: it has none).
    fn search_state(&self) -> Option<SearchState> {
        None
    }

    /// Describe match `i` (in document order).
    fn search_hit(&mut self, _i: usize) -> Option<SearchHit> {
        None
    }

    /// Make match `i` the current one: select it and scroll it into view.
    fn reveal_hit(&mut self, _i: usize) {}
}

/// Whether closing this document needs the user's decision.
pub fn needs_attention(d: &dyn Document) -> bool {
    match d.persistence() {
        Persistence::Explicit => d.is_dirty(),
        Persistence::Auto => d.is_dirty() && d.save_error().is_some(),
        Persistence::None => false,
    }
}
