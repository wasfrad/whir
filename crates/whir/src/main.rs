#![windows_subsystem = "windows"]
//! whir: a fast, lean viewer and notepad for images, PDFs, CSVs and notes.
//!
//! Startup is the product: everything here is ordered so a double-clicked file
//! is on screen as early as possible.
//!   1. forward to a running instance if there is one (milliseconds, then exit)
//!   2. start the GPU device on a helper thread
//!   3. start loading the documents (decoding overlaps device + window creation)
//!   4. create the window cloaked, attach the renderer, render, then uncloak

mod app;
mod close_flow;
mod dispatch;
mod drawer;
#[cfg(test)]
mod gallery;
mod keys;
mod menus;
mod register;
mod reltime;
mod search_panel;
mod settings;
mod single_instance;
mod tab_bar;

use std::path::PathBuf;

use whir_core::trace;
use whir_gfx::{Device, GpuTextures, Renderer};
use whir_platform::{WakeSlot, Window, WindowOptions};

struct Args {
    files: Vec<PathBuf>,
    register: bool,
    unregister: bool,
    warp: bool,
    /// Relaunched after losing the GPU device.
    restarted: bool,
    /// `--dock` and friends: this process is the capture dock, not a window.
    dock: Option<whir_capture::Command>,
    /// `--settings [page]`: open Settings (at that page).
    settings: Option<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        files: Vec::new(),
        register: false,
        unregister: false,
        warp: false,
        restarted: false,
        dock: None,
        settings: None,
    };
    let mut all = std::env::args_os().skip(1).peekable();
    while let Some(a) = all.next() {
        match a.to_str() {
            Some("--register") => args.register = true,
            Some("--unregister") => args.unregister = true,
            Some("--warp") => args.warp = true,
            Some("--restarted") => args.restarted = true,
            Some("--settings") => {
                // The page, if one is named.
                let page = all
                    .next_if(|p| {
                        p.to_str()
                            .is_some_and(|p| settings::Section::from_name(p).is_some())
                    })
                    .and_then(|p| p.into_string().ok());
                args.settings = Some(page.unwrap_or_default());
            }
            Some(s) if whir_capture::Command::from_arg(s).is_some() => {
                args.dock = whir_capture::Command::from_arg(s);
            }
            _ => args.files.push(PathBuf::from(a)),
        }
    }
    args
}

/// Relaunch after a lost GPU device. A second loss in a row falls back to
/// software rendering (WARP) so a broken driver can't cause a restart loop.
fn relaunch(paths: &[PathBuf], previous: &Args) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--restarted");
    if previous.restarted || previous.warp {
        cmd.arg("--warp");
    }
    cmd.args(paths);
    let _ = cmd.spawn();
}

/// A native Yes/No question (used when the GPU is gone and egui can't draw).
pub(crate) fn message_yes_no(text: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW};
    use windows::core::{HSTRING, w};
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            w!("whir"),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

fn message_box(text: &str, error: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW,
    };
    use windows::core::{HSTRING, w};
    let icon = if error {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("whir"), MB_OK | icon);
    }
}

