//! How a PDF opens: at the zoom picked in Settings, or at the one used last on
//! any PDF; whether each file reopens where it was left; the minimap.

use std::sync::Mutex;

use whir_core::settings;

const OPENING: &str = "pdf.zoom";
const LAST: &str = "pdf.zoom.last";
const REMEMBER: &str = "pdf.remember";
const MINIMAP: &str = "pdf.minimap";

/// A zoom a PDF can be at: a fit (which follows the window) or a percentage
/// (100% is actual size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZoomPref {
    FitWidth,
    FitPage,
    Percent(f64),
}

impl ZoomPref {
    /// As stored: `fit-width`, `fit-page`, `85%`.
    pub fn name(self) -> String {
        match self {
            ZoomPref::FitWidth => "fit-width".into(),
            ZoomPref::FitPage => "fit-page".into(),
            ZoomPref::Percent(p) => format!("{}%", (p * 10.0).round() / 10.0),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "fit-width" => Some(ZoomPref::FitWidth),
            "fit-page" => Some(ZoomPref::FitPage),
            s => {
                let p: f64 = s.strip_suffix('%')?.trim().parse().ok()?;
                (p.is_finite() && p > 0.0).then_some(ZoomPref::Percent(p))
            }
        }
    }

    /// For people: "Fit page", "Fit width", "Actual size", "85%".
    pub fn label(self) -> String {
        match self {
            ZoomPref::FitWidth => "Fit width".into(),
            ZoomPref::FitPage => "Fit page".into(),
            ZoomPref::Percent(p) if (p - 100.0).abs() < 0.05 => "Actual size".into(),
            ZoomPref::Percent(p) => format!("{p:.0}%"),
        }
    }
}

/// What Settings says a PDF opens at (one it has no view of its own for).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    /// Whatever was picked last on any PDF.
    Last,
    FitPage,
    FitWidth,
    Actual,
}

impl Opening {
    pub const ALL: [Opening; 4] = [
        Opening::Last,
        Opening::FitPage,
        Opening::FitWidth,
        Opening::Actual,
    ];

    fn name(self) -> &'static str {
        match self {
            Opening::Last => "last",
            Opening::FitPage => "fit-page",
            Opening::FitWidth => "fit-width",
            Opening::Actual => "actual",
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        Opening::ALL.into_iter().find(|o| o.name() == s.trim())
    }

    pub fn label(self) -> &'static str {
        match self {
            Opening::Last => "The zoom I used last",
            Opening::FitPage => "Fit page",
            Opening::FitWidth => "Fit width",
            Opening::Actual => "Actual size",
        }
    }
}

pub fn opening() -> Opening {
    settings::get_str(OPENING)
        .and_then(|s| Opening::from_name(&s))
        .unwrap_or(Opening::Last)
}

pub fn set_opening(o: Opening) {
    settings::set_str(OPENING, o.name());
}

/// The zoom a PDF opens at when it has no view of its own to go back to.
pub fn initial() -> ZoomPref {
    match opening() {
        Opening::Last => last(),
        Opening::FitPage => ZoomPref::FitPage,
        Opening::FitWidth => ZoomPref::FitWidth,
        Opening::Actual => ZoomPref::Percent(100.0),
    }
}

/// The zoom last picked on any PDF, in memory (loaded from settings once).
static LAST_USED: Mutex<Option<ZoomPref>> = Mutex::new(None);

/// The zoom last picked on any PDF: fit width until one is.
pub fn last() -> ZoomPref {
    let mut l = LAST_USED.lock().unwrap_or_else(|e| e.into_inner());
    *l.get_or_insert_with(|| {
        settings::get_str(LAST)
            .and_then(|s| ZoomPref::parse(&s))
            .unwrap_or(ZoomPref::FitWidth)
    })
}

/// Someone put a PDF at `z`: PDFs opened from now on follow (on disk at
/// [`save_last`]).
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

/// Each PDF reopens at the page and zoom it was left at.
pub fn remember() -> bool {
    settings::get_bool(REMEMBER, true)
}

pub fn set_remember(on: bool) {
    settings::set_bool(REMEMBER, on);
}

pub fn minimap() -> bool {
    settings::get_bool(MINIMAP, true)
}

pub fn set_minimap(on: bool) {
    settings::set_bool(MINIMAP, on);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_names_round_trip() {
        for z in [
            ZoomPref::FitWidth,
            ZoomPref::FitPage,
            ZoomPref::Percent(100.0),
            ZoomPref::Percent(85.3),
            ZoomPref::Percent(1200.0),
        ] {
            assert_eq!(ZoomPref::parse(&z.name()), Some(z), "{}", z.name());
        }
        assert_eq!(ZoomPref::Percent(85.0).name(), "85%");
        assert_eq!(ZoomPref::Percent(85.333).name(), "85.3%");
        assert_eq!(ZoomPref::parse(" 150 % "), Some(ZoomPref::Percent(150.0)));
        for bad in ["", "fit", "150", "-5%", "0%", "x%", "inf%"] {
            assert_eq!(ZoomPref::parse(bad), None, "{bad:?}");
        }
        assert_eq!(ZoomPref::Percent(100.0).label(), "Actual size");
        assert_eq!(ZoomPref::Percent(82.6).label(), "83%");
    }
}
