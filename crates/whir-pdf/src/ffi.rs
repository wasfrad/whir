//! Minimal PDFium bindings, resolved at runtime from `pdfium.dll`.
//!
//! Only what the viewer needs. Hand-rolled (instead of pdfium-render) so the
//! DLL is loaded lazily, on the PDF thread, the first time a PDF is opened —
//! and so there's no coupling to a particular PDFium version's feature flags.

use std::ffi::{c_char, c_void};
use std::path::PathBuf;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{HSTRING, PCSTR};

pub type Document = *mut c_void;
pub type Page = *mut c_void;
pub type Bitmap = *mut c_void;
pub type TextPage = *mut c_void;

pub const BITMAP_BGRA: i32 = 4;
pub const FLAG_ANNOT: i32 = 0x01;

pub const RENDER_TOBECONTINUED: i32 = 1;
pub const RENDER_DONE: i32 = 2;

pub const ERR_FILE: u32 = 2;
pub const ERR_FORMAT: u32 = 3;
pub const ERR_PASSWORD: u32 = 4;
pub const ERR_SECURITY: u32 = 5;

/// `FPDF_FILEACCESS`. `unsigned long` is 32-bit on Windows.
#[repr(C)]
pub struct FileAccess {
    pub file_len: u32,
    pub get_block: Option<
        unsafe extern "C" fn(param: *mut c_void, position: u32, buf: *mut u8, size: u32) -> i32,
    >,
    pub param: *mut c_void,
}

/// `IFSDK_PAUSE`.
#[repr(C)]
pub struct Pause {
    pub version: i32,
    pub need_to_pause_now: Option<unsafe extern "C" fn(this: *mut Pause) -> i32>,
    pub user: *mut c_void,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct SizeF {
    pub width: f32,
    pub height: f32,
}

/// `FS_RECTF` (field order as declared: left, top, right, bottom).
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// Text extraction (`fpdf_text.h`) plus the page transform. Optional: a
/// pdfium.dll without these still renders; selection and search are off.
pub struct TextApi {
    /// `FPDFText_LoadPage`: NULL on error.
    pub load_page: unsafe extern "C" fn(Page) -> TextPage,
    pub close_page: unsafe extern "C" fn(TextPage),
    /// −1 on error.
    pub count_chars: unsafe extern "C" fn(TextPage) -> i32,
    /// 0 when the character has no Unicode mapping.
    pub get_unicode: unsafe extern "C" fn(TextPage, i32) -> u32,
    /// 1 / 0 / −1 (error).
    pub is_generated: unsafe extern "C" fn(TextPage, i32) -> i32,
    /// 1 / 0 / −1 (error).
    pub is_hyphen: unsafe extern "C" fn(TextPage, i32) -> i32,
    /// Radians in [0, 2π), −1 on error.
    pub char_angle: unsafe extern "C" fn(TextPage, i32) -> f32,
    /// `FPDFText_GetLooseCharBox`: font ascent/descent box, page space.
    pub loose_box: unsafe extern "C" fn(TextPage, i32, *mut RectF) -> i32,
    /// `FPDFText_GetCharBox`: tight glyph box. Note the out-parameter order:
    /// left, *right*, *bottom*, top.
    pub char_box:
        unsafe extern "C" fn(TextPage, i32, *mut f64, *mut f64, *mut f64, *mut f64) -> i32,
    /// `FPDF_DeviceToPage(page, start_x, start_y, size_x, size_y, rotate,
    /// device_x, device_y, *page_x, *page_y)`.
    pub device_to_page:
        unsafe extern "C" fn(Page, i32, i32, i32, i32, i32, i32, i32, *mut f64, *mut f64) -> i32,
}

pub struct Api {
    _lib: HMODULE,
    pub init_library: unsafe extern "C" fn(),
    pub get_last_error: unsafe extern "C" fn() -> u32,
    pub load_custom_document: unsafe extern "C" fn(*mut FileAccess, *const c_char) -> Document,
    pub close_document: unsafe extern "C" fn(Document),
    pub get_page_count: unsafe extern "C" fn(Document) -> i32,
    pub get_page_size_by_index_f: unsafe extern "C" fn(Document, i32, *mut SizeF) -> i32,
    pub load_page: unsafe extern "C" fn(Document, i32) -> Page,
    pub close_page: unsafe extern "C" fn(Page),
    pub bitmap_create_ex: unsafe extern "C" fn(i32, i32, i32, *mut c_void, i32) -> Bitmap,
    pub bitmap_fill_rect: unsafe extern "C" fn(Bitmap, i32, i32, i32, i32, u32) -> i32,
    pub bitmap_destroy: unsafe extern "C" fn(Bitmap),
    pub render_start:
        unsafe extern "C" fn(Bitmap, Page, i32, i32, i32, i32, i32, i32, *mut Pause) -> i32,
    pub render_continue: unsafe extern "C" fn(Page, *mut Pause) -> i32,
    pub render_close: unsafe extern "C" fn(Page),
    pub text: Option<TextApi>,
}

// Function pointers into a DLL that is never unloaded.
unsafe impl Send for Api {}

fn exe_dir() -> Option<PathBuf> {
    let mut buf = vec![0u16; 32768];
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    (n > 0).then(|| {
        PathBuf::from(String::from_utf16_lossy(&buf[..n]))
            .parent()
            .map(PathBuf::from)
    })?
}

/// Where we look for pdfium.dll: next to whir.exe; in dev builds also `vendor/pdfium/bin`.
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(dir) = exe_dir() {
        v.push(dir.join("pdfium.dll"));
    }
    if cfg!(debug_assertions) {
        v.push(PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../vendor/pdfium/bin/pdfium.dll"
        )));
    }
    v
}

