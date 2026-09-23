//! Embeds the application manifest (DPI awareness, UTF-8 code page, segment
//! heap, long paths) and the icon/version resources into whir.exe.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    for f in ["whir.manifest", "whir.rc", "whir.ico"] {
        println!("cargo:rerun-if-changed={}", dir.join(f).display());
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        dir.join("whir.manifest").display()
    );

    match find_rc() {
        Some(rc) => {
            let res = out.join("whir.res");
            let status = Command::new(&rc)
                .current_dir(&dir)
                .args(["/nologo", "/fo"])
                .arg(&res)
                .arg("whir.rc")
                .status();
            match status {
                Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", res.display()),
                _ => println!("cargo:warning=rc.exe failed; whir.exe will have no icon"),
            }
        }
        None => {
            println!("cargo:warning=rc.exe (Windows SDK) not found; whir.exe will have no icon")
        }
    }
}

/// `RC` env var, else the newest x64 rc.exe in the Windows 10/11 SDK.
fn find_rc() -> Option<PathBuf> {
    if let Some(rc) = std::env::var_os("RC") {
        return Some(PathBuf::from(rc));
    }
    let base = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("x64").join("rc.exe").exists())
        .collect();
    versions.sort();
    versions.pop().map(|p| p.join("x64").join("rc.exe"))
}
