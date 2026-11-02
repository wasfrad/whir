//! Headless egui tests: real frames through `Document::ui`, real key and
//! pointer events, checking the annotation tools and their drawing flyouts.

use egui::{Event, RawInput};
use whir_core::{Captures, TextureDesc, UserTexture};

use super::*;
use crate::encode::{PixelLayout, encode_png};

/// Hands out handles without a GPU.
struct NoTextures;

impl TextureRegistry for NoTextures {
    fn create(
        self: Arc<Self>,
        desc: &TextureDesc,
        _pixels: &[u8],
        _stride: usize,
    ) -> Result<UserTexture, String> {
        Ok(UserTexture::new(1, [desc.width, desc.height], self))
    }

    fn max_texture_side(&self) -> u32 {
        4096
    }

    fn release(&self, _id: u64) {}
}

const IMAGE: egui::Vec2 = vec2(400.0, 200.0);

struct Harness {
    ctx: egui::Context,
    doc: ImageDoc,
    time: f64,
    viewport: Rect,
    drawing_toolbar: bool,
    drawing_buttons: [Rect; 2],
    shapes: Vec<Shape>,
    /// Everything the document has asked of the shell.
    requests: Vec<DocRequest>,
    cx: OpenCx,
    dir: PathBuf,
    _serial: Serial,
}

thread_local! {
    static SERIAL_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Tests run one at a time: the settings (and the zoom used last) are the
/// process's. A test's first harness holds the turn; any more it opens share it.
struct Serial(Option<std::sync::MutexGuard<'static, ()>>);

impl Serial {
    fn take() -> Self {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        if SERIAL_HELD.get() {
            return Serial(None);
        }
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        SERIAL_HELD.set(true);
        Serial(Some(guard))
    }
}

impl Drop for Serial {
    fn drop(&mut self) {
        if self.0.is_some() {
            SERIAL_HELD.set(false);
        }
    }
}

impl Harness {
    /// A white 400 × 200 image, shown at 100% in the middle of the window.
    /// It is not in the screenshots folder.
    fn open(name: &str) -> Self {
        Self::open_with(name, false, None)
    }

    /// The same image as a capture in the screenshots folder, with `marks`
    /// already saved for it.
    fn open_stored(name: &str, marks: Option<&str>) -> Self {
        Self::open_with(name, true, marks)
    }

    fn open_with(name: &str, stored: bool, marks: Option<&str>) -> Self {
        let serial = Serial::take();
        // No settings file is read or written by the tests.
        whir_core::settings::memory_only();
        prefs::reset_last();
        unsafe {
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let dir = std::env::temp_dir().join(format!("whir-image-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (IMAGE.x as u32, IMAGE.y as u32);
        let pixels = vec![255u8; (w * h * 4) as usize];
        let png = encode_png(&pixels, w, h, w as usize * 4, PixelLayout::Bgrx).unwrap();
        let path = dir.join("shot.png");
        std::fs::write(&path, png).unwrap();

        let captures = if stored {
            dir.clone()
        } else {
            dir.join("Screenshots")
        };
        if let Some(text) = marks {
            let file = whir_core::captures::marks_path(&path).unwrap();
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let cx = OpenCx {
            textures: Arc::new(NoTextures),
            waker: Waker::noop(),
            library: whir_core::Library::at(dir.join("notes"), Waker::noop()),
            captures: Captures::at(captures, Waker::noop()),
        };
        let mut h = Self {
            ctx: whir_core::theme::test_context(),
            doc: Self::load(&path, &cx),
            time: 0.0,
            viewport: Rect::NOTHING,
            drawing_toolbar: false,
            drawing_buttons: [Rect::NOTHING; 2],
            shapes: Vec::new(),
            requests: Vec::new(),
            cx,
            dir,
            _serial: serial,
        };
        h.frame(vec![]);
        h.frame(vec![]);
        h
    }

    /// A white `w` × `h` PNG called `name` beside the image.
    fn write_image(&self, name: &str, w: u32, h: u32) -> PathBuf {
        let pixels = vec![255u8; (w * h * 4) as usize];
        let png = encode_png(&pixels, w, h, w as usize * 4, PixelLayout::Bgrx).unwrap();
        let path = self.dir.join(name);
        std::fs::write(&path, png).unwrap();
        path
    }

    /// Open `path` in a new tab (this one).
    fn show(&mut self, path: &Path) {
        self.doc = Self::load(path, &self.cx);
        self.frame(vec![]);
    }

    /// Go to `path` as ←/→ do, and draw it.
    fn browse(&mut self, path: &Path) {
        self.doc.go_to(path.to_owned());
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.doc.is_loading() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
            self.doc.poll();
        }
        self.frame(vec![]);
    }

    fn load(path: &Path, cx: &OpenCx) -> ImageDoc {
        let mut doc = ImageDoc::open(path.to_owned(), cx);
        let deadline = Instant::now() + Duration::from_secs(10);
        while doc.is_loading() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
            doc.poll();
        }
        assert!(doc.error.is_none(), "{:?}", doc.error);
        doc
    }

    /// Close the tab (letting its saves finish) and open the image again.
    fn reopen(&mut self) {
        self.saved();
        self.doc = Self::load(&self.path(), &self.cx);
        self.frame(vec![]);
        self.frame(vec![]);
    }

    /// Wait for the marks to be written.
    fn saved(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        self.doc.flush_blocking(deadline).unwrap();
    }

    fn marks_file(&self) -> PathBuf {
        whir_core::captures::marks_path(&self.path()).unwrap()
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.time += 0.05;
        let raw = RawInput {
            events,
            time: Some(self.time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 600.0))),
            ..Default::default()
        };
        let Self {
            ctx,
            doc,
            viewport,
            drawing_toolbar,
            drawing_buttons,
            shapes,
            requests,
            ..
        } = self;
        let textures: Arc<dyn TextureRegistry> = Arc::new(NoTextures);
        let waker = Waker::noop();
        let mut out = ctx.run_ui(raw, |ui| {
            if *drawing_toolbar {
                let theme = Theme::current();
                ui.horizontal(|ui| {
                    for (i, tool) in [Tool::Freehand, Tool::Arrow].into_iter().enumerate() {
                        drawing_buttons[i] = ui
                            .scope(|ui| doc.drawing_tool(ui, tool, &theme))
                            .response
                            .rect;
                    }
                });
                *viewport = ui.available_rect_before_wrap();
            } else {
                *viewport = ui.max_rect();
            }
            let mut cx = DocCx {
                textures: &textures,
                waker: &waker,
                just_activated: false,
                requests,
            };
            doc.ui(ui, &mut cx);
        });
        if *drawing_toolbar {
            *shapes = std::mem::take(&mut out.shapes)
                .into_iter()
                .map(|s| s.shape)
                .collect();
        }
        out.drop_without_applying_deltas();
    }

    /// Opt in to just the two real drawing buttons, leaving old canvas layouts alone.
    fn show_drawing_toolbar(&mut self) {
        self.drawing_toolbar = true;
        self.frame(vec![]);
        self.frame(vec![]);
    }

    fn panel_open(&self, arrow: bool) -> bool {
        egui::Popup::is_id_open(&self.ctx, egui::Id::new("whir.image.drawing").with(arrow))
    }

    fn open_drawing_panel(&mut self, arrow: bool) {
        let at = self.drawing_buttons[arrow as usize].center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.button(at, PointerButton::Secondary, true);
        self.button(at, PointerButton::Secondary, false);
        assert!(self.panel_open(arrow));
        // Let the popup finish sizing and fading in before locating its painted controls.
        self.time += 0.3;
        self.frame(vec![]);
        self.frame(vec![]);
    }

    fn colour_choice(&self, pen: Pen) -> Pos2 {
        self.shapes
            .iter()
            .find_map(|shape| match shape {
                Shape::Rect(r) if r.rect.size() == vec2(14.0, 14.0) && r.fill == pen.color() => {
                    Some(r.rect.center())
                }
                _ => None,
            })
            .expect("the panel's colour swatch")
    }

    fn width_choice(&self, width: u32) -> Pos2 {
        let heading = self
            .shapes
            .iter()
            .find_map(|shape| match shape {
                Shape::Text(t) if t.galley.text() == "Width" => Some(t.pos),
                _ => None,
            })
            .expect("the panel's Width group");
        self.shapes
            .iter()
            .find_map(|shape| match shape {
                Shape::LineSegment { points, stroke }
                    if stroke.width == width as f32
                        && (points[0].distance(points[1]) - 14.0).abs() < 0.01
                        && points[0].x >= heading.x
                        && (heading.y..heading.y + 60.0).contains(&points[0].y) =>
                {
                    Some(points[0].lerp(points[1], 0.5))
                }
                _ => None,
            })
            .expect("the panel's width sample")
    }

    fn choose_drawing_style(&mut self, arrow: bool, pen: Pen, width: u32) {
        self.open_drawing_panel(arrow);
        let colour = self.colour_choice(pen);
        let widths = [2, 4, 8, 12].map(|width| self.width_choice(width));
        assert!(colour.x < widths[0].x, "colour left, widths right");
        for pair in widths.windows(2) {
            assert!(pair[0].x < pair[1].x && (pair[0].y - pair[1].y).abs() < 0.01);
        }
        self.click(colour);
        assert!(
            self.panel_open(arrow),
            "keep the panel open to choose width too"
        );
        self.click(self.width_choice(width));
        assert!(self.panel_open(arrow));
    }

    /// Where image pixel (`x`, `y`) is on screen (however it is turned).
    fn at(&self, x: f32, y: f32) -> Pos2 {
        let at = self.doc.placement(self.viewport, 1.0).unwrap();
        at.screen(pos2(x, y))
    }

    fn button(&mut self, pos: Pos2, button: PointerButton, pressed: bool) {
        self.frame(vec![pointer_button(pos, button, pressed)]);
    }

    /// Follow an image-space path, one motion frame per point, with the current tool.
    fn stroke(&mut self, points: &[Pos2]) {
        let from = self.at(points[0].x, points[0].y);
        self.frame(vec![Event::PointerMoved(from)]);
        self.button(from, PointerButton::Primary, true);
        for point in &points[1..] {
            self.frame(vec![Event::PointerMoved(self.at(point.x, point.y))]);
        }
        let last = points.last().unwrap();
        self.button(self.at(last.x, last.y), PointerButton::Primary, false);
    }

    fn mark(&self, index: usize) -> Mark {
        self.doc.editor().unwrap().marks()[index].clone()
    }

    /// Press at `from`, move to `to` in two steps, release.
    fn drag(&mut self, from: Pos2, to: Pos2, button: PointerButton) {
        self.frame(vec![Event::PointerMoved(from)]);
        self.button(from, button, true);
        self.frame(vec![Event::PointerMoved(from + (to - from) * 0.5)]);
        self.frame(vec![Event::PointerMoved(to)]);
        self.button(to, button, false);
    }

    fn click(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
        self.button(pos, PointerButton::Primary, true);
        self.button(pos, PointerButton::Primary, false);
        // Far enough apart that two of these are never a double click.
        self.time += 1.0;
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]);
    }

    /// Two clicks in quick succession.
    fn double_click(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
        for _ in 0..2 {
            self.button(pos, PointerButton::Primary, true);
            self.button(pos, PointerButton::Primary, false);
        }
        self.time += 1.0;
    }

    fn type_text(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.to_owned())]);
    }

    /// The text of mark `i`.
    fn text(&self, i: usize) -> Text {
        let e = self.doc.editor().unwrap();
        e.marks()[i].text().expect("a text mark").clone()
    }

    /// Pick up the text tool, click at pixel (`x`, `y`), type `text`, Esc.
    fn write(&mut self, x: f32, y: f32, text: &str) {
        if self.doc.tool != Some(Tool::Text) {
            self.key(Key::T, Modifiers::NONE);
        }
        let at = self.at(x, y);
        self.click(at);
        assert!(self.doc.typing.is_some(), "typing");
        self.type_text(text);
        self.key(Key::Escape, Modifiers::NONE);
        assert!(self.doc.typing.is_none(), "finished");
    }

    fn marks(&self) -> Vec<(u32, u32, u32, u32)> {
        self.doc.editor().map_or(Vec::new(), |e| {
            e.marks().iter().map(|m| (m.x, m.y, m.w, m.h)).collect()
        })
    }

    /// Run frames until the document hands over a clipboard image.
    fn copied(&mut self) -> Vec<u8> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            self.frame(vec![]);
            let at = self
                .requests
                .iter()
                .position(|r| matches!(r, DocRequest::CopyImage(_)));
            if let Some(i) = at
                && let DocRequest::CopyImage(ClipImage(dib)) = self.requests.remove(i)
            {
                return dib;
            }
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("shot.png")
    }

    fn selected(&self) -> Option<usize> {
        self.doc.editor().and_then(|e| e.selected)
    }

    /// Arm the tool and draw a mark over pixels 50..150 × 40..80.
    fn draw_one(&mut self) {
        self.key(Key::H, Modifiers::NONE);
        let (a, b) = (self.at(50.0, 40.0), self.at(150.0, 80.0));
        self.drag(a, b, PointerButton::Primary);
        assert_eq!(self.marks(), [(50, 40, 100, 40)]);
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn an_image_opens_fitted_at_actual_size_or_at_the_zoom_used_last() {
    let mut h = Harness::open("opening");
    // Larger than the window (1000 × 600), so 100% is not the fit.
    let a = h.write_image("big-a.png", 1600, 1200);
    let b = h.write_image("big-b.png", 1600, 1200);
    let actual = Mode::Manual {
        zoom: 1.0,
        center: pos2(800.0, 600.0),
    };
    let none = Modifiers::NONE;

    // Out of the box each image is fitted, however the last one was zoomed.
    h.show(&a);
    assert_eq!(h.doc.view.mode, Mode::Fit);
    h.key(Key::Num1, none);
    assert_eq!(h.doc.view.mode, actual);
    assert_eq!(prefs::last(), ZoomPref::Percent(100.0));
    h.browse(&b);
    assert_eq!(h.doc.view.mode, Mode::Fit);

    // The zoom used last: browsing keeps it, and so does a new tab.
    prefs::set_opening(prefs::Opening::Last);
    h.key(Key::Num1, none);
    h.browse(&a);
    assert_eq!(h.doc.view.mode, actual);
    h.show(&b);
    assert_eq!(h.doc.view.mode, actual);
    h.key(Key::Num0, none);
    assert_eq!(prefs::last(), ZoomPref::Fit);
    h.browse(&a);
    assert_eq!(h.doc.view.mode, Mode::Fit);

    // Actual size: centred at 100%; an image smaller than the window is
    // fitted anyway (fit is 100% there).
    prefs::set_opening(prefs::Opening::Actual);
    h.show(&b);
    assert_eq!(h.doc.view.mode, actual);
    let small = h.path();
    h.browse(&small);
    assert_eq!(h.doc.view.mode, Mode::Fit);
}

#[test]
fn a_drag_draws_a_mark_only_while_the_tool_is_on() {
    let mut h = Harness::open("draw");
    let (a, b) = (h.at(50.0, 40.0), h.at(150.0, 80.0));
    h.drag(a, b, PointerButton::Primary);
    assert!(h.marks().is_empty());

    h.key(Key::H, Modifiers::NONE);
    assert_eq!(h.doc.tool, Some(Tool::Highlight));
    h.drag(a, b, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 100, 40)]);
    // Drawn right to left and bottom to top: the same mark.
    h.drag(b, a, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 100, 40), (50, 40, 100, 40)]);
    // Nothing is selected by drawing.
    assert_eq!(h.selected(), None);

    h.key(Key::H, Modifiers::NONE);
    assert_eq!(h.doc.tool, None);
}