impl Api {
    // Each symbol is transmuted to the type of the field it's assigned to.
    #[allow(clippy::missing_transmute_annotations)]
    pub fn load() -> Result<Self, String> {
        let candidates = candidate_paths();
        let path = candidates
            .iter()
            .find(|p| p.exists())
            .ok_or_else(|| "pdfium.dll wasn't found next to whir.exe.".to_owned())?;
        // Full path + restricted search: never picks up a DLL from the current directory.
        let lib = unsafe {
            LoadLibraryExW(
                &HSTRING::from(path.as_os_str()),
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map_err(|e| format!("Couldn't load {}: {}", path.display(), e.message()))?;

        macro_rules! sym {
            ($name:literal) => {{
                let p = unsafe { GetProcAddress(lib, PCSTR(concat!($name, "\0").as_ptr())) };
                match p {
                    // SAFETY: the signature matches the PDFium header for this symbol.
                    Some(f) => unsafe {
                        std::mem::transmute::<unsafe extern "system" fn() -> isize, _>(f)
                    },
                    None => return Err(format!("pdfium.dll is missing {}", $name)),
                }
            }};
        }
        // Like `sym!`, but a missing symbol is `None` instead of an error.
        macro_rules! opt_sym {
            ($name:literal) => {{
                let p = unsafe { GetProcAddress(lib, PCSTR(concat!($name, "\0").as_ptr())) };
                // SAFETY: the signature matches the PDFium header for this symbol.
                p.map(|f| unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, _>(f)
                })
            }};
        }

        // All or nothing: half a text API is no text API.
        let text = (|| {
            Some(TextApi {
                load_page: opt_sym!("FPDFText_LoadPage")?,
                close_page: opt_sym!("FPDFText_ClosePage")?,
                count_chars: opt_sym!("FPDFText_CountChars")?,
                get_unicode: opt_sym!("FPDFText_GetUnicode")?,
                is_generated: opt_sym!("FPDFText_IsGenerated")?,
                is_hyphen: opt_sym!("FPDFText_IsHyphen")?,
                char_angle: opt_sym!("FPDFText_GetCharAngle")?,
                loose_box: opt_sym!("FPDFText_GetLooseCharBox")?,
                char_box: opt_sym!("FPDFText_GetCharBox")?,
                device_to_page: opt_sym!("FPDF_DeviceToPage")?,
            })
        })();

        Ok(Self {
            _lib: lib,
            init_library: sym!("FPDF_InitLibrary"),
            get_last_error: sym!("FPDF_GetLastError"),
            load_custom_document: sym!("FPDF_LoadCustomDocument"),
            close_document: sym!("FPDF_CloseDocument"),
            get_page_count: sym!("FPDF_GetPageCount"),
            get_page_size_by_index_f: sym!("FPDF_GetPageSizeByIndexF"),
            load_page: sym!("FPDF_LoadPage"),
            close_page: sym!("FPDF_ClosePage"),
            bitmap_create_ex: sym!("FPDFBitmap_CreateEx"),
            bitmap_fill_rect: sym!("FPDFBitmap_FillRect"),
            bitmap_destroy: sym!("FPDFBitmap_Destroy"),
            render_start: sym!("FPDF_RenderPageBitmap_Start"),
            render_continue: sym!("FPDF_RenderPage_Continue"),
            render_close: sym!("FPDF_RenderPage_Close"),
            text,
        })
    }

    pub fn last_error(&self) -> u32 {
        unsafe { (self.get_last_error)() }
    }
}

/// What to tell the user about `FPDF_GetLastError`'s `code`.
pub fn error_message(code: u32) -> String {
    match code {
        ERR_FILE => "The file couldn't be read.".into(),
        ERR_FORMAT => "This isn't a valid PDF, or it's damaged.".into(),
        ERR_SECURITY => "This PDF uses an unsupported security scheme.".into(),
        code => format!("PDFium couldn't open this PDF (error {code})."),
    }
}
