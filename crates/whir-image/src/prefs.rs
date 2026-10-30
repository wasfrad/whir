//! How an image opens (and the next one in the folder): fitted to the
//! window, at actual size, or at the zoom used last.

use std::sync::Mutex;

use whir_core::settings;

const OPENING: &str = "image.zoom";
const LAST: &str = "image.zoom.last";

/// An image's zoom: fitted (large images shrink to the window, small ones
/// stay at 100%), or a percentage of actual size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoomPref {
    Fit,
    Percent(f32),
}

impl ZoomPref {
    /// As stored: `fit`, `250%`.
    fn name(self) -> String {
        match self {
            ZoomPref::Fit => "fit".into(),
            ZoomPref::Percent(p) => format!("{}%", (p * 10.0).round() / 10.0),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "fit" => Some(ZoomPref::Fit),
            s => {
                let p: f32 = s.strip_suffix('%')?.trim().parse().ok()?;
                (p.is_finite() && p > 0.0).then_some(ZoomPref::Percent(p))
            }
        }
    }

    /// For people: "Fit to the window", "Actual size", "250%".
    pub fn label(self) -> String {
        match self {
            ZoomPref::Fit => "Fit to the window".into(),
            ZoomPref::Percent(p) if (p - 100.0).abs() < 0.05 => "Actual size".into(),
            ZoomPref::Percent(p) => format!("{p:.0}%"),
        }
    }
}

/// What Settings says an image opens at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    Fit,
    Actual,
    /// Whatever was picked last on any image.
    Last,
}

impl Opening {
    pub const ALL: [Opening; 3] = [Opening::Fit, Opening::Actual, Opening::Last];

    fn name(self) -> &'static str {
        match self {
            Opening::Fit => "fit",
            Opening::Actual => "actual",
            Opening::Last => "last",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Opening::Fit => "Fit to the window",
            Opening::Actual => "Actual size",
            Opening::Last => "The zoom I used last",
        }
    }
}

pub fn opening() -> Opening {
    let name = settings::get_str(OPENING);
    Opening::ALL
        .into_iter()
        .find(|o| name.as_deref().map(str::trim) == Some(o.name()))
        .unwrap_or(Opening::Fit)
}

pub fn set_opening(o: Opening) {
    settings::set_str(OPENING, o.name());
}

/// The zoom an image opens at.
pub fn initial() -> ZoomPref {
    match opening() {
        Opening::Fit => ZoomPref::Fit,
        Opening::Actual => ZoomPref::Percent(100.0),
        Opening::Last => last(),
    }
}

/// The zoom last picked on any image, in memory (loaded from settings once).
static LAST_USED: Mutex<Option<ZoomPref>> = Mutex::new(None);

/// The zoom last picked on any image: fit until one is.
pub fn last() -> ZoomPref {
    let mut l = LAST_USED.lock().unwrap_or_else(|e| e.into_inner());
    *l.get_or_insert_with(|| {
        settings::get_str(LAST)
            .and_then(|s| ZoomPref::parse(&s))
            .unwrap_or(ZoomPref::Fit)
    })
}

/// Someone zoomed an image to `z` (on disk at [`save_last`]).
pub(crate) fn used(z: ZoomPref) {
    *LAST_USED.lock().unwrap_or_else(|e| e.into_inner()) = Some(z);
}

/// Keep the last zoom in the settings file (nothing is written if it's
/// unchanged).
pub(crate) fn save_last() {
    let last = *LAST_USED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(z) = last {
        settings::set_str(LAST, &z.name());
    }
}

/// Forget the in-memory last zoom (tests start from the settings).
#[cfg(test)]
pub(crate) fn reset_last() {
    *LAST_USED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_names_round_trip() {
        for z in [
            ZoomPref::Fit,
            ZoomPref::Percent(100.0),
            ZoomPref::Percent(37.5),
            ZoomPref::Percent(6400.0),
        ] {
            assert_eq!(ZoomPref::parse(&z.name()), Some(z), "{}", z.name());
        }
        for bad in ["", "fit-page", "50", "-1%", "0%"] {
            assert_eq!(ZoomPref::parse(bad), None, "{bad:?}");
        }
        assert_eq!(ZoomPref::Percent(100.0).label(), "Actual size");
    }
}