#[test]
fn a_drag_past_the_edge_stops_at_the_edge() {
    let mut h = Harness::open("clamp");
    h.key(Key::H, Modifiers::NONE);
    let (a, b) = (h.at(350.0, 150.0), h.at(900.0, 700.0));
    h.drag(a, b, PointerButton::Primary);
    assert_eq!(h.marks(), [(350, 150, 50, 50)]);
}

#[test]
fn a_slip_draws_nothing() {
    let mut h = Harness::open("slip");
    h.key(Key::H, Modifiers::NONE);
    // Far enough to be a drag, but only 2 points tall.
    let (a, b) = (h.at(50.0, 40.0), h.at(90.0, 42.0));
    h.drag(a, b, PointerButton::Primary);
    assert!(h.marks().is_empty());
}

#[test]
fn clicking_selects_and_dragging_the_selection_moves_it() {
    let mut h = Harness::open("move");
    h.draw_one();
    let inside = h.at(100.0, 60.0);
    h.click(inside);
    assert_eq!(h.selected(), Some(0));

    let to = h.at(130.0, 70.0);
    h.drag(inside, to, PointerButton::Primary);
    assert_eq!(h.marks(), [(80, 50, 100, 40)]);
    assert_eq!(h.selected(), Some(0));

    // It can't leave the image.
    let (from, far) = (h.at(100.0, 60.0), h.at(-500.0, -500.0));
    h.drag(from, far, PointerButton::Primary);
    assert_eq!(h.marks(), [(0, 0, 100, 40)]);

    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.marks(), [(80, 50, 100, 40)]);
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.marks(), [(50, 40, 100, 40)]);
    h.key(Key::Y, Modifiers::CTRL);
    assert_eq!(h.marks(), [(80, 50, 100, 40)]);

    // A click on bare image lets go of it.
    let bare = h.at(300.0, 150.0);
    h.click(bare);
    assert_eq!(h.selected(), None);
}

#[test]
fn a_drag_over_an_unselected_mark_draws_another() {
    let mut h = Harness::open("over");
    h.draw_one();
    let (a, b) = (h.at(60.0, 50.0), h.at(200.0, 100.0));
    h.drag(a, b, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 100, 40), (60, 50, 140, 50)]);
}

