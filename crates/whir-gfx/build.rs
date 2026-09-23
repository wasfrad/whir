//! Compiles `shaders/*.hlsl` to DXBC at build time so the app never loads
//! d3dcompiler_47.dll (or parses HLSL) at runtime.
//!
//! `D3DCompile` is declared by hand instead of pulling the `windows` crate into
//! the build-dependency graph (which would compile it a second time).

use std::ffi::{CString, c_char, c_void};
use std::path::PathBuf;

#[repr(C)]
struct BlobVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut Blob) -> u32,
    get_buffer_pointer: unsafe extern "system" fn(*mut Blob) -> *mut c_void,
    get_buffer_size: unsafe extern "system" fn(*mut Blob) -> usize,
}

#[repr(C)]
struct Blob {
    vtbl: *const BlobVtbl,
}

#[link(name = "d3dcompiler_47", kind = "raw-dylib")]
unsafe extern "system" {
    fn D3DCompile(
        src: *const c_void,
        src_size: usize,
        source_name: *const c_char,
        defines: *const c_void,
        include: *const c_void,
        entry_point: *const c_char,
        target: *const c_char,
        flags1: u32,
        flags2: u32,
        code: *mut *mut Blob,
        errors: *mut *mut Blob,
    ) -> i32;
}

const D3DCOMPILE_ENABLE_STRICTNESS: u32 = 1 << 11;
const D3DCOMPILE_OPTIMIZATION_LEVEL3: u32 = 1 << 15;

unsafe fn blob_bytes(blob: *mut Blob) -> Vec<u8> {
    unsafe {
        let ptr = ((*(*blob).vtbl).get_buffer_pointer)(blob) as *const u8;
        let len = ((*(*blob).vtbl).get_buffer_size)(blob);
        let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
        ((*(*blob).vtbl).release)(blob);
        bytes
    }
}

fn compile(src: &str, name: &str, entry: &str, target: &str) -> Vec<u8> {
    let name_c = CString::new(name).unwrap();
    let entry_c = CString::new(entry).unwrap();
    let target_c = CString::new(target).unwrap();
    let mut code: *mut Blob = std::ptr::null_mut();
    let mut errors: *mut Blob = std::ptr::null_mut();
    let hr = unsafe {
        D3DCompile(
            src.as_ptr().cast(),
            src.len(),
            name_c.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            entry_c.as_ptr(),
            target_c.as_ptr(),
            D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            &mut errors,
        )
    };
    let messages = if errors.is_null() {
        String::new()
    } else {
        String::from_utf8_lossy(&unsafe { blob_bytes(errors) }).into_owned()
    };
    if hr < 0 || code.is_null() {
        panic!("failed to compile {name}:{entry} ({target}), hr=0x{hr:08x}\n{messages}");
    }
    if !messages.trim().is_empty() {
        println!("cargo:warning={name}:{entry}: {}", messages.trim());
    }
    unsafe { blob_bytes(code) }
}

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let path = "shaders/egui.hlsl";
    println!("cargo:rerun-if-changed={path}");
    let src = std::fs::read_to_string(path).unwrap();
    // level_9_3 targets keep WARP and very old GPUs working.
    std::fs::write(
        out.join("egui_vs.dxbc"),
        compile(&src, path, "vs_main", "vs_4_0_level_9_3"),
    )
    .unwrap();
    std::fs::write(
        out.join("egui_ps.dxbc"),
        compile(&src, path, "ps_main", "ps_4_0_level_9_3"),
    )
    .unwrap();
}
