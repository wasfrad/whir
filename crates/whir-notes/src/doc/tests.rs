//! The save lifecycle, with the real saver thread and real files: what ends up
//! on disk after the things a person does while a save is running.

use std::sync::Arc;

use whir_core::{Captures, TextureDesc, TextureRegistry, UserTexture};

use super::*;

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

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("whir-note-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cx(library: &Path) -> OpenCx {
    OpenCx {
        textures: Arc::new(NoTextures),
        waker: Waker::noop(),
        library: Library::at(library.to_path_buf(), Waker::noop()),
        captures: Captures::at(library.join("no-captures"), Waker::noop()),
    }
}

fn opened(path: PathBuf, cx: &OpenCx) -> NoteDoc {
    let mut doc = NoteDoc::open(path, cx);
    let until = Instant::now() + Duration::from_secs(5);
    while !matches!(doc.phase, Phase::Ready) {
        assert!(Instant::now() < until, "the note never loaded");
        doc.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    doc
}

fn set_body(doc: &mut NoteDoc, text: &str) {
    doc.body = text.to_owned();
    doc.touched();
}

/// Until nothing is being written: how the shell waits on a closed tab.
fn settle(doc: &mut NoteDoc) {
    let until = Instant::now() + Duration::from_secs(5);
    while doc.is_saving() {
        assert!(Instant::now() < until, "the save never finished");
        doc.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn an_emptied_note_leaves_no_file_and_undo_brings_one_back() {
    let d = dir("emptied");
    let cx = cx(&d);
    let path = d.join("Plan.md");
    std::fs::write(&path, "# Plan\n\nsomething I wanted gone\n").unwrap();
    let mut doc = opened(path.clone(), &cx);

    doc.title.clear();
    set_body(&mut doc, "");
    doc.flush();
    settle(&mut doc);
    assert!(!doc.is_dirty());
    assert!(!path.exists(), "the old text is still on disk");
    assert_eq!(doc.path, None);

    // Ctrl+Z, a moment later: the text is back, so a file is too.
    doc.title = "Plan".into();
    set_body(&mut doc, "something I wanted gone\n");
    doc.flush();
    settle(&mut doc);
    assert!(!doc.is_dirty());
    let again = doc.path.clone().expect("a new file");
    assert!(
        std::fs::read_to_string(again)
            .unwrap()
            .contains("something I wanted gone")
    );
}

#[test]
fn an_emptied_note_changed_elsewhere_is_left_alone() {
    let d = dir("emptied-theirs");
    let cx = cx(&d);
    let path = d.join("Plan.md");
    std::fs::write(&path, "# Plan\n\none\n").unwrap();
    let mut doc = opened(path.clone(), &cx);

    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&path, "# Plan\n\ntheirs, and longer\n").unwrap();
    doc.title.clear();
    set_body(&mut doc, "");
    doc.flush();
    settle(&mut doc);

    assert!(doc.conflict);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# Plan\n\ntheirs, and longer\n"
    );
}

/// `App::remove_tab` flushes, then `App::poll_zombies` waits for the saves to
/// finish and drops the document.
#[test]
fn closing_a_tab_during_a_save_keeps_what_was_typed_after_it() {
    let d = dir("close");
    let cx = cx(&d);
    let path = d.join("Plan.md");
    std::fs::write(&path, "# Plan\n\none\n").unwrap();
    let mut doc = opened(path.clone(), &cx);

    set_body(&mut doc, "one\ntwo\n");
    doc.flush(); // the autosave in flight
    assert!(doc.is_saving());
    set_body(&mut doc, "one\ntwo\nthree\n"); // typed since it started

    doc.flush();
    settle(&mut doc);
    assert_eq!(doc.save_error(), None);
    assert!(!doc.is_dirty());
    assert_eq!(doc.queued_toast, None, "no one asked to be told");
    drop(doc);

    assert!(saver::wait_idle(Instant::now() + Duration::from_secs(5)));
    assert!(std::fs::read_to_string(&path).unwrap().contains("three"));
}

#[test]
fn save_as_during_a_save_lands_at_the_new_file() {
    let d = dir("save-as");
    let cx = cx(&d.join("library"));
    let old = d.join("old.txt");
    let new = d.join("new.txt");
    std::fs::write(&old, "one\n").unwrap();
    let mut doc = opened(old.clone(), &cx);

    set_body(&mut doc, "one\ntwo");
    assert!(matches!(doc.save(), SaveResult::Started));
    assert!(doc.is_saving());
    assert!(matches!(doc.save_as(new.clone()), SaveResult::Started));
    settle(&mut doc);

    assert_eq!(doc.path.as_deref(), Some(new.as_path()));
    assert!(!doc.is_dirty());
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "one\ntwo\n");

    // Saves from here on go to the new file too.
    set_body(&mut doc, "one\ntwo\nthree");
    assert!(matches!(doc.save(), SaveResult::Started));
    settle(&mut doc);
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "one\ntwo\nthree\n");
}

#[test]
fn ctrl_s_during_a_save_still_says_saved() {
    let d = dir("resave");
    let cx = cx(&d.join("library"));
    let path = d.join("a.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut doc = opened(path.clone(), &cx);

    set_body(&mut doc, "two");
    assert!(matches!(doc.save(), SaveResult::Started));
    set_body(&mut doc, "three");
    assert!(matches!(doc.save(), SaveResult::Started));
    settle(&mut doc);

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\n");
    assert_eq!(doc.queued_toast.as_deref(), Some("Saved"));
}