#[test]
fn delete_removes_the_selection_and_undo_brings_it_back() {
    let mut h = Harness::open("delete");
    h.draw_one();
    // Nothing selected: Delete does nothing.
    h.key(Key::Delete, Modifiers::NONE);
    assert_eq!(h.marks().len(), 1);

    let inside = h.at(100.0, 60.0);
    h.click(inside);
    h.key(Key::Delete, Modifiers::NONE);
    assert!(h.marks().is_empty());
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.marks(), [(50, 40, 100, 40)]);
    h.key(Key::Z, Modifiers::CTRL | Modifiers::SHIFT);
    assert!(h.marks().is_empty());
}

#[test]
fn escape_lets_go_one_thing_at_a_time() {
    let mut h = Harness::open("escape");
    h.draw_one();
    let inside = h.at(100.0, 60.0);
    h.click(inside);
    assert_eq!(h.selected(), Some(0));

    // A drag under way: Esc drops it, and the release then draws nothing.
    let (a, b) = (h.at(200.0, 100.0), h.at(300.0, 150.0));
    h.frame(vec![Event::PointerMoved(a)]);
    h.button(a, PointerButton::Primary, true);
    h.frame(vec![Event::PointerMoved(b)]);
    assert!(h.doc.drag.is_some());
    h.key(Key::Escape, Modifiers::NONE);
    assert!(h.doc.drag.is_none());
    h.button(b, PointerButton::Primary, false);
    assert_eq!(h.marks().len(), 1);
    assert_eq!(h.selected(), Some(0));

    h.key(Key::Escape, Modifiers::NONE);
    assert_eq!(h.selected(), None);
    assert_eq!(h.doc.tool, Some(Tool::Highlight));
    h.key(Key::Escape, Modifiers::NONE);
    assert_eq!(h.doc.tool, None);
}

#[test]
fn the_middle_button_still_pans_while_the_tool_is_on() {
    let mut h = Harness::open("pan");
    h.key(Key::H, Modifiers::NONE);
    let vp = h.viewport.size();
    h.doc.view.set_zoom(4.0, egui::Vec2::ZERO, IMAGE, vp, 1.0);
    let before = h.doc.view.center(IMAGE);
    let from = h.viewport.center();
    h.drag(from, from + vec2(-100.0, 0.0), PointerButton::Middle);
    let after = h.doc.view.center(IMAGE);
    assert!(
        (after.x - before.x - 25.0).abs() < 0.01,
        "{before:?} → {after:?}"
    );
    assert!(h.marks().is_empty());
}

/// (width, height) of a clipboard DIB, and its BGRA pixel at (`x`, `y`) from
/// the top left.
fn dib_pixel(dib: &[u8], x: usize, y: usize) -> ((usize, usize), [u8; 4]) {
    let int = |i: usize| i32::from_le_bytes(dib[i..i + 4].try_into().unwrap()) as usize;
    let (w, h) = (int(4), int(8));
    let i = 40 + ((h - 1 - y) * w + x) * 4;
    ((w, h), dib[i..i + 4].try_into().unwrap())
}

#[test]
fn copying_bakes_the_marks_into_the_clipboard_image() {
    let mut h = Harness::open("copy");
    h.draw_one();
    h.frame(vec![Event::Copy]);
    let dib = h.copied();
    let [r, g, b] = Ink::Yellow.rgb();
    assert_eq!(dib_pixel(&dib, 100, 60), ((400, 200), [b, g, r, 255]));
    assert_eq!(dib_pixel(&dib, 49, 60).1, [255; 4]);
    assert_eq!(dib_pixel(&dib, 100, 80).1, [255; 4]);
}

#[test]
fn a_copy_is_turned_like_the_screen() {
    let mut h = Harness::open("copy-turned");
    h.draw_one();
    h.key(Key::R, Modifiers::NONE);
    h.frame(vec![Event::Copy]);
    let dib = h.copied();
    let [r, g, b] = Ink::Yellow.rgb();
    // Clockwise: image pixel (x, y) is now at (199 - y, x).
    assert_eq!(dib_pixel(&dib, 199 - 60, 100), ((200, 400), [b, g, r, 255]));
    assert_eq!(dib_pixel(&dib, 199 - 10, 100).1, [255; 4]);
}

#[test]
fn saving_a_copy_writes_the_marks_and_leaves_the_original_alone() {
    let mut h = Harness::open("export");
    // Nothing drawn: Ctrl+S has nothing to offer.
    assert!(matches!(h.doc.save(), SaveResult::Unsupported));
    h.draw_one();
    let SaveResult::NeedsPath(spec) = h.doc.save() else {
        panic!("expected a Save dialog");
    };
    assert_eq!(spec.suggested_name, "shot copy.png");
    assert_eq!(spec.folder.as_deref(), Some(h.dir.as_path()));

    let original = std::fs::read(h.path()).unwrap();
    assert!(matches!(h.doc.save_as(h.path()), SaveResult::Failed(_)));
    let dest = h.dir.join(spec.suggested_name);
    assert!(matches!(h.doc.save_as(dest.clone()), SaveResult::Started));
    assert!(h.doc.is_saving());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while h.doc.is_saving() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(2));
        h.doc.poll();
    }
    assert_eq!(h.doc.notice.as_deref(), Some("Saved shot copy.png"));

    let d = decode::decode(&dest, None).unwrap();
    assert_eq!((d.width, d.height), (400, 200));
    let px = |x: usize, y: usize| &d.pixels[y * d.stride + x * 4..][..4];
    let [r, g, b] = Ink::Yellow.rgb();
    assert_eq!(px(100, 60), [b, g, r, 255]);
    assert_eq!(px(10, 10), [255; 4]);
    assert_eq!(std::fs::read(h.path()).unwrap(), original);
}

#[test]
fn a_captures_highlights_save_themselves_and_come_back() {
    let mut h = Harness::open_stored("stored", None);
    assert_eq!(h.doc.persistence(), Persistence::Auto);
    assert!(!h.marks_file().exists());
    let original = std::fs::read(h.path()).unwrap();

    // Just captured, so the highlighter is already in hand. Drawn, and
    // written without anyone saving.
    assert_eq!(h.doc.tool, Some(Tool::Highlight));
    let (a, b) = (h.at(50.0, 40.0), h.at(150.0, 80.0));
    h.drag(a, b, PointerButton::Primary);
    h.saved();
    assert!(!h.doc.is_dirty() && !h.doc.is_saving());
    assert_eq!(h.doc.save_error(), None);
    assert_eq!(
        std::fs::read_to_string(h.marks_file()).unwrap(),
        "whir-marks 1
size 400 200
rect 50 40 100 40 yellow
"
    );
    assert_eq!(
        std::fs::read(h.path()).unwrap(),
        original,
        "the capture is never rewritten"
    );

    // Another day: it is still there, and still an object that can be moved.
    h.reopen();
    assert_eq!(h.marks(), [(50, 40, 100, 40)]);
    let inside = h.at(100.0, 60.0);
    h.click(inside);
    let to = h.at(130.0, 70.0);
    h.drag(inside, to, PointerButton::Primary);
    h.reopen();
    assert_eq!(h.marks(), [(80, 50, 100, 40)]);

    // Removed: the file goes with the last highlight.
    let inside = h.at(100.0, 60.0);
    h.click(inside);
    h.key(Key::Delete, Modifiers::NONE);
    h.saved();
    assert!(!h.marks_file().exists());
    h.reopen();
    assert!(h.marks().is_empty());
}

#[test]
fn an_image_elsewhere_keeps_its_highlights_to_the_tab() {
    let mut h = Harness::open("unstored");
    assert_eq!(h.doc.persistence(), Persistence::None);
    h.draw_one();
    h.saved();
    assert_eq!(h.doc.persistence(), Persistence::None);
    assert!(!h.doc.is_dirty());
    // Nothing was written beside it.
    let names: Vec<_> = std::fs::read_dir(&h.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["shot.png"]);
    h.reopen();
    assert!(h.marks().is_empty());
}

#[test]
fn a_fresh_capture_opens_ready_to_highlight() {
    // The file was written a moment ago, in the screenshots folder.
    let mut h = Harness::open_stored("fresh", None);
    assert_eq!(h.doc.tool, Some(Tool::Highlight));
    let (a, b) = (h.at(50.0, 40.0), h.at(150.0, 80.0));
    h.drag(a, b, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 100, 40)]);
    // Anywhere else, the highlighter waits to be picked up.
    assert_eq!(Harness::open("not-fresh").doc.tool, None);
}

#[test]
fn saved_marks_load_and_marks_for_another_image_are_let_go() {
    let saved = "whir-marks 1
size 400 200
rect 10 20 30 40 pink
rect 0 0 5 5 blue
";
    let h = Harness::open_stored("load", Some(saved));
    assert_eq!(h.marks(), [(10, 20, 30, 40), (0, 0, 5, 5)]);
    let kind = &h.doc.editor().unwrap().marks()[0].kind;
    assert_eq!(*kind, Kind::Highlight(Ink::Pink));
    assert!(!h.doc.is_dirty(), "loading isn't an edit");

    // The file was replaced by one of another size: its marks can't be placed.
    let stale = "whir-marks 1
size 999 999
rect 10 20 30 40 pink
";
    let h = Harness::open_stored("stale", Some(stale));
    assert!(h.marks().is_empty());
}

