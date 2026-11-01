//! Pictures of the real interface, to look at while designing it.
//!
//! `cargo test -p whir -- --ignored gallery` drives the whole `App` on made-up
//! files through an offscreen software renderer, and writes each scene, in both
//! themes at 100% and 150%, to PNGs in `WHIR_PREVIEW_DIR`. Narrow it down with
//! `WHIR_PREVIEW_ONLY=pdf,csv` (scene names containing any of those) and
//! `WHIR_PREVIEW_MODES=dark-100,light-150`.
//!
//! Everything it opens or writes is under `<WHIR_PREVIEW_DIR>/.fixtures`: the
//! notes, screenshots, timesheet and `%LOCALAPPDATA%` are all pointed there,
//! settings stay in memory, and no capture dock is started.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2, ViewportId, pos2};
use whir_core::Waker;
use whir_core::settings;
use whir_core::time::{Stamp, monday_of};
use whir_core::timesheet::{Grid, Store};
use whir_gfx::{Device, GpuTextures, Renderer};
use whir_image::{PixelLayout, encode_png};
use whir_platform::{FrameInfo, FrameOutput, Handler as _};

use crate::app::App;

const WINDOW: Vec2 = Vec2::new(1280.0, 800.0);
const CTRL: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: false,
    mac_cmd: false,
    command: true,
};
const CTRL_SHIFT: Modifiers = Modifiers {
    shift: true,
    ..CTRL
};
/// Where a note's text is, in a [`WINDOW`]: a click there gives it the keyboard.
const IN_THE_NOTE: Pos2 = pos2(500.0, 200.0);

