use std::sync::Arc;

/// Wakes the UI thread from a background worker so it repaints.
///
/// The shell installs a callback that posts a coalesced window message; workers
/// call [`Waker::wake`] after publishing results.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    pub fn noop() -> Self {
        Self::new(|| {})
    }

    #[inline]
    pub fn wake(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}