fn key_event(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn a_selected_highlight_is_resized_by_its_handles() {
    let mut h = Harness::open("resize");
    h.draw_one();
    let inside = h.at(100.0, 60.0);
    h.click(inside);
    assert_eq!(h.selected(), Some(0));

    // A corner moves two sides.
    let (corner, to) = (h.at(150.0, 80.0), h.at(200.0, 100.0));
    h.drag(corner, to, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 150, 60)]);
    assert_eq!(h.selected(), Some(0));
    // The middle of a side moves that side only.
    let (side, to) = (h.at(200.0, 70.0), h.at(220.0, 150.0));
    h.drag(side, to, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 170, 60)]);

    // Each resize is an edit of its own.
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.marks(), [(50, 40, 150, 60)]);
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.marks(), [(50, 40, 100, 40)]);
    h.key(Key::Y, Modifiers::CTRL);
    assert_eq!(h.marks(), [(50, 40, 150, 60)]);
}

#[test]
fn an_unselected_mark_has_no_handles() {
    let mut h = Harness::open("resize-unselected");
    h.draw_one();
    // From its corner, without selecting it first: another highlight.
    let (corner, to) = (h.at(150.0, 80.0), h.at(200.0, 100.0));
    h.drag(corner, to, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 100, 40), (150, 80, 50, 20)]);
}

#[test]
fn handles_work_on_a_turned_image() {
    let mut h = Harness::open("resize-turned");
    h.draw_one();
    h.key(Key::R, Modifiers::NONE);
    let inside = h.at(100.0, 60.0);
    h.click(inside);
    assert_eq!(h.selected(), Some(0));
    let (corner, to) = (h.at(150.0, 80.0), h.at(200.0, 100.0));
    h.drag(corner, to, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 150, 60)]);
}

#[test]
fn the_text_tool_types_where_it_is_clicked() {
    let mut h = Harness::open("text");
    h.write(60.0, 50.0, "Hi");
    let t = h.text(0);
    assert_eq!(t.body, "Hi");
    assert_eq!(t.size, 20, "20 points tall on screen, at 100%");
    assert_eq!(t.turn, 0);
    let (x, y, w, height) = h.marks()[0];
    assert_eq!((x, y), (60, 50));
    assert!(w > 10 && height > 10, "{w} × {height}");
    // Finished, it is selected: ready to move, resize, recolour or remove.
    assert_eq!(h.selected(), Some(0));
    assert_eq!(h.doc.tool, Some(Tool::Text));
    // Typed in one go, undone in one.
    h.key(Key::Z, Modifiers::CTRL);
    assert!(h.marks().is_empty());
    h.key(Key::Y, Modifiers::CTRL);
    assert_eq!(h.text(0).body, "Hi");
}

#[test]
fn enter_starts_a_line_and_ctrl_enter_finishes() {
    let mut h = Harness::open("text-lines");
    h.write(60.0, 50.0, "one");
    let one_line = h.marks()[0].3;

    let at = h.at(60.0, 120.0);
    h.click(at);
    h.type_text("one");
    h.frame(vec![key_event(Key::Enter)]);
    h.type_text("two");
    h.key(Key::Enter, Modifiers::CTRL);
    assert!(h.doc.typing.is_none());
    assert_eq!(h.text(1).body, "one\ntwo");
    assert!(h.marks()[1].3 > one_line * 3 / 2, "two lines tall");
}

#[test]
fn keys_typed_are_the_texts_not_the_tabs() {
    let mut h = Harness::open("text-keys");
    h.key(Key::T, Modifiers::NONE);
    let at = h.at(60.0, 50.0);
    h.click(at);
    h.frame(vec![
        key_event(Key::H),
        Event::Text("h".into()),
        key_event(Key::R),
        Event::Text("r".into()),
        key_event(Key::T),
        Event::Text("t".into()),
    ]);
    assert_eq!(h.doc.typing.as_ref().unwrap().body, "hrt");
    // Not the highlighter, not a turn, not the tool put down.
    assert_eq!(h.doc.tool, Some(Tool::Text));
    assert_eq!(h.doc.turns(), 0);
    // Esc finishes it.
    h.key(Key::Escape, Modifiers::NONE);
    assert_eq!(h.text(0).body, "hrt");
}

#[test]
fn a_click_elsewhere_finishes_the_text_and_does_nothing_else() {
    let mut h = Harness::open("text-click-off");
    h.key(Key::T, Modifiers::NONE);
    let a = h.at(60.0, 50.0);
    h.click(a);
    h.type_text("Hi");
    let b = h.at(250.0, 150.0);
    h.click(b);
    assert!(h.doc.typing.is_none(), "finished, not started again");
    assert_eq!(h.marks().len(), 1);
    assert_eq!(h.text(0).body, "Hi");
    // The next click starts more; left empty, nothing is kept.
    h.click(b);
    assert!(h.doc.typing.is_some());
    h.key(Key::Escape, Modifiers::NONE);
    assert_eq!(h.marks().len(), 1);
}

#[test]
fn double_clicking_text_retypes_it() {
    let mut h = Harness::open("retype");
    h.write(60.0, 50.0, "Hi");
    let inside = h.at(65.0, 55.0);
    h.double_click(inside);
    let t = h.doc.typing.as_ref().expect("retyping");
    assert_eq!((t.index, t.body.as_str()), (Some(0), "Hi"));
    h.type_text("!");
    h.key(Key::Escape, Modifiers::NONE);
    assert_eq!(h.marks().len(), 1);
    assert_eq!(h.text(0).body, "Hi!");

    // Emptied, it goes; undo brings it back.
    h.double_click(inside);
    h.frame(vec![key_event(Key::Backspace); 3]);
    h.key(Key::Escape, Modifiers::NONE);
    assert!(h.marks().is_empty());
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.text(0).body, "Hi!");
}

#[test]
fn text_is_resized_from_a_corner_and_removed_with_delete() {
    let mut h = Harness::open("text-resize");
    h.write(60.0, 50.0, "Hi");
    let (x, y, w, height) = h.marks()[0];
    let size = h.text(0).size;
    // Dragged out to twice as wide: twice the size, from the same top left.
    let corner = h.at((x + w) as f32, (y + height) as f32);
    let to = h.at((x + 2 * w) as f32, (y + height) as f32);
    h.drag(corner, to, PointerButton::Primary);
    assert_eq!(h.text(0).size, size * 2);
    let (x2, y2, w2, h2) = h.marks()[0];
    assert_eq!((x2, y2), (x, y));
    assert!(w2 > w * 3 / 2 && h2 > height * 3 / 2, "{w2} × {h2}");
    assert_eq!(h.text(0).body, "Hi");
    // The next text is that size too.
    assert_eq!(h.doc.text_size, Some(size * 2));

    h.key(Key::Delete, Modifiers::NONE);
    assert!(h.marks().is_empty());
}

#[test]
fn text_typed_on_a_turned_image_reads_upright_that_way() {
    let mut h = Harness::open("text-turned");
    h.key(Key::R, Modifiers::NONE);
    h.write(60.0, 150.0, "Hello");
    let t = h.text(0);
    assert_eq!(t.turn, 1);
    // On the image itself it runs up the page: taller than wide.
    let (x, _, w, height) = h.marks()[0];
    assert!(height > w, "{w} × {height}");
    assert_eq!(x, 60);
}

#[test]
fn copying_sets_the_text_into_the_clipboard_image() {
    let mut h = Harness::open("copy-text");
    h.doc.text_size = Some(60);
    h.write(20.0, 20.0, "Hi");
    let (x, y, w, height) = h.marks()[0];
    h.frame(vec![Event::Copy]);
    let dib = h.copied();
    let [r, g, b] = h.text(0).pen.rgb();
    let mut inked = false;
    for py in 0..200 {
        for px in 0..400 {
            let p = dib_pixel(&dib, px, py).1;
            let inside =
                (x..x + w).contains(&(px as u32)) && (y..y + height).contains(&(py as u32));
            if inside {
                inked |= p == [b, g, r, 255];
            } else {
                assert_eq!(p, [255; 4], "outside the text at ({px}, {py})");
            }
        }
    }
    assert!(inked, "no letters in the copy");
}