/// Every picture a design pass should look at.
const SCENES: &[Scene] = &[
    Scene::new("note", "notes", |s, f| s.open(&[&f.note])),
    Scene::new("note-images", "shots", |s, f| s.open(&[&f.note])),
    Scene::new("pdf", "notes", |s, f| s.open(&[&f.pdf])),
    Scene::new("pdf-find", "notes", |s, f| {
        s.open(&[&f.pdf]);
        s.settle();
        s.key(CTRL, Key::F);
        s.text("rack");
    }),
    Scene::new("image", "shots", |s, f| s.open(&[&f.shots[0]])),
    Scene::new("csv", "notes", |s, f| s.open(&[&f.csv])),
    Scene::new("csv-find", "notes", |s, f| {
        s.open(&[&f.csv]);
        s.settle();
        s.key(CTRL, Key::F);
        s.text("Linden");
    }),
    Scene::new("csv-menu", "notes", |s, f| {
        s.open(&[&f.csv]);
        s.settle();
        s.click(pos2(320.0, 180.0), PointerButton::Secondary);
    }),
    Scene::new("csv-unsaved", "notes", |s, f| {
        s.open(&[&f.csv]);
        s.settle();
        s.click(pos2(320.0, 180.0), PointerButton::Primary);
        s.text("Edited");
        s.key(Modifiers::NONE, Key::Enter);
        s.key(CTRL, Key::W);
    }),
    Scene::new("search-tabs", "notes", |s, f| {
        s.open(&[&f.csv, &f.csv_2]);
        s.settle();
        s.key(CTRL_SHIFT, Key::F);
        s.text("Linden");
        s.key(Modifiers::NONE, Key::Enter);
    }),
    Scene::new("settings-timesheet", "notes", |s, _| {
        s.app.open_settings("timesheet")
    })
    .fluid(),
    Scene::new("settings-milestones", "notes", |s, _| {
        s.app.open_settings("milestones")
    })
    .fluid(),
    Scene::new("settings-tracking", "notes", |s, _| {
        s.app.open_settings("tracking")
    })
    .fluid(),
    Scene::new("settings-appearance", "notes", |s, _| {
        s.app.open_settings("appearance")
    })
    .fluid(),
    Scene::new("settings-pdfs", "notes", |s, _| s.app.open_settings("pdfs")).fluid(),
    Scene::new("settings-capture", "notes", |s, _| {
        s.app.open_settings("capture")
    })
    .fluid(),
    Scene::new("settings-startup", "notes", |s, _| {
        s.app.open_settings("startup")
    })
    .fluid(),
    Scene::new("settings-milestones-menu", "notes", |s, _| {
        s.app.open_settings("milestones");
        s.settle();
        s.click(pos2(929.0, 239.0), PointerButton::Primary);
    }),
    Scene::new("settings-milestones-adding", "notes", |s, _| {
        s.app.open_settings("milestones");
        s.settle();
        s.click(pos2(871.0, 291.0), PointerButton::Primary);
        s.settle();
        s.text("Rail contract");
    }),
    Scene::new("settings-milestones-archived", "notes", |s, _| {
        s.app.open_settings("milestones");
        s.settle();
        s.click(pos2(292.0, 492.0), PointerButton::Primary);
    }),
    Scene::new("settings-timesheet-adder", "notes", |s, _| {
        s.app.open_settings("timesheet");
        s.settle();
        s.click(pos2(295.0, 332.0), PointerButton::Primary);
        s.settle();
        s.text("sch");
    }),
    Scene::new("settings-timesheet-new-milestone", "notes", |s, _| {
        s.app.open_settings("timesheet");
        s.settle();
        s.click(pos2(295.0, 332.0), PointerButton::Primary);
        s.settle();
        s.text("zzz");
        s.settle();
        s.click(pos2(315.0, 430.0), PointerButton::Primary);
        s.settle();
        s.text("Rail contract");
    }),
    Scene::new("settings-timesheet-entries", "notes", |s, _| {
        s.app.open_settings("timesheet");
        s.settle();
        s.click(pos2(700.0, 227.0), PointerButton::Primary);
    }),
    Scene::new("palette", "notes", |s, f| {
        s.open(&[&f.note]);
        s.settle();
        s.key(CTRL_SHIFT, Key::P);
    }),
    Scene::new("palette-narrow", "notes", |s, f| {
        s.resize(Vec2::new(480.0, 640.0));
        s.open(&[&f.note]);
        s.settle();
        s.key(CTRL_SHIFT, Key::P);
    }),
    Scene::new("palette-pdf", "notes", |s, f| {
        s.open(&[&f.pdf]);
        s.settle();
        s.key(CTRL_SHIFT, Key::P);
        s.settle();
        s.text("fit");
    }),
    Scene::new("palette-found", "notes", |s, f| {
        s.open(&[&f.csv]);
        s.settle();
        s.key(CTRL_SHIFT, Key::P);
        s.settle();
        s.text("note");
        s.settle();
        s.key(Modifiers::NONE, Key::ArrowDown);
    }),
    Scene::new("menu-narrow", "notes", |s, f| {
        s.resize(Vec2::new(480.0, 640.0));
        s.open(&[&f.note]);
        s.settle();
        s.click(pos2(23.0, 54.0), PointerButton::Primary);
    }),
    Scene::new("menu-view-theme", "notes", |s, f| {
        s.open(&[&f.note]);
        s.settle();
        s.click(pos2(101.0, 54.0), PointerButton::Primary);
        s.settle();
        s.hover(pos2(135.0, 105.0));
        s.wait(0.4);
    }),
    Scene::new("tabs-many", "notes", |s, f| {
        let many: Vec<&Path> = f.notes.iter().map(PathBuf::as_path).collect();
        s.open(&many);
        s.open(&[&f.pdf, &f.csv, &f.csv_2, &f.shots[1]]);
    }),
    Scene::new("tabs-overflow-menu", "notes", |s, f| {
        let many: Vec<&Path> = f.notes.iter().map(PathBuf::as_path).collect();
        s.open(&many);
        s.open(&[&f.pdf, &f.csv, &f.csv_2, &f.shots[1]]);
        s.settle();
        s.click(pos2(1046.0, 22.0), PointerButton::Primary);
    }),
    Scene::new("narrow-pdf", "notes", |s, f| {
        s.resize(Vec2::new(520.0, 640.0));
        s.open(&[&f.pdf]);
    }),
    Scene::new("narrow-note", "notes", |s, f| {
        s.resize(Vec2::new(520.0, 640.0));
        s.open(&[&f.note]);
    }),
    Scene::new("tab-menu", "notes", |s, f| {
        s.open(&[&f.note, &f.csv]);
        s.settle();
        s.click(pos2(90.0, 22.0), PointerButton::Secondary);
    }),
    Scene::new("new-menu", "notes", |s, f| {
        s.open(&[&f.csv]);
        s.settle();
        s.click(pos2(255.0, 22.0), PointerButton::Secondary);
    }),
    Scene::new("menu-file", "notes", |s, f| {
        s.open(&[&f.note]);
        s.settle();
        s.click(pos2(24.0, 54.0), PointerButton::Primary);
    }),
    Scene::new("menu-edit-note", "notes", |s, f| {
        s.open(&[&f.note]);
        s.settle();
        s.click(pos2(61.0, 54.0), PointerButton::Primary);
    }),
    Scene::new("menu-view-pdf", "notes", |s, f| {
        s.open(&[&f.pdf]);
        s.settle();
        s.click(pos2(101.0, 54.0), PointerButton::Primary);
    }),
    Scene::new("menu-tools", "notes", |s, f| {
        s.open(&[&f.csv]);
        s.settle();
        s.click(pos2(145.0, 54.0), PointerButton::Primary);
    }),
    Scene::new("note-bold", "notes", |s, f| {
        s.open(&[&f.note]);
        s.settle();
        s.click(pos2(500.0, 200.0), PointerButton::Primary);
        s.settle();
        s.key(CTRL, Key::A);
        s.settle();
        s.click(pos2(255.0, 54.0), PointerButton::Primary);
    }),
    Scene::new("note-bold-undone", "notes", |s, f| {
        s.open(&[&f.note]);
        s.settle();
        s.click(pos2(500.0, 200.0), PointerButton::Primary);
        s.settle();
        s.key(CTRL, Key::A);
        s.settle();
        s.click(pos2(255.0, 54.0), PointerButton::Primary);
        s.settle();
        s.click(pos2(427.0, 54.0), PointerButton::Primary);
    }),
    Scene::new("pdf-zoomed", "notes", |s, f| {
        s.open(&[&f.pdf]);
        s.settle();
        s.click(pos2(488.0, 54.0), PointerButton::Primary);
        s.settle();
        s.click(pos2(559.0, 54.0), PointerButton::Primary);
    }),
    Scene::new("csv-sorted", "notes", |s, f| {
        s.open(&[&f.csv]);
        s.settle();
        s.click(pos2(425.0, 86.0), PointerButton::Primary);
    }),
    Scene::new("about", "notes", |s, f| {
        s.open(&[&f.note]);
        s.settle();
        s.click(pos2(190.0, 54.0), PointerButton::Primary);
        s.settle();
        s.click(pos2(214.0, 102.0), PointerButton::Primary);
    }),
    Scene::new("tooltip", "notes", |s, f| {
        s.open(&[&f.note]);
        s.settle();
        s.hover(pos2(255.0, 54.0));
        s.wait(0.6);
    }),
];

