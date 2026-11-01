//! whir's Settings: a tab of its own. The timesheet (the week at a glance,
//! its entries, closing it out), the timers (customers and their projects),
//! how time is kept, how whir looks, how PDFs and images open, the capture dock,
//! and how whir starts.

mod appearance;
mod capture;
mod startup;
mod timers;
mod timesheet;
mod tracking;
mod viewing;

use std::path::Path;

use egui::{Align, Layout, Margin, Rect, RichText, Stroke, pos2, vec2};
use whir_core::icons::Icon;
use whir_core::kit;
use whir_core::theme::Theme;
use whir_core::timesheet::{self as ts, Store};
use whir_core::{DocCx, DocKind, DocRequest, Document};
use whir_platform::ipc::{self, Changed};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Timesheet,
    Timers,
    Tracking,
    Appearance,
    Viewing,
    Capture,
    Startup,
}

/// The sections down the left, in the two groups they make: your time, then
/// how whir behaves.
const GROUPS: [&[Section]; 2] = [
    &[Section::Timesheet, Section::Timers, Section::Tracking],
    &[
        Section::Appearance,
        Section::Viewing,
        Section::Capture,
        Section::Startup,
    ],
];
/// Wide enough for the sections' names, and for a page beside them below this
/// the sections fold to their icons.
const NAV_WIDTH: f32 = 200.0;
const RAIL_WIDTH: f32 = 48.0;
const FOLD_BELOW: f32 = 760.0;
/// The widest a page of settings reads well, and the timesheet.
const PAGE_MAX: f32 = 720.0;
const SHEET_MAX: f32 = 1120.0;

impl Section {
    pub fn all() -> impl Iterator<Item = Section> {
        GROUPS.into_iter().flatten().copied()
    }

    /// What `from_name` knows it by.
    pub fn name(self) -> &'static str {
        match self {
            Section::Timesheet => "timesheet",
            Section::Timers => "timers",
            Section::Tracking => "tracking",
            Section::Appearance => "appearance",
            Section::Viewing => "pdfs",
            Section::Capture => "capture",
            Section::Startup => "startup",
        }
    }

    /// Words the command palette finds it by, for what's on the page.
    pub fn keywords(self) -> &'static str {
        match self {
            Section::Timesheet => "week hours timecard entries close",
            Section::Timers => "customers projects codes archive",
            Section::Tracking => "rounding round away idle keep dock",
            Section::Appearance => "theme dark light mode note text size font",
            Section::Viewing => "pdf image zoom fit minimap remember reopen",
            Section::Capture => "screenshot shortcut hotkey delay dock hide show",
            Section::Startup => "sign in autostart window monitor folders",
        }
    }

    /// As `whir.exe --settings <name>` and the dock ask for it.
    pub fn from_name(name: &str) -> Option<Section> {
        match name.to_ascii_lowercase().as_str() {
            "timesheet" | "time" | "week" => Some(Section::Timesheet),
            "timers" | "projects" | "customers" => Some(Section::Timers),
            "tracking" | "options" => Some(Section::Tracking),
            "appearance" | "theme" => Some(Section::Appearance),
            "viewing" | "pdf" | "pdfs" | "images" | "zoom" => Some(Section::Viewing),
            "capture" | "dock" => Some(Section::Capture),
            "startup" | "general" | "window" => Some(Section::Startup),
            _ => None,
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Section::Timesheet => Icon::Calendar,
            Section::Timers => Icon::People,
            Section::Tracking => Icon::Stopwatch,
            Section::Appearance => Icon::Palette,
            Section::Viewing => Icon::Picture,
            Section::Capture => Icon::Camera,
            Section::Startup => Icon::Power,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::Timesheet => "Timesheet",
            Section::Timers => "Customers & projects",
            Section::Tracking => "Time tracking",
            Section::Appearance => "Appearance",
            Section::Viewing => "PDFs & images",
            Section::Capture => "Capture & dock",
            Section::Startup => "Startup & window",
        }
    }
}

pub struct SettingsDoc {
    section: Section,
    /// `None`: no Documents folder to keep time in.
    store: Option<Store>,
    /// The timesheet as of this [`ts::generation`].
    generation: u64,
    sheet: timesheet::View,
    timers: timers::View,
    capture: capture::View,
}

impl SettingsDoc {
    pub fn new(section: Section) -> SettingsDoc {
        let store = ts::resolve_dir()
            .map(|dir| Store::open(dir).on_change(|| ipc::notify_changed(Changed::Time)));
        SettingsDoc::with_store(section, store)
    }