#[test]
fn a_captures_text_saves_itself_even_mid_word() {
    let mut h = Harness::open_stored("text-stored", None);
    h.key(Key::T, Modifiers::NONE);
    let at = h.at(60.0, 50.0);
    h.click(at);
    h.type_text("Hi \"there\"");
    // Closing the tab (or quitting) finishes what is being typed.
    h.saved();
    assert!(h.doc.typing.is_none());
    let file = std::fs::read_to_string(h.marks_file()).unwrap();
    assert!(file.contains("text 60 50 "), "{file}");
    assert!(file.contains(r#""Hi \"there\"""#), "{file}");

    h.reopen();
    assert_eq!(h.text(0).body, "Hi \"there\"");
    // Leaving the tab does too.
    h.key(Key::T, Modifiers::NONE);
    let at = h.at(60.0, 120.0);
    h.click(at);
    h.type_text("Yo");
    h.doc.on_visibility(false);
    assert!(h.doc.typing.is_none());
    assert_eq!(h.marks().len(), 2);
}

fn pointer_button(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn points_near(mark: &Mark, expected: &[Pos2]) {
    let actual = mark.drawing_points();
    assert_eq!(actual.len(), expected.len(), "{actual:?} != {expected:?}");
    for (actual, expected) in actual.iter().zip(expected) {
        // Paths are stored as normalized integers, not the original floats.
        assert!(
            actual.distance(*expected) < 0.01,
            "{actual:?} != {expected:?}"
        );
    }
}

fn style(mark: &Mark) -> (Pen, u32) {
    let d = mark.drawing_style().expect("a drawing mark");
    (d.pen, d.width)
}

#[test]
fn drawing_pen_keeps_bends_and_batched_press_motion_and_release_as_one_edit() {
    let mut h = Harness::open("drawing-samples");
    edit::set_drawing_pen(false, Pen::Green);
    edit::set_width(false, 8);
    h.key(Key::P, Modifiers::NONE);
    assert_eq!(h.doc.tool, Some(Tool::Freehand));
    let points = [
        pos2(30.0, 30.0),
        pos2(90.0, 30.0),
        pos2(90.0, 80.0),
        pos2(45.0, 80.0),
        pos2(45.0, 130.0),
        pos2(160.0, 130.0),
        pos2(160.25, 130.0),
    ];
    let screen = points.map(|p| h.at(p.x, p.y));
    h.frame(vec![
        Event::PointerMoved(h.at(10.0, 10.0)), // Before the press: not ink.
        pointer_button(screen[0], PointerButton::Primary, true),
        Event::PointerMoved(screen[1]),
        Event::PointerMoved(screen[2]),
    ]);
    let Some(Drag::Freehand {
        points: sampled,
        pen,
        width,
    }) = &h.doc.drag
    else {
        panic!("the pen must start collecting on the press frame");
    };
    assert_eq!(sampled.len(), 3);
    for (sample, expected) in sampled.iter().zip(&points) {
        assert!(
            sample.distance(*expected) < 0.001,
            "{sample:?} != {expected:?}"
        );
    }
    assert_eq!((*pen, *width), (Pen::Green, 8));
    assert!(h.marks().is_empty());
    h.frame(vec![
        Event::PointerMoved(screen[3]),
        Event::PointerMoved(screen[4]),
    ]);
    assert!(h.marks().is_empty());
    h.frame(vec![
        Event::PointerMoved(screen[5]),
        // The final location is kept even below the normal sampling distance.
        pointer_button(screen[6], PointerButton::Primary, false),
        Event::PointerMoved(h.at(300.0, 180.0)), // After the release: not ink.
    ]);
    assert!(h.doc.drag.is_none());
    assert_eq!(h.marks().len(), 1);
    let mark = h.mark(0);
    assert!(matches!(mark.kind, Kind::Freehand(_)));
    points_near(&mark, &points);
    assert_eq!(style(&mark), (Pen::Green, 8));
    assert_eq!(h.selected(), None);
    h.key(Key::Z, Modifiers::CTRL);
    assert!(h.marks().is_empty());
    assert!(
        !h.doc.editor().unwrap().can_undo(),
        "one undo for the whole path"
    );
    h.key(Key::Y, Modifiers::CTRL);
    assert_eq!(h.mark(0), mark);

    // A quick stroke can also arrive entirely between two rendered frames.
    let quick = [
        pos2(240.0, 30.0),
        pos2(300.0, 30.0),
        pos2(300.0, 80.0),
        pos2(340.0, 80.0),
    ];
    let screen = quick.map(|p| h.at(p.x, p.y));
    h.frame(vec![
        Event::PointerMoved(screen[0]),
        pointer_button(screen[0], PointerButton::Primary, true),
        Event::PointerMoved(screen[1]),
        Event::PointerMoved(screen[2]),
        pointer_button(screen[3], PointerButton::Primary, false),
    ]);
    assert_eq!(h.marks().len(), 2, "a whole gesture batched into one frame");
    points_near(&h.mark(1), &quick);
    h.key(Key::Z, Modifiers::CTRL);
    assert_eq!(h.doc.editor().unwrap().marks(), &[mark]);
    h.key(Key::P, Modifiers::NONE);
    assert_eq!(h.doc.tool, None);
}

#[test]
fn drawing_previews_are_not_committed_or_saved_before_release() {
    for (name, key) in [("pen", Key::P), ("arrow", Key::A)] {
        let mut h = Harness::open_stored(&format!("drawing-preview-{name}"), None);
        h.key(key, Modifiers::NONE);
        let (a, b, tip) = (h.at(50.0, 40.0), h.at(150.0, 90.0), h.at(180.0, 120.0));
        h.frame(vec![Event::PointerMoved(a)]);
        h.button(a, PointerButton::Primary, true);
        h.frame(vec![Event::PointerMoved(b)]);
        assert!(h.doc.drag.is_some());
        assert!(h.marks().is_empty());
        assert!(!h.doc.editor().unwrap().can_undo());
        h.saved();
        assert!(!h.marks_file().exists(), "{name}: no preview on disk");
        assert!(!h.doc.is_dirty() && !h.doc.is_saving());
        assert!(h.doc.drag.is_some(), "flushing must not finish a gesture");
        h.button(tip, PointerButton::Primary, false);
        assert_eq!(h.marks().len(), 1);
        h.saved();
        let saved = marks::parse(&std::fs::read_to_string(h.marks_file()).unwrap()).unwrap();
        assert_eq!(saved.marks, [h.mark(0)]);
        let expected = if key == Key::P {
            vec![pos2(50.0, 40.0), pos2(150.0, 90.0), pos2(180.0, 120.0)]
        } else {
            vec![pos2(50.0, 40.0), pos2(180.0, 120.0)]
        };
        points_near(&h.mark(0), &expected);
    }
}

#[test]
fn drawing_pen_clicks_make_dots_or_select_but_drags_over_unselected_marks_draw() {
    let mut h = Harness::open("drawing-dot-select");
    h.draw_one();
    let highlight = h.mark(0);
    h.key(Key::P, Modifiers::NONE);
    h.click(h.at(100.0, 60.0));
    assert_eq!(h.selected(), Some(0));
    assert_eq!(
        h.marks().len(),
        1,
        "selecting a highlight must not leave a dot"
    );
    h.key(Key::Escape, Modifiers::NONE);
    let path = [pos2(60.0, 50.0), pos2(100.0, 110.0), pos2(200.0, 110.0)];
    h.stroke(&path);
    assert_eq!(h.marks().len(), 2);
    assert_eq!(h.mark(0), highlight, "the unselected mark was not moved");
    points_near(&h.mark(1), &path);
    assert_eq!(h.selected(), None);
    h.click(h.at(150.0, 110.0));
    assert_eq!(h.selected(), Some(1));
    assert_eq!(
        h.marks().len(),
        2,
        "selecting a stroke must not leave a dot"
    );
    h.key(Key::Escape, Modifiers::NONE);
    h.click(h.at(320.0, 160.0));
    assert_eq!(h.marks().len(), 3);
    points_near(&h.mark(2), &[pos2(320.0, 160.0)]);
    h.click(h.at(320.0, 160.0));
    assert_eq!(h.selected(), Some(2));
    assert_eq!(h.marks().len(), 3, "a second click selects the dot");
    h.drag(
        h.at(320.0, 160.0),
        h.at(340.0, 170.0),
        PointerButton::Primary,
    );
    assert_eq!(h.marks().len(), 3);
    points_near(&h.mark(2), &[pos2(340.0, 170.0)]);
    h.key(Key::Z, Modifiers::CTRL);
    points_near(&h.mark(2), &[pos2(320.0, 160.0)]);
}

#[test]
fn drawing_escape_and_undo_cancel_and_swallow_the_rest_of_the_gesture() {
    for (name, key, tool) in [
        ("pen", Key::P, Tool::Freehand),
        ("arrow", Key::A, Tool::Arrow),
    ] {
        let mut h = Harness::open(&format!("drawing-cancel-{name}"));
        h.key(key, Modifiers::NONE);
        h.stroke(&[pos2(50.0, 40.0), pos2(150.0, 80.0)]);
        let kept = h.mark(0);
        for (moved, move_after_cancel) in [(false, false), (false, true), (true, true)] {
            if !moved && tool == Tool::Arrow {
                continue; // Unlike the pen, an arrow starts once the pointer drags.
            }
            let a = h.at(220.0, 80.0);
            h.frame(vec![Event::PointerMoved(a)]);
            h.button(a, PointerButton::Primary, true);
            if moved {
                h.frame(vec![Event::PointerMoved(h.at(260.0, 110.0))]);
            }
            assert!(h.doc.drag.is_some());
            h.key(Key::Escape, Modifiers::NONE);
            assert!(h.doc.drag.is_none());
            assert!(h.doc.swallow);
            let release = if move_after_cancel {
                h.frame(vec![Event::PointerMoved(h.at(300.0, 140.0))]);
                assert!(
                    h.doc.drag.is_none(),
                    "a cancelled press cannot restart a drag"
                );
                h.at(320.0, 150.0)
            } else {
                a // Still an egui click: its release must not leave a dot either.
            };
            h.button(release, PointerButton::Primary, false);
            assert!(!h.doc.swallow);
            assert_eq!(h.doc.tool, Some(tool));
            assert_eq!(h.doc.editor().unwrap().marks(), std::slice::from_ref(&kept));
        }
        let a = h.at(220.0, 80.0);
        h.frame(vec![Event::PointerMoved(a)]);
        h.button(a, PointerButton::Primary, true);
        h.frame(vec![Event::PointerMoved(h.at(260.0, 110.0))]);
        h.key(Key::Z, Modifiers::CTRL);
        assert!(h.doc.drag.is_none());
        assert!(
            h.marks().is_empty(),
            "undo acts on the previous completed stroke"
        );
        h.frame(vec![Event::PointerMoved(h.at(300.0, 140.0))]);
        h.button(h.at(320.0, 150.0), PointerButton::Primary, false);
        assert!(
            h.marks().is_empty(),
            "undo must also swallow the pending release"
        );
        h.key(Key::Y, Modifiers::CTRL);
        assert_eq!(h.doc.editor().unwrap().marks(), &[kept]);
    }
}

#[test]
fn drawing_starts_only_inside_the_image_and_clamps_an_inside_drag_at_its_edge() {
    for (name, key) in [("pen", Key::P), ("arrow", Key::A)] {
        let mut h = Harness::open(&format!("drawing-outside-{name}"));
        h.key(key, Modifiers::NONE);
        for from in [
            pos2(-30.0, 100.0),
            pos2(430.0, 100.0),
            pos2(200.0, -30.0),
            pos2(200.0, 230.0),
        ] {
            h.click(h.at(from.x, from.y));
            h.drag(
                h.at(from.x, from.y),
                h.at(200.0, 100.0),
                PointerButton::Primary,
            );
            assert!(
                h.marks().is_empty(),
                "{name}: a press at {from:?} is outside"
            );
            assert!(h.doc.drag.is_none());
        }
        h.stroke(&[pos2(350.0, 150.0), pos2(450.0, 250.0)]);
        assert_eq!(h.marks().len(), 1);
        points_near(&h.mark(0), &[pos2(350.0, 150.0), pos2(400.0, 200.0)]);
    }
}

#[test]
fn drawing_arrows_keep_tail_to_release_tip_in_all_quadrants_and_on_both_axes() {
    let mut h = Harness::open("drawing-arrow-directions");
    h.key(Key::A, Modifiers::NONE);
    assert_eq!(h.doc.tool, Some(Tool::Arrow));
    let tail = pos2(200.0, 100.0);
    for delta in [
        vec2(90.0, 60.0),
        vec2(-90.0, 60.0),
        vec2(90.0, -60.0),
        vec2(-90.0, -60.0),
        vec2(90.0, 0.0),
        vec2(-90.0, 0.0),
        vec2(0.0, 60.0),
        vec2(0.0, -60.0),
    ] {
        let tip = tail + delta;
        let a = h.at(tail.x, tail.y);
        h.frame(vec![Event::PointerMoved(a)]);
        h.button(a, PointerButton::Primary, true);
        h.frame(vec![Event::PointerMoved(
            h.at(tail.x + delta.x * 0.5, tail.y + delta.y * 0.5),
        )]);
        // No PointerMoved at the tip: the button-up event is authoritative.
        h.button(h.at(tip.x, tip.y), PointerButton::Primary, false);
        assert_eq!(h.marks().len(), 1, "direction {delta:?}");
        assert!(matches!(h.mark(0).kind, Kind::Arrow(_)));
        points_near(&h.mark(0), &[tail, tip]);
        assert_eq!(h.selected(), None);
        h.key(Key::Z, Modifiers::CTRL);
        assert!(h.marks().is_empty());
    }
    h.key(Key::A, Modifiers::NONE);
    assert_eq!(h.doc.tool, None);
}

#[test]
fn a_batched_arrow_stops_at_release_not_subsequent_pointer_motion() {
    let mut h = Harness::open("drawing-quick-arrow");
    h.key(Key::A, Modifiers::NONE);
    let (a, b) = (h.at(60.0, 40.0), h.at(250.0, 140.0));
    h.frame(vec![
        Event::PointerMoved(a),
        pointer_button(a, PointerButton::Primary, true),
        Event::PointerMoved(h.at(150.0, 80.0)),
        pointer_button(b, PointerButton::Primary, false),
        Event::PointerMoved(h.at(320.0, 180.0)),
    ]);
    assert_eq!(h.marks().len(), 1);
    points_near(&h.mark(0), &[pos2(60.0, 40.0), pos2(250.0, 140.0)]);
}

#[test]
fn a_pen_click_with_small_motion_does_not_move_or_resize_the_selection() {
    let mut h = Harness::open("drawing-click-jitter");
    h.draw_one();
    h.click(h.at(100.0, 60.0));
    let original = h.mark(0);
    h.key(Key::P, Modifiers::NONE);
    for p in [pos2(100.0, 60.0), pos2(150.0, 80.0)] {
        let a = h.at(p.x, p.y);
        h.frame(vec![Event::PointerMoved(a)]);
        h.button(a, PointerButton::Primary, true);
        assert!(h.doc.drag.is_none());
        h.button(a + vec2(-1.0, -1.0), PointerButton::Primary, false);
        assert_eq!(h.marks().len(), 1);
        assert_eq!(h.mark(0), original);
    }
    h.key(Key::Z, Modifiers::CTRL);
    assert!(h.marks().is_empty(), "the clicks added no history");
}

#[test]
fn short_arrow_handles_choose_the_nearest_endpoint() {
    let mut h = Harness::open("drawing-short-handles");
    h.key(Key::A, Modifiers::NONE);
    h.stroke(&[pos2(100.0, 60.0), pos2(160.0, 60.0)]);
    // Both handles fall within the same hit radius after shortening.
    let arrow = tools::arrow(
        pos2(100.0, 60.0),
        pos2(104.0, 60.0),
        IMAGE,
        4.0,
        Pen::Red,
        2,
    )
    .unwrap();
    h.doc.editor_mut().unwrap().change(0, arrow);
    h.click(h.at(102.0, 60.0));
    assert_eq!(h.selected(), Some(0));
    let at = h.doc.placement(h.viewport, 1.0).unwrap();
    assert_eq!(
        h.doc.handle_at(&at, h.at(100.0, 60.0)),
        Some((0, Handle::Start))
    );
    assert_eq!(
        h.doc.handle_at(&at, h.at(104.0, 60.0)),
        Some((0, Handle::End))
    );
    h.drag(h.at(104.0, 60.0), h.at(180.0, 90.0), PointerButton::Primary);
    points_near(&h.mark(0), &[pos2(100.0, 60.0), pos2(180.0, 90.0)]);
}

#[test]
fn drawing_arrow_minimum_length_is_four_screen_points_at_any_zoom() {
    let mut h = Harness::open("drawing-arrow-minimum");
    h.key(Key::A, Modifiers::NONE);
    for zoom in [1.0, 1.25, 2.0] {
        h.doc
            .view
            .set_zoom(zoom, Vec2::ZERO, IMAGE, h.viewport.size(), 1.0);
        h.frame(vec![]);
        for length in [3.5, 4.0] {
            let a = h.at(200.0, 100.0);
            h.frame(vec![Event::PointerMoved(a)]);
            h.button(a, PointerButton::Primary, true);
            // Establish an egui drag, then come back to the exact length under test.
            h.frame(vec![Event::PointerMoved(a + vec2(30.0, 0.0))]);
            h.button(a + vec2(length, 0.0), PointerButton::Primary, false);
            if length < 4.0 {
                assert!(h.marks().is_empty(), "{length} screen points at {zoom}x");
            } else {
                assert_eq!(h.marks().len(), 1, "{length} screen points at {zoom}x");
                points_near(
                    &h.mark(0),
                    &[pos2(200.0, 100.0), pos2(200.0 + length / zoom, 100.0)],
                );
                h.key(Key::Z, Modifiers::CTRL);
            }
        }
    }
}

#[test]
fn drawing_arrow_endpoint_handles_work_after_rotation_and_zoom() {
    for (turns, zoom) in [(0, 1.0), (1, 1.25), (2, 2.0), (3, 1.25)] {
        let mut h = Harness::open(&format!("drawing-endpoints-{turns}"));
        edit::set_drawing_pen(true, Pen::Blue);
        edit::set_width(true, 4);
        h.key(Key::A, Modifiers::NONE);
        h.stroke(&[pos2(300.0, 60.0), pos2(100.0, 140.0)]);
        let original = h.mark(0);
        h.click(h.at(200.0, 100.0));
        assert_eq!(h.selected(), Some(0));
        for _ in 0..turns {
            h.key(Key::R, Modifiers::NONE);
        }
        h.doc.view.set_zoom(
            zoom,
            Vec2::ZERO,
            view::turned(IMAGE, turns),
            h.viewport.size(),
            1.0,
        );
        h.frame(vec![]);
        let at = h.doc.placement(h.viewport, 1.0).unwrap();
        assert!((at.zoom() - zoom).abs() < 0.001);
        let handles: Vec<_> = at.handles(&original).collect();
        assert_eq!(handles.len(), 2);
        for (handle, point) in [
            (Handle::Start, pos2(300.0, 60.0)),
            (Handle::End, pos2(100.0, 140.0)),
        ] {
            let screen = h.at(point.x, point.y);
            assert!(
                handles
                    .iter()
                    .any(|(which, p)| *which == handle && p.distance(screen) < 0.01)
            );
            assert_eq!(h.doc.handle_at(&at, screen), Some((0, handle)));
        }
        h.drag(
            h.at(300.0, 60.0),
            h.at(330.0, 160.0),
            PointerButton::Primary,
        );
        points_near(&h.mark(0), &[pos2(330.0, 160.0), pos2(100.0, 140.0)]);
        let tail_changed = h.mark(0);
        h.drag(
            h.at(100.0, 140.0),
            h.at(130.0, 30.0),
            PointerButton::Primary,
        );
        points_near(&h.mark(0), &[pos2(330.0, 160.0), pos2(130.0, 30.0)]);
        assert_eq!(style(&h.mark(0)), (Pen::Blue, 4));
        assert_eq!(h.selected(), Some(0));
        h.key(Key::Z, Modifiers::CTRL);
        assert_eq!(h.mark(0), tail_changed);
        h.key(Key::Z, Modifiers::CTRL);
        assert_eq!(h.mark(0), original);
        h.key(Key::Y, Modifiers::CTRL);
        assert_eq!(h.mark(0), tail_changed);
    }
}

#[test]
fn drawing_selected_strokes_move_resize_delete_and_walk_history() {
    let mut h = Harness::open("drawing-stroke-edits");
    edit::set_drawing_pen(false, Pen::Black);
    edit::set_width(false, 4);
    h.key(Key::P, Modifiers::NONE);
    h.stroke(&[
        pos2(50.0, 20.0),
        pos2(100.0, 20.0),
        pos2(100.0, 60.0),
        pos2(150.0, 60.0),
    ]);
    let original = h.mark(0);
    h.key(Key::Delete, Modifiers::NONE);
    assert_eq!(h.marks().len(), 1, "drawing does not select the new stroke");
    h.click(h.at(75.0, 20.0));
    assert_eq!(h.selected(), Some(0));
    h.drag(h.at(75.0, 20.0), h.at(95.0, 50.0), PointerButton::Primary);
    let moved = h.mark(0);
    points_near(
        &moved,
        &[
            pos2(70.0, 50.0),
            pos2(120.0, 50.0),
            pos2(120.0, 90.0),
            pos2(170.0, 90.0),
        ],
    );
    assert_eq!(moved.kind, original.kind);
    h.drag(h.at(170.0, 90.0), h.at(270.0, 90.0), PointerButton::Primary);
    let resized = h.mark(0);
    points_near(
        &resized,
        &[
            pos2(70.0, 50.0),
            pos2(170.0, 50.0),
            pos2(170.0, 130.0),
            pos2(270.0, 130.0),
        ],
    );
    assert_eq!(
        resized.kind, original.kind,
        "resize keeps the path and pen width"
    );
    assert_eq!(h.selected(), Some(0));
    h.key(Key::Delete, Modifiers::NONE);
    assert!(h.marks().is_empty());
    for expected in [&resized, &moved, &original] {
        h.key(Key::Z, Modifiers::CTRL);
        assert_eq!(&h.mark(0), expected);
    }
    h.key(Key::Z, Modifiers::CTRL);
    assert!(h.marks().is_empty());
    for expected in [&original, &moved, &resized] {
        h.key(Key::Y, Modifiers::CTRL);
        assert_eq!(&h.mark(0), expected);
    }
    h.key(Key::Y, Modifiers::CTRL);
    assert!(h.marks().is_empty());
}

#[test]
fn drawing_hits_follow_ink_including_arrowheads_not_the_bounding_box() {
    for (name, key) in [("pen", Key::P), ("arrow", Key::A)] {
        let mut h = Harness::open(&format!("drawing-hit-{name}"));
        edit::set_width(key == Key::A, 4);
        h.key(Key::H, Modifiers::NONE);
        h.stroke(&[pos2(30.0, 40.0), pos2(330.0, 170.0)]);
        let highlight = h.mark(0);
        h.key(key, Modifiers::NONE);
        let (path, on_ink, empty) = if key == Key::P {
            (
                vec![pos2(60.0, 60.0), pos2(300.0, 60.0), pos2(300.0, 150.0)],
                pos2(180.0, 60.0),
                pos2(80.0, 130.0),
            )
        } else {
            (
                vec![pos2(60.0, 60.0), pos2(300.0, 150.0)],
                pos2(180.0, 105.0),
                pos2(80.0, 130.0),
            )
        };
        h.stroke(&path);
        let drawing = h.mark(1);
        h.click(h.at(empty.x, empty.y));
        assert_eq!(
            h.selected(),
            Some(0),
            "empty space in the drawing's box hits the highlight below"
        );
        h.click(h.at(on_ink.x, on_ink.y));
        assert_eq!(h.selected(), Some(1), "the stroke is above the highlight");
        if key == Key::A {
            h.key(Key::Escape, Modifiers::NONE);
            let wing = drawing.drawing_paths()[1][0];
            h.click(h.at(wing.x, wing.y));
            assert_eq!(
                h.selected(),
                Some(1),
                "the open arrowhead is selectable too"
            );
        }
        h.stroke(&[empty, pos2(120.0, 145.0)]);
        assert_eq!(
            h.marks().len(),
            3,
            "dragging empty box space draws, rather than moving the selected stroke"
        );
        assert_eq!(h.mark(0), highlight);
        assert_eq!(h.mark(1), drawing);
    }
}

#[test]
fn drawing_flyouts_open_on_hold_or_right_click_without_toggling_and_dismiss_cleanly() {
    let mut h = Harness::open("drawing-flyout-open");
    h.show_drawing_toolbar();
    for (arrow, key, tool) in [(false, Key::P, Tool::Freehand), (true, Key::A, Tool::Arrow)] {
        h.key(key, Modifiers::NONE);
        let button = h.drawing_buttons[arrow as usize].center();
        h.frame(vec![Event::PointerMoved(button)]);
        h.button(button, PointerButton::Primary, true);
        h.time += 0.15;
        h.frame(vec![]);
        assert!(!h.panel_open(arrow), "a short hold is not a menu yet");
        h.time += 0.2;
        h.frame(vec![]);
        assert!(h.panel_open(arrow));
        h.time += 0.5;
        h.frame(vec![]);
        h.button(button, PointerButton::Primary, false);
        assert!(
            h.panel_open(arrow),
            "the long-press release does not close the panel"
        );
        assert_eq!(
            h.doc.tool,
            Some(tool),
            "the release does not toggle the active tool"
        );
        h.key(Key::Escape, Modifiers::NONE);
        assert!(!h.panel_open(arrow));
        assert_eq!(h.doc.tool, Some(tool), "Esc closes only the panel");

        h.open_drawing_panel(arrow);
        assert_eq!(h.doc.tool, Some(tool), "right-click does not toggle either");
        let outside = h.at(200.0, 100.0);
        h.frame(vec![Event::PointerMoved(outside)]);
        h.button(outside, PointerButton::Primary, true);
        assert!(
            !h.panel_open(arrow),
            "outside press closes without waiting for release"
        );
        h.button(outside, PointerButton::Primary, false);
        assert!(h.marks().is_empty());
        assert_eq!(h.doc.tool, Some(tool));

        h.click(button);
        assert_eq!(h.doc.tool, None, "ordinary clicks still toggle");
        h.button(button, PointerButton::Primary, true);
        h.time += 0.4;
        h.frame(vec![]);
        h.button(button, PointerButton::Primary, false);
        assert!(h.panel_open(arrow));
        assert_eq!(h.doc.tool, None, "holding an inactive tool does not arm it");
        h.key(Key::Escape, Modifiers::NONE);
        assert!(!h.panel_open(arrow));
    }
}

#[test]
fn drawing_flyout_colour_and_width_edit_only_the_matching_selected_kind_with_undo() {
    let mut h = Harness::open("drawing-flyout-selection");
    edit::set_drawing_pen(false, Pen::Red);
    edit::set_width(false, 4);
    edit::set_drawing_pen(true, Pen::Blue);
    edit::set_width(true, 2);
    h.draw_one();
    h.write(270.0, 10.0, "Hi");
    h.key(Key::Escape, Modifiers::NONE);
    h.key(Key::P, Modifiers::NONE);
    h.stroke(&[pos2(30.0, 110.0), pos2(100.0, 110.0), pos2(100.0, 160.0)]);
    h.key(Key::A, Modifiers::NONE);
    h.stroke(&[pos2(210.0, 110.0), pos2(340.0, 160.0)]);
    assert_eq!(h.marks().len(), 4);
    h.show_drawing_toolbar();
    let hits = [
        pos2(100.0, 60.0),
        pos2(275.0, 15.0),
        pos2(65.0, 110.0),
        pos2(275.0, 135.0),
    ];
    for (arrow, target, pen, width) in [(false, 2, Pen::Green, 8), (true, 3, Pen::White, 12)] {
        let before = h.doc.editor().unwrap().marks().to_vec();
        h.key(Key::Z, Modifiers::CTRL);
        let previous = h.doc.editor().unwrap().marks().to_vec();
        h.key(Key::Y, Modifiers::CTRL);
        for wrong_kind in [0, 1, if arrow { 2 } else { 3 }] {
            let hit = hits[wrong_kind];
            h.click(h.at(hit.x, hit.y));
            assert_eq!(h.selected(), Some(wrong_kind));
            h.choose_drawing_style(arrow, pen, width);
            assert_eq!(
                h.doc.editor().unwrap().marks(),
                before,
                "a different selected kind is untouched"
            );
            h.key(Key::Escape, Modifiers::NONE);
            assert_eq!(
                h.selected(),
                Some(wrong_kind),
                "Esc dismissed only the panel"
            );
        }
        h.key(Key::Z, Modifiers::CTRL);
        assert_eq!(
            h.doc.editor().unwrap().marks(),
            previous,
            "wrong-kind choices add no undo entries"
        );
        h.key(Key::Y, Modifiers::CTRL);
        assert_eq!(h.doc.editor().unwrap().marks(), before);

        let hit = hits[target];
        h.click(h.at(hit.x, hit.y));
        assert_eq!(h.selected(), Some(target));
        h.open_drawing_panel(arrow);
        h.click(h.colour_choice(pen));
        assert!(h.panel_open(arrow));
        let recoloured = h.mark(target);
        assert_eq!(style(&recoloured), (pen, style(&before[target]).1));
        points_near(&recoloured, &before[target].drawing_points());
        h.click(h.width_choice(width));
        assert!(h.panel_open(arrow));
        let restyled = h.mark(target);
        assert_eq!(style(&restyled), (pen, width));
        points_near(&restyled, &before[target].drawing_points());
        for (i, mark) in before.iter().enumerate() {
            if i != target {
                assert_eq!(h.mark(i), *mark);
            }
        }
        assert_eq!(
            h.doc.tool,
            Some(if arrow { Tool::Arrow } else { Tool::Freehand })
        );
        assert_eq!(h.selected(), Some(target));
        h.key(Key::Escape, Modifiers::NONE);
        h.key(Key::Z, Modifiers::CTRL);
        assert_eq!(h.mark(target), recoloured, "undo width");
        h.key(Key::Z, Modifiers::CTRL);
        assert_eq!(h.doc.editor().unwrap().marks(), before, "undo colour");
        h.key(Key::Y, Modifiers::CTRL);
        h.key(Key::Y, Modifiers::CTRL);
        assert_eq!(h.mark(target), restyled);
        assert_eq!(edit::current_drawing_pen(arrow), pen);
        assert_eq!(edit::current_width(arrow), width);
    }
}

#[test]
fn drawing_tools_remember_independent_colours_and_widths_for_new_marks() {
    let mut h = Harness::open("drawing-preferences");
    edit::set_drawing_pen(false, Pen::Red);
    edit::set_width(false, 4);
    edit::set_drawing_pen(true, Pen::Blue);
    edit::set_width(true, 2);
    let (ink, text_pen) = (edit::current_ink(), edit::current_pen());
    h.show_drawing_toolbar();
    h.choose_drawing_style(false, Pen::Green, 8);
    assert_eq!(h.doc.tool, Some(Tool::Freehand));
    assert_eq!(
        (edit::current_drawing_pen(true), edit::current_width(true)),
        (Pen::Blue, 2)
    );
    h.key(Key::Escape, Modifiers::NONE);
    h.choose_drawing_style(true, Pen::Black, 12);
    assert_eq!(h.doc.tool, Some(Tool::Arrow));
    h.key(Key::Escape, Modifiers::NONE);
    assert_eq!(
        (edit::current_drawing_pen(false), edit::current_width(false)),
        (Pen::Green, 8)
    );
    assert_eq!((edit::current_ink(), edit::current_pen()), (ink, text_pen));
    for (key, value) in [
        ("image.freehand.pen", "green"),
        ("image.freehand.width", "8"),
        ("image.arrow.pen", "black"),
        ("image.arrow.width", "12"),
    ] {
        assert_eq!(
            whir_core::settings::get_str(key).as_deref(),
            Some(value),
            "{key}"
        );
    }
    h.reopen();
    h.key(Key::P, Modifiers::NONE);
    h.stroke(&[pos2(40.0, 40.0), pos2(140.0, 80.0)]);
    h.key(Key::A, Modifiers::NONE);
    h.stroke(&[pos2(220.0, 140.0), pos2(340.0, 100.0)]);
    h.key(Key::P, Modifiers::NONE);
    h.click(h.at(60.0, 160.0));
    assert_eq!(h.marks().len(), 3);
    assert!(matches!(h.mark(0).kind, Kind::Freehand(_)));
    assert!(matches!(h.mark(1).kind, Kind::Arrow(_)));
    assert_eq!(style(&h.mark(0)), (Pen::Green, 8));
    assert_eq!(style(&h.mark(1)), (Pen::Black, 12));
    assert_eq!(style(&h.mark(2)), (Pen::Green, 8));
}

#[test]
fn drawing_mixed_capture_marks_reopen_and_put_ink_in_copies_and_exports() {
    let mut h = Harness::open_stored("drawing-mixed-stored", None);
    let original = std::fs::read(h.path()).unwrap();
    h.stroke(&[pos2(20.0, 20.0), pos2(120.0, 50.0)]);
    h.write(20.0, 120.0, "Mixed marks");
    h.key(Key::Escape, Modifiers::NONE);
    edit::set_drawing_pen(false, Pen::Green);
    edit::set_width(false, 8);
    h.key(Key::P, Modifiers::NONE);
    h.stroke(&[pos2(160.0, 30.0), pos2(160.0, 90.0), pos2(240.0, 90.0)]);
    edit::set_drawing_pen(true, Pen::Blue);
    edit::set_width(true, 8);
    h.key(Key::A, Modifiers::NONE);
    h.stroke(&[pos2(240.0, 150.0), pos2(360.0, 150.0)]);
    let expected = h.doc.editor().unwrap().marks().to_vec();
    assert_eq!(expected.len(), 4);
    assert!(matches!(expected[0].kind, Kind::Highlight(_)));
    assert!(matches!(expected[1].kind, Kind::Text(_)));
    assert!(matches!(expected[2].kind, Kind::Freehand(_)));
    assert!(matches!(expected[3].kind, Kind::Arrow(_)));
    h.saved();
    let saved = marks::parse(&std::fs::read_to_string(h.marks_file()).unwrap()).unwrap();
    assert_eq!(saved.size, [400, 200]);
    assert_eq!(saved.marks, expected);
    assert!(saved.other.is_empty());
    h.reopen();
    assert_eq!(h.doc.editor().unwrap().marks(), expected);
    assert!(!h.doc.is_dirty() && !h.doc.is_saving());
    assert_eq!(h.doc.save_error(), None);
    h.key(Key::P, Modifiers::NONE);
    h.click(h.at(160.0, 60.0));
    assert_eq!(h.selected(), Some(2), "the saved stroke is still editable");
    h.key(Key::R, Modifiers::NONE);
    h.frame(vec![Event::Copy]);
    let dib = h.copied();
    // A bent pen segment, the arrow shaft, and a wing away from the shaft.
    let ink = [
        (160, 60, Pen::Green),
        (300, 150, Pen::Blue),
        (343, 140, Pen::Blue),
    ];
    for (x, y, pen) in ink {
        let [r, g, b] = pen.rgb();
        assert_eq!(
            dib_pixel(&dib, 199 - y, x),
            ((200, 400), [b, g, r, 255]),
            "copied ({x}, {y})"
        );
    }
    assert_eq!(dib_pixel(&dib, 0, 0).1, [255; 4]);
    let dest = h.dir.join("drawing copy.png");
    assert!(matches!(h.doc.save_as(dest.clone()), SaveResult::Started));
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.doc.is_saving() {
        assert!(Instant::now() < deadline, "export timed out");
        std::thread::sleep(Duration::from_millis(2));
        h.doc.poll();
    }
    let exported = decode::decode(&dest, None).unwrap();
    assert_eq!((exported.width, exported.height), (200, 400));
    for (x, y, pen) in ink {
        let [r, g, b] = pen.rgb();
        let i = x * exported.stride + (199 - y) * 4;
        assert_eq!(
            &exported.pixels[i..i + 4],
            [b, g, r, 255],
            "exported ({x}, {y})"
        );
    }
    assert_eq!(&exported.pixels[..4], [255; 4]);
    assert_eq!(
        std::fs::read(h.path()).unwrap(),
        original,
        "the screenshot is never rewritten"
    );
    assert_eq!(
        h.doc.editor().unwrap().marks(),
        expected,
        "copy/export do not alter marks"
    );
}