/// Widths a page has to cope with, besides [`WINDOW`]'s.
const NARROWER: &[f32] = &[900.0, 640.0, 480.0];

struct Scene {
    name: &'static str,
    /// The drawer tab it opens on.
    drawer: &'static str,
    setup: fn(&mut Stage, &Fixtures),
    /// Shown again at each of these.
    widths: &'static [f32],
}

impl Scene {
    const fn new(
        name: &'static str,
        drawer: &'static str,
        setup: fn(&mut Stage, &Fixtures),
    ) -> Self {
        Self {
            name,
            drawer,
            setup,
            widths: &[],
        }
    }

    /// Shown at [`NARROWER`] too.
    const fn fluid(self) -> Self {
        Self {
            widths: NARROWER,
            ..self
        }
    }
}

/// One `App`, drawing to memory, with input played into it.
struct Stage {
    app: App,
    points: Vec2,
    ppp: f32,
    time: f64,
    woken: Arc<AtomicBool>,
}

impl Stage {
    fn new(dark: bool, ppp: f32) -> Self {
        let textures = GpuTextures::new();
        let woken = Arc::new(AtomicBool::new(false));
        let flag = woken.clone();
        let waker = Waker::new(move || flag.store(true, Ordering::Relaxed));
        let restart = Rc::new(RefCell::new(None));
        let mut app = App::new(textures.clone(), waker, dark, restart);
        let device = Device::create(true).expect("software renderer");
        let renderer =
            Renderer::offscreen(device, textures, size_px(WINDOW, ppp)).expect("offscreen");
        app.set_renderer(renderer);
        Self {
            app,
            points: WINDOW,
            ppp,
            time: 0.0,
            woken,
        }
    }

    fn resize(&mut self, points: Vec2) {
        self.points = points;
        self.app.resized(size_px(points, self.ppp));
    }

    fn open(&mut self, paths: &[&Path]) {
        self.app
            .open_files(paths.iter().map(|p| p.to_path_buf()).collect());
    }

    fn frame(&mut self, events: Vec<Event>) -> FrameOutput {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.points)),
            time: Some(self.time),
            events,
            focused: true,
            ..Default::default()
        };
        if let Some(viewport) = raw.viewports.get_mut(&ViewportId::ROOT) {
            viewport.native_pixels_per_point = Some(self.ppp);
            viewport.focused = Some(true);
        }
        let info = FrameInfo {
            size_px: size_px(self.points, self.ppp),
            pixels_per_point: self.ppp,
            maximized: false,
            minimized: false,
            in_size_move: false,
        };
        self.time += 1.0 / 60.0;
        self.app.frame(raw, &info)
    }

    /// Frames until nothing has happened for a while: files loaded, thumbnails
    /// back from their threads, popups measured.
    fn settle(&mut self) {
        let mut quiet = 0;
        for _ in 0..500 {
            let woken = self.woken.swap(false, Ordering::Relaxed);
            let out = self.frame(Vec::new());
            let soon = out
                .repaint_after
                .is_some_and(|d| d < Duration::from_millis(100));
            quiet = if woken || soon || self.app.loading() {
                0
            } else {
                quiet + 1
            };
            if quiet >= 15 {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// Let `seconds` of the app's own time go by (a tooltip's delay).
    fn wait(&mut self, seconds: f64) {
        for _ in 0..(seconds * 60.0).ceil() as usize {
            self.frame(Vec::new());
        }
    }

    fn key(&mut self, modifiers: Modifiers, key: Key) {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        };
        self.frame(vec![event(true)]);
        self.frame(vec![event(false)]);
    }

    fn text(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.to_owned())]);
    }

    fn hover(&mut self, at: Pos2) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(Vec::new());
    }

    fn click(&mut self, at: Pos2, button: PointerButton) {
        self.hover(at);
        let event = |pressed| Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![event(true)]);
        self.frame(vec![event(false)]);
    }

    /// Ctrl+Shift+P, some typing, and Enter.
    fn palette(&mut self, query: &str) {
        self.key(CTRL_SHIFT, Key::P);
        self.settle();
        self.text(query);
        self.settle();
        self.key(Modifiers::NONE, Key::Enter);
    }

    /// What's on the screen, once it has settled.
    fn pixels(&mut self) -> Vec<u8> {
        self.settle();
        // The frame after a settle: one that has taken in everything before it.
        self.frame(Vec::new());
        self.app.read_pixels().expect("offscreen renderer")
    }

    fn shoot(&mut self, path: &Path) {
        self.settle();
        let pixels = self.app.read_pixels().expect("offscreen renderer");
        let [width, height] = size_px(self.points, self.ppp);
        let png = encode_png(
            &pixels,
            width,
            height,
            width as usize * 4,
            PixelLayout::Bgrx,
        )
        .expect("encode");
        std::fs::write(path, png).expect("write picture");
    }
}

