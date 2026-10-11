//! Headless egui tests: real frames through `Document::ui`, real key and
//! pointer events, checking the highlighter.

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
    /// Everything the document has asked of the shell.
    requests: Vec<DocRequest>,
    cx: OpenCx,
    dir: PathBuf,
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
        // No settings file is read or written by the tests.
        whir_core::settings::memory_only();
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
            ctx: egui::Context::default(),
            doc: Self::load(&path, &cx),
            time: 0.0,
            viewport: Rect::NOTHING,
            requests: Vec::new(),
            cx,
            dir,
        };
        h.frame(vec![]);
        h.frame(vec![]);
        h
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

    /// Wait for the highlights to be written.
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
            requests,
            ..
        } = self;
        let textures: Arc<dyn TextureRegistry> = Arc::new(NoTextures);
        let waker = Waker::noop();
        let out = ctx.run_ui(raw, |ui| {
            *viewport = ui.max_rect();
            let mut cx = DocCx {
                textures: &textures,
                waker: &waker,
                just_activated: false,
                requests,
            };
            doc.ui(ui, &mut cx);
        });
        out.drop_without_applying_deltas();
    }

    /// Where image pixel (`x`, `y`) is on screen.
    fn at(&self, x: f32, y: f32) -> Pos2 {
        let rect = self.doc.view.image_rect(self.viewport, IMAGE, 1.0);
        view::image_to_screen(pos2(x, y), rect, IMAGE, 0)
    }

    fn button(&mut self, pos: Pos2, button: PointerButton, pressed: bool) {
        self.frame(vec![Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        }]);
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
fn a_drag_draws_a_mark_only_while_the_tool_is_on() {
    let mut h = Harness::open("draw");
    let (a, b) = (h.at(50.0, 40.0), h.at(150.0, 80.0));
    h.drag(a, b, PointerButton::Primary);
    assert!(h.marks().is_empty());

    h.key(Key::H, Modifiers::NONE);
    assert!(h.doc.tool_on);
    h.drag(a, b, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 100, 40)]);
    // Drawn right to left and bottom to top: the same mark.
    h.drag(b, a, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 100, 40), (50, 40, 100, 40)]);
    // Nothing is selected by drawing.
    assert_eq!(h.selected(), None);

    h.key(Key::H, Modifiers::NONE);
    assert!(!h.doc.tool_on);
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
    assert!(h.doc.tool_on);
    h.key(Key::Escape, Modifiers::NONE);
    assert!(!h.doc.tool_on);
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

#[test]
fn drawn_marks_cover_whole_pixels() {
    let ink = Ink::Yellow;
    let m = drawn(pos2(10.4, 20.6), pos2(30.2, 25.1), IMAGE, 0.0, ink).unwrap();
    assert_eq!((m.x, m.y, m.w, m.h), (10, 20, 21, 6));
    // Too small a side.
    assert!(drawn(pos2(10.0, 20.0), pos2(30.0, 22.0), IMAGE, 4.0, ink).is_none());
    // Entirely on the edge: no pixels at all.
    assert!(drawn(pos2(400.0, 0.0), pos2(400.0, 50.0), IMAGE, 0.0, ink).is_none());
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
    assert!(h.doc.tool_on);
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
    assert!(h.doc.tool_on);
    let (a, b) = (h.at(50.0, 40.0), h.at(150.0, 80.0));
    h.drag(a, b, PointerButton::Primary);
    assert_eq!(h.marks(), [(50, 40, 100, 40)]);
    // Anywhere else, the highlighter waits to be picked up.
    assert!(!Harness::open("not-fresh").doc.tool_on);
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
    assert_eq!(h.doc.editor().unwrap().marks()[0].ink, Ink::Pink);
    assert!(!h.doc.is_dirty(), "loading isn't an edit");

    // The file was replaced by one of another size: its marks can't be placed.
    let stale = "whir-marks 1
size 999 999
rect 10 20 30 40 pink
";
    let h = Harness::open_stored("stale", Some(stale));
    assert!(h.marks().is_empty());
}