    fn with_store(section: Section, store: Option<Store>) -> SettingsDoc {
        let mut doc = SettingsDoc {
            section,
            store,
            generation: ts::generation(),
            sheet: timesheet::View::new(),
            timers: timers::View::default(),
            capture: capture::View::default(),
        };
        if let Some(s) = &mut doc.store {
            s.refresh();
        }
        doc
    }

    pub fn boxed(self) -> Box<dyn Document> {
        Box::new(self)
    }

    /// The timesheet was written elsewhere (the dock): read it again.
    fn refresh(&mut self) {
        if let Some(s) = &mut self.store {
            s.refresh();
        }
        self.sheet.forget();
        self.timers.forget();
        self.capture.forget();
    }
}

impl Document for SettingsDoc {
    fn title(&self) -> &str {
        "Settings"
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn kind(&self) -> DocKind {
        DocKind::Settings
    }

    fn show_section(&mut self, name: &str) {
        if let Some(s) = Section::from_name(name) {
            self.section = s;
        }
    }

    fn wants_every_key(&self) -> bool {
        self.section == Section::Capture && self.capture.recording()
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut DocCx<'_>) {
        let theme = Theme::current();
        if cx.just_activated || ts::generation() != self.generation {
            self.generation = ts::generation();
            self.refresh();
        }
        let area = ui.max_rect();
        ui.painter().rect_filled(area, 0.0, theme.page_bg);
        self.nav(ui, area, &theme);
        let folded = area.width() < FOLD_BELOW;
        let nav_width = if folded { RAIL_WIDTH } else { NAV_WIDTH };

        let body = Rect::from_min_max(pos2(area.left() + nav_width, area.top()), area.max);
        let mut body_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(body)
                .layout(Layout::top_down(Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt(("whir.settings", self.section as u8))
            .auto_shrink(false)
            .show(&mut body_ui, |ui| {
                let margin = if folded {
                    Margin::symmetric(16, 16)
                } else {
                    Margin::symmetric(32, 24)
                };
                egui::Frame::NONE.inner_margin(margin).show(ui, |ui| {
                    let widest = match self.section {
                        Section::Timesheet => SHEET_MAX,
                        _ => PAGE_MAX,
                    };
                    // `set_max_width` sets it: it would widen a narrower page.
                    ui.set_max_width(ui.available_width().min(widest));
                    match self.section {
                        Section::Timesheet => self.sheet.ui(ui, self.store.as_mut(), cx, &theme),
                        Section::Timers => self.timers.ui(ui, self.store.as_mut(), cx, &theme),
                        Section::Tracking => tracking::ui(ui, self.store.as_mut(), cx, &theme),
                        Section::Appearance => appearance::ui(ui, cx, &theme),
                        Section::Viewing => viewing::ui(ui, cx, &theme),
                        Section::Capture => self.capture.ui(ui, cx, &theme),
                        Section::Startup => startup::ui(ui, cx, &theme),
                    }
                });
            });

        // A running timer's time goes on: the timesheet shows its seconds.
        let running = self
            .store
            .as_ref()
            .is_some_and(|s| s.state().running.is_some());
        if running {
            let every = if self.section == Section::Timesheet {
                1
            } else {
                30
            };
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(every));
        }
    }
}

impl SettingsDoc {
    /// The sections down the left, or just their icons.
    fn nav(&mut self, ui: &mut egui::Ui, area: Rect, theme: &Theme) {
        let folded = area.width() < FOLD_BELOW;
        let width = if folded { RAIL_WIDTH } else { NAV_WIDTH };
        let nav = Rect::from_min_size(area.min, vec2(width, area.height()));
        ui.painter().rect_filled(nav, 0.0, theme.strip_bg);
        let inset = if folded {
            vec2(6.0, 10.0)
        } else {
            vec2(10.0, 14.0)
        };
        let mut nav_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(nav.shrink2(inset))
                .layout(Layout::top_down(Align::Min)),
        );
        nav_ui.spacing_mut().item_spacing.y = 2.0;
        if !folded {
            nav_ui.label(RichText::new("Settings").heading().color(theme.text));
            nav_ui.add_space(10.0);
        }
        for (i, sections) in GROUPS.into_iter().enumerate() {
            if i > 0 {
                nav_ui.add_space(6.0);
                let y = nav_ui.cursor().top();
                let y = nav_ui.painter().round_to_pixel_center(y);
                let x = nav_ui.max_rect().x_range();
                let line = Stroke::new(1.0, theme.separator);
                nav_ui.painter().hline(x, y, line);
                nav_ui.add_space(8.0);
            }
            for &s in sections {
                let on = s == self.section;
                if kit::nav_row(&mut nav_ui, s.icon(), s.label(), on, folded, theme) {
                    self.section = s;
                }
            }
        }
    }
}

/// Said in place of a page about time when there's nowhere to keep it.
fn no_folder(ui: &mut egui::Ui, title: &str, theme: &Theme) {
    kit::page_header(ui, title, "", theme, |_| {});
    note(
        ui,
        "The Documents folder isn't available, so there is nowhere to keep time.",
        theme,
    );
}

/// A line of explanation under a heading or a control.
fn note(ui: &mut egui::Ui, text: &str, theme: &Theme) {
    ui.label(RichText::new(text).color(theme.text_dim));
}

/// A time of day as people type it: "9", "9:05", "905", "13:20", "1:20 pm",
/// "9.30". Seconds since midnight.
fn parse_clock(text: &str) -> Option<u32> {
    let t = text.trim().to_ascii_lowercase();
    let (t, pm) = match t
        .strip_suffix("pm")
        .or_else(|| t.strip_suffix('p'))
        .map(|s| (s, Some(true)))
        .or_else(|| {
            t.strip_suffix("am")
                .or_else(|| t.strip_suffix('a'))
                .map(|s| (s, Some(false)))
        }) {
        Some((s, pm)) => (s.trim().to_owned(), pm),
        None => (t, None),
    };
    let (h, m) = match t.split_once([':', '.']) {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None if t.len() >= 3 => {
            let n: u32 = t.parse().ok()?;
            (n / 100, n % 100)
        }
        None => (t.parse().ok()?, 0),
    };
    let h = match pm {
        Some(true) if (1..=11).contains(&h) => h + 12,
        Some(false) if h == 12 => 0,
        Some(_) if !(1..=12).contains(&h) => return None,
        _ => h,
    };
    (h < 24 && m < 60).then_some(h * 3600 + m * 60)
}

/// A length of time as people type it: "1:30", "1.5", "90m", "2h", "1h 15m".
/// Seconds.
fn parse_duration(text: &str) -> Option<i64> {
    let t = text.trim().to_ascii_lowercase().replace(' ', "");
    if t.is_empty() {
        return None;
    }
    let secs = if let Some((h, m)) = t.split_once(':') {
        h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60
    } else if let Some(m) = t.strip_suffix("min").or_else(|| t.strip_suffix('m')) {
        let (h, m) = match m.split_once('h') {
            Some((h, m)) => (h.parse::<f64>().ok()?, m.parse::<f64>().ok()?),
            None => (0.0, m.parse::<f64>().ok()?),
        };
        (h * 3600.0 + m * 60.0).round() as i64
    } else {
        let h = t.strip_suffix('h').unwrap_or(&t);
        (h.parse::<f64>().ok()? * 3600.0).round() as i64
    };
    (secs > 0 && secs <= 24 * 3600).then_some(secs)
}

/// "09:05" (a 24-hour clock, as typed back into [`parse_clock`]).
fn clock_text(secs: u32) -> String {
    if secs >= 24 * 3600 {
        return "24:00".to_owned();
    }
    format!("{:02}:{:02}", secs / 3600, secs / 60 % 60)
}

/// Ask the shell to say something.
fn toast(cx: &mut DocCx<'_>, text: impl Into<String>) {
    cx.requests.push(DocRequest::Toast(text.into()));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use whir_core::time::{Stamp, monday_of};
    use whir_core::{TextureDesc, TextureRegistry, UserTexture, Waker};

    struct NoTextures;

    impl TextureRegistry for NoTextures {
        fn create(
            self: Arc<Self>,
            _: &TextureDesc,
            _: &[u8],
            _: usize,
        ) -> Result<UserTexture, String> {
            Err("no textures in tests".into())
        }

        fn max_texture_side(&self) -> u32 {
            4096
        }

        fn release(&self, _: u64) {}
    }

    /// Frames of the Settings tab, as the shell runs them.
    struct Harness {
        ctx: egui::Context,
        doc: SettingsDoc,
        requests: Vec<DocRequest>,
        time: f64,
        textures: Arc<dyn TextureRegistry>,
    }

    impl Harness {
        fn frame(&mut self) {
            self.time += 0.05;
            let raw = egui::RawInput {
                time: Some(self.time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(1200.0, 800.0),
                )),
                ..Default::default()
            };
            let Self {
                ctx,
                doc,
                requests,
                textures,
                ..
            } = self;
            let waker = Waker::noop();
            let mut out = ctx.run_ui(raw, |ui| {
                let mut cx = DocCx {
                    textures,
                    waker: &waker,
                    just_activated: false,
                    requests,
                };
                doc.ui(ui, &mut cx);
            });
            out.textures_delta.clear();
        }
    }