fn size_px(points: Vec2, ppp: f32) -> [u32; 2] {
    [(points.x * ppp) as u32, (points.y * ppp) as u32]
}

/// The made-up files the scenes open.
struct Fixtures {
    note: PathBuf,
    notes: Vec<PathBuf>,
    pdf: PathBuf,
    csv: PathBuf,
    csv_2: PathBuf,
    shots: Vec<PathBuf>,
}

impl Fixtures {
    fn write(root: &Path) -> Self {
        let _ = std::fs::remove_dir_all(root);
        let (notes_dir, shots_dir, time_dir, app_data, files) = (
            root.join("notes"),
            root.join("shots"),
            root.join("time"),
            root.join("appdata"),
            root.join("files"),
        );
        for dir in [&notes_dir, &shots_dir, &time_dir, &app_data, &files] {
            std::fs::create_dir_all(dir).expect("fixture folder");
        }
        // The whole test process is this one test, on this thread.
        unsafe {
            std::env::set_var("WHIR_NOTES_DIR", &notes_dir);
            std::env::set_var("WHIR_CAPTURES_DIR", &shots_dir);
            std::env::set_var("WHIR_TIME_DIR", &time_dir);
            std::env::set_var("LOCALAPPDATA", &app_data);
            std::env::set_var("WHIR_NO_DOCK", "1");
        }

        let note = notes_dir.join("Rack review.md");
        std::fs::write(&note, NOTE).expect("note");
        let mut notes = vec![note.clone()];
        for (i, title) in NOTE_TITLES.iter().enumerate() {
            let path = notes_dir.join(format!("{title}.md"));
            std::fs::write(&path, format!("# {title}\n\nNote {i}.\n")).expect("note");
            notes.push(path);
        }

        let pdf = files.join("Quarterly rack report.pdf");
        std::fs::write(&pdf, report_pdf(12)).expect("pdf");
        let csv = files.join("rack-prices.csv");
        std::fs::write(&csv, rack_csv(400, 0)).expect("csv");
        let csv_2 = files.join("rack-prices-prior.csv");
        std::fs::write(&csv_2, rack_csv(400, 7)).expect("csv");

        let shots = (0..8)
            .map(|i| {
                let path = shots_dir.join(format!("2026-10-{:02} 09-1{i}-00.png", 20 + i));
                std::fs::write(&path, screenshot(i)).expect("screenshot");
                path
            })
            .collect();
        timesheet(time_dir);
        Self {
            note,
            notes,
            pdf,
            csv,
            csv_2,
            shots,
        }
    }
}

const NOTE_TITLES: &[&str] = &[
    "Supplier call notes",
    "Freight quote follow up",
    "Terminal outage plan",
    "Hedge questions",
    "October invoices",
    "Tax rate changes",
    "Onboarding checklist",
    "Meeting with Dana",
    "Pricing model ideas",
    "Year end close",
    "Contract renewals",
];

const NOTE: &str = "# Rack review

