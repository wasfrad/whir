use std::path::Path;
use std::sync::Arc;

use crate::{TextureRegistry, Waker};

/// What kind of content a tab shows; drives the tab glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Image,
    Pdf,
    Csv,
    Error,
}

/// Everything a pillar needs to start loading a document (possibly before the
/// window or GPU device exist).
#[derive(Clone)]
pub struct OpenCx {
    pub textures: Arc<dyn TextureRegistry>,
    pub waker: Waker,
}

/// Per-frame context handed to [`Document::ui`].
pub struct DocCx<'a> {
    pub textures: &'a Arc<dyn TextureRegistry>,
    pub waker: &'a Waker,
    /// True for the frame in which this document became the active tab.
    pub just_activated: bool,
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
}