    #[test]
    fn every_section_draws_from_a_real_timesheet() {
        whir_core::settings::memory_only();
        whir_core::views::memory_only();
        let dir = std::env::temp_dir().join(format!("whir-settings-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(dir.clone());
        let now = Stamp::now();
        let monday = monday_of(now.day);
        let timer = store
            .update(|tx| {
                let acme = tx.state.add_customer("Acme");
                let t = tx.state.add_timer(acme, "Onboarding", "PRJ-1");
                tx.state.add_timer(acme, "Support", "");
                tx.state.add_customer("Nobody yet");
                tx.add(
                    t,
                    Stamp::at(monday, 9, 0, 0),
                    Stamp::at(monday, 11, 0, 0),
                    "kickoff",
                );
                tx.add(
                    t,
                    Stamp::at(monday - 7, 9, 0, 0),
                    Stamp::at(monday - 7, 10, 0, 0),
                    "",
                );
                tx.start(t, now.plus(-600));
                let _ = tx.close_week(monday - 14, now);
                t
            })
            .unwrap();
        let mut h = Harness {
            ctx: whir_core::theme::test_context(),
            doc: SettingsDoc::with_store(Section::Timesheet, Some(store)),
            requests: Vec::new(),
            time: 0.0,
            textures: Arc::new(NoTextures),
        };
        whir_core::theme::Theme::set_current(Theme::light());
        h.frame();
        h.frame();
        // A day's entries, and the close-out question.
        h.doc.sheet.selected = Some((timer, 0));
        h.frame();
        h.doc.show_section("timesheet");
        h.frame();
        h.doc.sheet.confirm_close = true;
        h.frame();
        for section in [
            "timers",
            "tracking",
            "appearance",
            "viewing",
            "capture",
            "startup",
            "timesheet",
        ] {
            h.doc.show_section(section);
            h.frame();
            h.frame();
        }
        // The week before: still open, so this week says so.
        assert!(
            h.doc
                .sheet
                .open_before
                .as_ref()
                .is_some_and(|w| w.contains(&(monday - 7)))
        );
        // Nothing was asked of the shell without a click.
        assert!(h.requests.is_empty(), "{:?}", h.requests);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn times_of_day_are_read_the_way_people_type_them() {
        let hm = |h: u32, m: u32| Some(h * 3600 + m * 60);
        for (text, want) in [
            ("9", hm(9, 0)),
            ("9:05", hm(9, 5)),
            ("09:05", hm(9, 5)),
            ("905", hm(9, 5)),
            ("1320", hm(13, 20)),
            ("13:20", hm(13, 20)),
            ("1:20 pm", hm(13, 20)),
            ("1:20pm", hm(13, 20)),
            ("12:10 am", hm(0, 10)),
            ("12 pm", hm(12, 0)),
            ("9.30", hm(9, 30)),
            ("9a", hm(9, 0)),
            ("0:00", hm(0, 0)),
        ] {
            assert_eq!(parse_clock(text), want, "{text}");
        }
        for bad in ["", "24:00", "9:60", "13 pm", "x", "9:5x", "0 am"] {
            assert_eq!(parse_clock(bad), None, "{bad}");
        }
        assert_eq!(clock_text(9 * 3600 + 5 * 60), "09:05");
        assert_eq!(parse_clock(&clock_text(13 * 3600 + 20 * 60)), hm(13, 20));
    }

    #[test]
    fn durations_are_read_the_way_people_type_them() {
        for (text, want) in [
            ("1:30", 5400),
            ("1.5", 5400),
            ("1.25h", 4500),
            ("90m", 5400),
            ("90 min", 5400),
            ("2h", 7200),
            ("1h 15m", 4500),
            ("0:15", 900),
        ] {
            assert_eq!(parse_duration(text), Some(want), "{text}");
        }
        for bad in ["", "0", "-1", "25", "x", "1:xx"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    #[test]
    fn sections_have_names() {
        assert_eq!(Section::all().count(), 7);
        for section in Section::all() {
            assert_eq!(Section::from_name(section.name()), Some(section));
        }
        assert_eq!(Section::from_name("dock"), Some(Section::Capture));
        assert_eq!(Section::from_name("TIMESHEET"), Some(Section::Timesheet));
        assert_eq!(Section::from_name("nope"), None);
    }
}
