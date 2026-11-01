//! A test binary of its own: the panic hook is the whole process's.

use whir_core::crash;

#[test]
fn a_panic_is_written_down() {
    let dir = std::env::temp_dir().join(format!("whir-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Nothing else runs in this process.
    unsafe { std::env::set_var("LOCALAPPDATA", &dir) };

    crash::install();
    let thread = std::thread::Builder::new()
        .name("the-one-that-fails".into())
        .spawn(|| panic!("this is the message"))
        .unwrap();
    assert!(thread.join().is_err());

    let log = std::fs::read_to_string(dir.join("whir").join("crash.log")).unwrap();
    assert!(log.contains("this is the message"), "{log}");
    assert!(log.contains("the-one-that-fails"), "{log}");
    assert!(log.contains("crash_log.rs"), "{log}");
    assert!(log.contains(env!("CARGO_PKG_VERSION")), "{log}");
}
