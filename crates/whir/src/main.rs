#![windows_subsystem = "windows"]
//! whir: a fast, lean viewer for images, PDFs and CSVs.
//!
//! Startup is the product: everything here is ordered so a double-clicked file
//! is on screen as early as possible.
//!   1. forward to a running instance if there is one (milliseconds, then exit)
//!   2. start the GPU device on a helper thread
//!   3. start loading the documents (decoding overlaps device + window creation)
//!   4. create the window cloaked, attach the renderer, render, then uncloak

mod app;
mod dispatch;
mod register;
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
}

fn parse_args() -> Args {
    let mut args = Args {
        files: Vec::new(),
        register: false,
        unregister: false,
        warp: false,
    };
    for a in std::env::args_os().skip(1) {
        match a.to_str() {
            Some("--register") => args.register = true,
            Some("--unregister") => args.unregister = true,
            Some("--warp") => args.warp = true,
            _ => args.files.push(PathBuf::from(a)),
        }
    }
    args
}

fn message_box(text: &str, error: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW};
    use windows::core::{HSTRING, w};
    let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("whir"), MB_OK | icon);
    }
}

fn main() {
    trace::init();
    trace::mark("main");
    let args = parse_args();

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
        message_box("whir's file associations were removed.", false);
        return;
    }

    let paths: Vec<PathBuf> = args
        .files
        .iter()
        .map(|p| whir_platform::ipc::canonicalize(p))
        .collect();
    let instance = match single_instance::acquire(&paths) {
        single_instance::Instance::Forwarded => {
            trace::mark("forwarded to running instance");
            trace::flush();
            return;
        }
        single_instance::Instance::Primary(guard) => guard,
    };
    trace::mark("primary instance");

    // GPU device creation (~10-50 ms of driver work) overlaps everything below.
    let warp = args.warp;
    let device_thread = std::thread::Builder::new()
        .name("gpu-init".into())
        .spawn(move || {
            let d = Device::create(warp);
            trace::mark("gpu device ready");
            d
        })
        .expect("spawn gpu-init");

    unsafe {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }

    let textures = GpuTextures::new();
    let wake = WakeSlot::new();
    let dark = whir_platform::os::system_uses_dark_theme();

    // Documents start loading now: decoders only need the GPU at upload time.
    let mut app = app::App::new(textures.clone(), wake.waker(), dark);
    use whir_platform::Handler as _;
    app.open_files(paths);
    trace::mark("documents started");

    let window = match Window::create(
        &WindowOptions {
            title: "whir".to_owned(),
            dark,
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

    let device = match device_thread.join() {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            textures.set_failed();
            message_box(&format!("No usable graphics device (D3D11): {e}"), true);
            return;
        }
        Err(_) => {
            textures.set_failed();
            return;
        }
    };
    let background = if dark {
        whir_core::theme::Theme::dark().strip_bg
    } else {
        whir_core::theme::Theme::light().strip_bg
    };
    match Renderer::new(device, textures.clone(), window.hwnd(), window.size_px(), background) {
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
    std::process::exit(code);
}