Prices came in **higher than the prior week** on every terminal except Linden, where
the new supplier's discount offsets *most* of the freight increase. See the
[weekly summary](https://example.com/summary) for the detail.

## To follow up

- [x] Confirm the October freight table
- [ ] Ask Dana about the Linden discount
- [ ] Reconcile the tax rate change
  - Check the state filing
  - Update the delivered price sheet

## Numbers

1. Rack average: `2.4500`
2. Delivered average: `2.8125`

> A one cent move on rack is roughly four thousand dollars a month at current volumes.

```
delivered = rack + freight + tax
```

---

Next review is on Friday.
";

/// A report: a title, a rule, paragraphs and a shaded table on each page.
fn report_pdf(pages: usize) -> Vec<u8> {
    let kids: String = (0..pages).map(|k| format!("{} 0 R ", 5 + 2 * k)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>"),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_string(),
    ];
    for page in 0..pages {
        let mut content = format!(
            "BT /F2 22 Tf 72 708 Td (Quarterly rack report: section {}) Tj ET\n\
             0.75 G 72 696 m 540 696 l S\nBT /F1 11 Tf 15 TL 72 672 Td\n",
            page + 1
        );
        for line in PARAGRAPH {
            let _ = writeln!(content, "({line}) Tj T*");
        }
        content.push_str("ET\n");
        for row in 0..9 {
            let y = 500 - row * 22;
            if row % 2 == 0 {
                let _ = writeln!(content, "0.93 g 72 {} 468 22 re f 0 g", y - 6);
            }
            let _ = writeln!(
                content,
                "BT /F1 10 Tf 80 {y} Td (Terminal {}) Tj 200 0 Td ({}.{:02}) Tj ET",
                row + 1,
                2 + row % 2,
                (row * 7 + page as i32 * 3) % 100
            );
        }
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
             /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> >>",
            6 + 2 * page
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    let size = objects.len() + 1;
    out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").bytes());
    }
    out.extend(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").bytes(),
    );
    out
}

const PARAGRAPH: &[&str] = &[
    "Average rack prices rose in every region this quarter, led by the northern terminals",
    "where refinery maintenance tightened supply through the first half of the period.",
    "Delivered costs followed, although the freight table held steady after September.",
    "",
    "The table below lists the closing rack price at each terminal. Prices are in dollars",
    "per gallon before tax and freight, and include any negotiated supplier discount.",
];

fn rack_csv(rows: usize, seed: usize) -> String {
    const TERMINALS: &[&str] = &["Linden", "Newark", "Albany", "Selkirk", "Rensselaer"];
    const PRODUCTS: &[&str] = &["ULSD", "RBOB 87", "RBOB 93", "Heating oil", "Kerosene"];
    const SUPPLIERS: &[&str] = &["Northwind", "Tailspin", "Contoso", "Fabrikam"];
    let mut out = String::from(
        "Date,Terminal,Product,Supplier,Rack price,Freight,Tax,Delivered,Gallons,Notes\n",
    );
    for row in 0..rows {
        let n = row * 37 + seed * 11;
        let rack = 2.1 + (n % 90) as f64 / 100.0;
        let freight = 0.08 + (n % 13) as f64 / 100.0;
        let tax = 0.1865;
        let _ = writeln!(
            out,
            "2026-10-{:02},{},{},{},{rack:.4},{freight:.4},{tax:.4},{:.4},{},{}",
            row % 28 + 1,
            TERMINALS[n % TERMINALS.len()],
            PRODUCTS[(n / 3) % PRODUCTS.len()],
            SUPPLIERS[(n / 7) % SUPPLIERS.len()],
            rack + freight + tax,
            7_500 + n % 900,
            if row % 9 == 0 {
                "\"Spot, confirm\""
            } else {
                ""
            },
        );
    }
    out
}

/// A made-up window, so the image view and thumbnails have something to show.
fn screenshot(variant: usize) -> Vec<u8> {
    const SIZE: (usize, usize) = (1600, 1000);
    let hue = variant * 40 % 200;
    let mut pixels = vec![0xffu8; SIZE.0 * SIZE.1 * 4];
    for (i, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i % SIZE.0, i / SIZE.0);
        let (b, g, r) = match (x, y) {
            (_, 0..=59) => (0x38 + hue as u8 / 4, 0x30, 0x2c),
            (0..=239, _) => (0xee, 0xec, 0xea),
            _ if (y - 60) % 44 < 2 => (0xdd, 0xdd, 0xdd),
            _ if x % 300 < 240 && (y - 60) % 44 > 16 && (y - 60) % 44 < 28 => {
                (0x70, 0x60 + hue as u8 / 3, 0x50)
            }
            _ => (0xff, 0xff, 0xff),
        };
        pixel[0] = b;
        pixel[1] = g;
        pixel[2] = r;
    }
    encode_png(
        &pixels,
        SIZE.0 as u32,
        SIZE.1 as u32,
        SIZE.0 * 4,
        PixelLayout::Bgrx,
    )
    .expect("encode")
}

/// A week with projects, milestones and hours in it, and what a page has to
/// cope with: long names, no code, a project with no milestones, an archived
/// milestone, a week already closed.
fn timesheet(dir: PathBuf) {
    let now = Stamp::now();
    let monday = monday_of(now.day);
    let mut store = Store::open(dir);
    store
        .update(|tx| {
            let northwind = tx.state.add_project("Northwind Fuels");
            let tailspin = tx.state.add_project("Tailspin Logistics");
            let contoso = tx
                .state
                .add_project("Contoso Midstream Partners of the Northeast");
            tx.state.add_project("Fabrikam");
            let milestones = [
                tx.state.add_milestone(northwind, "Rack audit", "NW-104"),
                tx.state.add_milestone(northwind, "Pricing model", "NW-118"),
                tx.state.add_milestone(tailspin, "Freight review", "TS-022"),
                tx.state.add_milestone(
                    contoso,
                    "Terminal automation rollout, phase two and beyond",
                    "CMP-2291",
                ),
            ];
            tx.state.add_milestone(tailspin, "Driver scheduling", "");
            let old = tx
                .state
                .add_milestone(northwind, "Q2 reconciliation", "NW-087");
            if let Some(t) = tx.state.milestone_mut(old) {
                t.archived = true;
            }
            for (t, milestone) in milestones.iter().enumerate() {
                for day in 0..5 {
                    let hours = 1 + (t as i64 + day) % 4;
                    tx.set_day(*milestone, monday + day, hours * 3600);
                }
                tx.set_day(*milestone, monday - 14, 2 * 3600);
            }
            tx.set_day(old, monday - 14, 3 * 3600);
            tx.close_week(monday - 14, now).expect("closed week");
        })
        .expect("timesheet");
}