fn main() {
    trace::init();
    trace::mark("main");
    let args = parse_args();

    // The capture dock is a process of its own: no window, no GPU, and none
    // of the single-instance hand-off below.
    if let Some(cmd) = args.dock {
        std::process::exit(whir_capture::run(cmd));
    }

    if args.register {
        match register::register() {
            Ok(n) => message_box(
                &format!(
                    "whir is now available for {n} file types.\n\n\
                     To make it the default, right-click a file → Open with → \
                     Choose another app → whir, and tick \"Always\"."
                ),
                false,
            ),
            Err(e) => message_box(&format!("Registration failed: {e}"), true),
        }
        return;
    }
    if args.unregister {
        register::unregister();
        message_box(
            "whir's file associations were removed, and it no longer starts with Windows.",
            false,
        );
        return;
    }

    // Flagging a note tells the dock (it shows the todos).
    whir_core::todos::on_change(|| {
        whir_platform::ipc::notify_changed(whir_platform::ipc::Changed::Todos)
    });

    let paths: Vec<PathBuf> = args
        .files
        .iter()
        .map(|p| whir_platform::ipc::canonicalize(p))
        .collect();
    let instance = match single_instance::acquire(&paths, args.settings.as_deref()) {
        single_instance::Instance::Forwarded => {
            trace::mark("forwarded to running instance");
            trace::flush();
            return;
        }
        single_instance::Instance::Primary(guard) => guard,
    };
    trace::mark("primary instance");

    // GPU device creation (driver load: 30-150+ ms) overlaps everything below.
    let warp = args.warp;
    let (device_tx, device_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("gpu-init".into())
        .spawn(move || {
            let d = Device::create(warp);
            trace::mark("gpu device ready");
            let _ = device_tx.send(d);
        })
        .expect("spawn gpu-init");

    unsafe {
        use windows::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
        };
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }

    let textures = GpuTextures::new();
    let wake = WakeSlot::new();
    trace::mark("com initialized");
    let dark = whir_platform::os::prefers_dark();

    // Documents start loading now: decoders only need the GPU at upload time.
    let restart = std::rc::Rc::new(std::cell::RefCell::new(None));
    let mut app = app::App::new(textures.clone(), wake.waker(), dark, restart.clone());
    use whir_platform::Handler as _;
    if !paths.is_empty() {
        app.open_files(paths);
    }
    if let Some(page) = &args.settings {
        app.open_settings(page);
    } else if args.files.is_empty() {
        // A bare launch is a blank note, ready to type into (no disk access).
        app.bare_launch();
    }
    trace::mark("documents started");

    // Where the window was left (settings.ini: a small file, likely cached).
    let placement = whir_platform::placement::saved();
    let exact = whir_platform::placement::exact();
    trace::mark("window placement read");
    let window = match Window::create(
        &WindowOptions {
            title: "whir".to_owned(),
            dark,
            placement,
            exact,
        },
        wake,
    ) {
        Ok(w) => w,
        Err(e) => {
            message_box(&format!("Couldn't create the window: {e}"), true);
            return;
        }
    };
    trace::mark("window created");

    let theme = if dark {
        whir_core::theme::Theme::dark()
    } else {
        whir_core::theme::Theme::light()
    };
    // A slow GPU driver shouldn't delay the window itself: after a short grace
    // period, show a placeholder in the app's colours until the device is ready.
    let device = match device_rx.recv_timeout(std::time::Duration::from_millis(40)) {
        Ok(d) => Ok(d),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let strip_px =
                (theme.metrics.tab_strip_height * window.pixels_per_point()).round() as i32;
            window.show_placeholder(theme.strip_bg, theme.content_bg, strip_px);
            trace::mark("placeholder shown");
            device_rx.recv().map_err(|_| ())
        }
        Err(_) => Err(()),
    };
    let device = match device {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            textures.set_failed();
            message_box(&format!("No usable graphics device (D3D11): {e}"), true);
            return;
        }
        Err(()) => {
            textures.set_failed();
            return;
        }
    };
    let background = theme.strip_bg;
    match Renderer::new(
        device,
        textures.clone(),
        window.hwnd(),
        window.size_px(),
        background,
    ) {
        Ok(r) => app.set_renderer(r),
        Err(e) => {
            textures.set_failed();
            message_box(&format!("Couldn't initialize rendering: {e}"), true);
            return;
        }
    }
    trace::mark("renderer ready");

    let code = window.run(Box::new(app));
    trace::mark("exit");
    trace::flush();
    // Release the single-instance mutex, then skip slow teardown of GPU/COM objects.
    drop(instance);
    // Let the last writes land (process::exit would cut them off): notes, and
    // the settings and views (the window's place, where each PDF was left).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    whir_notes::saver::wait_idle(deadline);
    whir_core::settings::wait_idle(deadline);
    if let Some(paths) = restart.borrow_mut().take() {
        relaunch(&paths, &args);
    }
    std::process::exit(code);
}