#[test]
#[ignore = "writes pictures to look at"]
fn gallery() {
    let Some(dir) = std::env::var_os("WHIR_PREVIEW_DIR").map(PathBuf::from) else {
        return;
    };
    let wanted = |variable: &str, name: &str| match std::env::var(variable) {
        Ok(list) => list.split(',').any(|want| name.contains(want.trim())),
        Err(_) => true,
    };
    unsafe {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let fixtures = Fixtures::write(&dir.join(".fixtures"));
    for scene in SCENES {
        let widths = std::iter::once(None).chain(scene.widths.iter().copied().map(Some));
        for width in widths {
            let name = match width {
                Some(width) => format!("{}-{width}", scene.name),
                None => scene.name.to_owned(),
            };
            if !wanted("WHIR_PREVIEW_ONLY", &name) {
                continue;
            }
            for (theme, dark) in [("dark", true), ("light", false)] {
                for (percent, ppp) in [(100, 1.0), (150, 1.5)] {
                    let mode = format!("{theme}-{percent}");
                    if !wanted("WHIR_PREVIEW_MODES", &mode) {
                        continue;
                    }
                    // A fresh start each time: nothing carries over from the last scene.
                    settings::memory_only();
                    settings::set_str("drawer.tab", scene.drawer);
                    let mut stage = Stage::new(dark, ppp);
                    if let Some(width) = width {
                        stage.resize(Vec2::new(width, WINDOW.y));
                    }
                    (scene.setup)(&mut stage, &fixtures);
                    stage.shoot(&dir.join(format!("{name}-{mode}.png")));
                }
            }
        }
    }
}

/// The made-up files, for a test that drives the whole app. These run one at a
/// time (`--test-threads=1`) and want `WHIR_PREVIEW_DIR`, as the gallery does.
fn live() -> Option<Fixtures> {
    let dir = PathBuf::from(std::env::var_os("WHIR_PREVIEW_DIR")?);
    unsafe {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    settings::memory_only();
    Some(Fixtures::write(&dir.join(".fixtures-live")))
}

#[test]
#[ignore = "drives the whole app"]
fn what_is_typed_in_the_palette_stays_out_of_the_document() {
    let Some(f) = live() else { return };
    let mut s = Stage::new(true, 1.0);
    s.open(&[&f.note]);
    s.settle();
    s.click(IN_THE_NOTE, PointerButton::Primary);
    s.settle();
    let before = s.app.active_tab();
    let keyboard = s.app.focused();
    assert!(keyboard.is_some(), "the click gave the note the keyboard");

    s.key(CTRL_SHIFT, Key::P);
    s.settle();
    assert!(s.app.palette_is_open());
    assert_ne!(s.app.focused(), keyboard, "the palette has the keyboard");
    s.text("three more words");
    s.key(Modifiers::NONE, Key::Backspace);
    s.key(CTRL, Key::A);
    s.settle();
    assert_eq!(
        s.app.active_tab(),
        before,
        "the note took keys from the palette"
    );

    // Esc goes back to where it was, with the keyboard.
    s.key(Modifiers::NONE, Key::Escape);
    s.settle();
    assert!(!s.app.palette_is_open());
    assert_eq!(s.app.active_tab(), before, "Esc reached the note");
    assert_eq!(s.app.focused(), keyboard, "the note lost the keyboard");
    s.text("three more words ");
    s.settle();
    assert_ne!(
        s.app.active_tab().1,
        before.1,
        "typing didn't reach the note"
    );
}

#[test]
#[ignore = "drives the whole app"]
fn the_palette_runs_a_documents_own_command_on_its_selection() {
    let Some(f) = live() else { return };
    let mut s = Stage::new(true, 1.0);
    s.open(&[&f.note]);
    s.settle();
    s.click(IN_THE_NOTE, PointerButton::Primary);
    s.settle();
    s.key(CTRL, Key::A);
    s.settle();
    assert!(!s.app.active_tab().2, "selecting isn't a change");
    s.palette("bold");
    s.frame(Vec::new());
    s.frame(Vec::new());
    assert!(!s.app.palette_is_open());
    assert!(s.app.active_tab().2, "Bold didn't reach the note");
}

#[test]
#[ignore = "drives the whole app"]
fn the_palette_runs_a_command_once() {
    let Some(f) = live() else { return };
    let mut s = Stage::new(true, 1.0);
    s.open(&[&f.csv]);
    s.settle();
    let tabs = s.app.tab_count();
    s.palette("new table");
    s.settle();
    assert_eq!(s.app.tab_count(), tabs + 1);
    // And it's first in the list now, with nothing typed.
    s.key(CTRL_SHIFT, Key::P);
    s.settle();
    s.key(Modifiers::NONE, Key::Enter);
    s.settle();
    assert_eq!(s.app.tab_count(), tabs + 2);
}

#[test]
#[ignore = "drives the whole app"]
fn the_palette_opens_notes_and_settings() {
    let Some(f) = live() else { return };
    let mut s = Stage::new(true, 1.0);
    s.open(&[&f.csv]);
    // The notes are read once the window is up.
    s.settle();
    s.wait(0.2);
    s.settle();
    s.palette("hedge");
    s.settle();
    assert_eq!(s.app.active_tab().0, "Hedge questions");
    s.palette("appearance");
    s.settle();
    assert_eq!(s.app.active_tab().0, "Settings");
    // A setting's page, by a word that's on it.
    s.palette("minimap");
    s.settle();
    assert_eq!(s.app.active_tab().0, "Settings");
}

#[test]
#[ignore = "drives the whole app"]
fn a_change_of_look_leaves_nothing_of_the_old_one() {
    use whir_core::theme::{Mode, NoteSize};
    let Some(f) = live() else { return };
    let shots = f.shots[0].clone();
    let documents: [(&str, Option<&Path>); 5] = [
        ("note", Some(&f.note)),
        ("table", Some(&f.csv)),
        ("pdf", Some(&f.pdf)),
        ("image", Some(&shots)),
        ("settings", None),
    ];
    let mut wrong = Vec::new();
    for (name, path) in documents {
        let open = |s: &mut Stage| match path {
            Some(path) => s.open(&[path]),
            None => s.app.open_settings("milestones"),
        };
        for (from_dark, mode) in [(true, Mode::Light), (false, Mode::Dark)] {
            // Changed while it's showing.
            settings::memory_only();
            let mut changed = Stage::new(from_dark, 1.0);
            open(&mut changed);
            changed.settle();
            changed.app.change_look(mode, NoteSize::Medium);
            let after = changed.pixels();

            // As though it had always been so.
            settings::memory_only();
            mode.save(|| {});
            let mut fresh = Stage::new(!from_dark, 1.0);
            open(&mut fresh);
            let want = fresh.pixels();

            assert_eq!(after.len(), want.len());
            let off = after
                .chunks_exact(4)
                .zip(want.chunks_exact(4))
                .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 2))
                .count();
            println!("{name}, to {mode:?}: {off} pixels differ");
            if off > 0 {
                wrong.push(format!("{name} to {mode:?}: {off}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:?}");
}

#[test]
#[ignore = "drives the whole app"]
fn a_note_takes_a_new_text_size_while_it_is_open() {
    use whir_core::theme::{Mode, NoteSize};
    let Some(f) = live() else { return };
    let mut s = Stage::new(true, 1.0);
    s.open(&[&f.note]);
    let medium = s.pixels();
    s.app.change_look(Mode::Dark, NoteSize::Large);
    let large = s.pixels();
    assert_ne!(medium, large, "the note didn't change size");

    settings::memory_only();
    Mode::Dark.save(|| {});
    NoteSize::Large.save();
    let mut fresh = Stage::new(true, 1.0);
    fresh.open(&[&f.note]);
    let want = fresh.pixels();
    let off = large
        .chunks_exact(4)
        .zip(want.chunks_exact(4))
        .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 2))
        .count();
    assert_eq!(off, 0, "pixels differ from a note opened at that size");
}

/// The timesheet as it is on disk.
fn on_disk() -> Store {
    let mut store = Store::open(whir_core::timesheet::resolve_dir().expect("a time folder"));
    store.refresh();
    store
}

#[test]
#[ignore = "drives the whole app"]
fn hours_are_typed_along_a_row_with_the_keyboard() {
    let Some(_f) = live() else { return };
    let store = on_disk();
    let milestone = store
        .state()
        .milestones
        .iter()
        .find(|t| t.name == "Pricing model")
        .expect("the fixture's milestone")
        .id;
    let monday = monday_of(Stamp::now().day);
    let cell = |d: usize| {
        let mut store = on_disk();
        let week = store.week(monday).clone();
        Grid::of(&week, None, Stamp::now()).cell(milestone, d)
    };
    let (mon, tue, wed) = (cell(0), cell(1), cell(2));
    assert_ne!(mon, 12600);

    let mut s = Stage::new(true, 1.0);
    s.app.open_settings("timesheet");
    s.settle();
    // Pricing model's Monday: typing replaces what it showed.
    s.click(pos2(680.0, 227.0), PointerButton::Primary);
    s.settle();
    s.text("3.5");
    // Tab puts it down and goes on to Tuesday.
    s.key(Modifiers::NONE, Key::Tab);
    s.settle();
    assert_eq!(cell(0), 12600, "Tab put Monday's hours down");
    s.text("1.25");
    s.key(Modifiers::NONE, Key::Enter);
    s.settle();
    assert_eq!(cell(1), 4500, "Enter put Tuesday's hours down");

    // Esc leaves a cell as it was.
    s.click(pos2(820.0, 227.0), PointerButton::Primary);
    s.settle();
    s.text("9");
    s.key(Modifiers::NONE, Key::Escape);
    s.settle();
    assert_eq!(cell(2), wed, "Esc left Wednesday alone");

    // Shift+Tab goes back along the row.
    s.click(pos2(820.0, 227.0), PointerButton::Primary);
    s.settle();
    s.text("2");
    s.key(Modifiers::SHIFT, Key::Tab);
    s.settle();
    assert_eq!(cell(2), 7200, "Shift+Tab put Wednesday's hours down");
    s.text("4");
    s.key(Modifiers::NONE, Key::Enter);
    s.settle();
    assert_eq!(cell(1), 14400, "and went back to Tuesday");
    assert_ne!(tue, 14400);

    // A row stays where it is while a cell in it is being typed in: the next
    // one is still under the same click.
    s.click(pos2(680.0, 227.0), PointerButton::Primary);
    s.settle();
    s.click(pos2(680.0, 261.0), PointerButton::Primary);
    s.settle();
    s.text("6");
    s.key(Modifiers::NONE, Key::Enter);
    s.settle();
    let rack = on_disk()
        .state()
        .milestones
        .iter()
        .find(|t| t.name == "Rack audit")
        .expect("the fixture's milestone")
        .id;
    let mut store = on_disk();
    let week = store.week(monday).clone();
    assert_eq!(Grid::of(&week, None, Stamp::now()).cell(rack, 0), 21600);
    assert_eq!(cell(0), 12600, "the first cell kept its hours");
    let _ = mon;
}

#[test]
#[ignore = "drives the whole app"]
fn projects_and_milestones_are_changed_where_they_stand() {
    let Some(_f) = live() else { return };
    let mut s = Stage::new(true, 1.0);
    s.app.open_settings("milestones");
    s.settle();
    let project = |name: &str| {
        let store = on_disk();
        store
            .state()
            .projects
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.id)
    };

    // A new project: named, and Enter.
    s.click(pos2(900.0, 111.0), PointerButton::Primary);
    s.settle();
    s.text("Aardvark Oil");
    s.key(Modifiers::NONE, Key::Enter);
    s.settle();
    assert!(project("Aardvark Oil").is_some(), "Enter added the project");

    // Esc doesn't add one.
    s.click(pos2(900.0, 111.0), PointerButton::Primary);
    s.settle();
    s.text("Never mind");
    s.key(Modifiers::NONE, Key::Escape);
    s.settle();
    assert_eq!(project("Never mind"), None, "Esc called it off");

    // Its card comes first, with its + Milestone: name one, with Enter.
    s.click(pos2(871.0, 203.0), PointerButton::Primary);
    s.settle();
    s.text("Pipeline survey");
    s.key(Modifiers::NONE, Key::Enter);
    s.settle();
    let aardvark = project("Aardvark Oil").expect("the new project");
    let store = on_disk();
    let added = store
        .state()
        .milestones
        .iter()
        .find(|t| t.name == "Pipeline survey");
    assert_eq!(added.map(|t| t.project), Some(aardvark));

    // A name changes where it stands.
    s.click(pos2(300.0, 203.0), PointerButton::Primary);
    s.settle();
    s.key(CTRL, Key::A);
    s.text("Aardvark Petroleum");
    s.key(Modifiers::NONE, Key::Enter);
    s.settle();
    assert!(
        project("Aardvark Petroleum").is_some(),
        "Enter kept the name"
    );
    assert_eq!(project("Aardvark Oil"), None);

    // Esc puts one back.
    s.click(pos2(300.0, 203.0), PointerButton::Primary);
    s.settle();
    s.key(CTRL, Key::A);
    s.text("Nothing like it");
    s.key(Modifiers::NONE, Key::Escape);
    s.settle();
    assert_eq!(project("Nothing like it"), None, "Esc put the name back");
    assert!(project("Aardvark Petroleum").is_some());
}

/// Every icon at the sizes it's used, named: a codepoint that exists can
/// still be the wrong shape. `WHIR_PREVIEW_GLYPHS` adds codepoints to try.
#[test]
#[ignore = "writes pictures to look at"]
fn icon_sheet() {
    let Some(dir) = std::env::var_os("WHIR_PREVIEW_DIR").map(PathBuf::from) else {
        return;
    };
    use whir_core::icons::{self, Icon};
    unsafe {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let extra: Vec<u32> = std::env::var("WHIR_PREVIEW_GLYPHS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|h| u32::from_str_radix(h.trim(), 16).ok())
        .collect();
    let rows = Icon::ALL.len().div_ceil(4) + extra.len().div_ceil(8);
    let ppp = 2.0f32;
    let points = Vec2::new(780.0, 30.0 + rows as f32 * 44.0);
    let size = [(points.x * ppp) as u32, (points.y * ppp) as u32];
    let ctx = egui::Context::default();
    ctx.set_fonts(whir_platform::fonts::definitions(whir_core::theme::FONTS));
    let palette = whir_core::theme::Theme::dark();
    palette.apply(&ctx);
    let device = Device::create(true).expect("software renderer");
    let mut renderer = Renderer::offscreen(device, GpuTextures::new(), size).expect("offscreen");
    for _ in 0..3 {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, points)),
            ..Default::default()
        };
        if let Some(viewport) = raw.viewports.get_mut(&ViewportId::ROOT) {
            viewport.native_pixels_per_point = Some(ppp);
        }
        let mut full = ctx.run_ui(raw, |ui| {
            let painter = ui.painter();
            painter.rect_filled(ui.max_rect(), 0.0, palette.content_bg);
            let small = egui::FontId::proportional(11.0);
            for (i, icon) in Icon::ALL.iter().enumerate() {
                let (col, row) = ((i % 4) as f32, (i / 4) as f32);
                let at = pos2(24.0 + col * 188.0, 26.0 + row * 44.0);
                icons::paint(painter, at, *icon, 16.0, palette.text);
                icons::paint(
                    painter,
                    at + egui::vec2(28.0, 0.0),
                    *icon,
                    10.0,
                    palette.text,
                );
                painter.text(
                    at + egui::vec2(44.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{icon:?}"),
                    small.clone(),
                    palette.text_dim,
                );
            }
            let top = 26.0 + (Icon::ALL.len().div_ceil(4)) as f32 * 44.0;
            for (i, code) in extra.iter().enumerate() {
                let (col, row) = ((i % 8) as f32, (i / 8) as f32);
                let at = pos2(24.0 + col * 94.0, top + row * 44.0);
                let glyph = char::from_u32(*code).unwrap_or(' ');
                painter.text(
                    at,
                    egui::Align2::CENTER_CENTER,
                    glyph,
                    icons::font(16.0),
                    palette.text,
                );
                painter.text(
                    at + egui::vec2(16.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    format!("{code:X}"),
                    small.clone(),
                    palette.text_dim,
                );
            }
        });
        let primitives = ctx.tessellate(full.shapes, full.pixels_per_point);
        renderer.render(
            &primitives,
            &mut full.textures_delta,
            ppp,
            palette.content_bg,
            false,
        );
    }
    let pixels = renderer.read_pixels().expect("offscreen renderer");
    let png = encode_png(
        &pixels,
        size[0],
        size[1],
        size[0] as usize * 4,
        PixelLayout::Bgrx,
    )
    .expect("encode");
    std::fs::write(dir.join("icons.png"), png).expect("write");
}
