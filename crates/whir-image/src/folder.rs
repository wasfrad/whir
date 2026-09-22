//! Sibling images in a folder, in Explorer's natural sort order.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use windows::Win32::UI::Shell::StrCmpLogicalW;
use windows::core::{HSTRING, PCWSTR};

/// Explorer's name ordering ("img2" < "img10"), case-insensitive.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (HSTRING::from(a), HSTRING::from(b));
    let r = unsafe { StrCmpLogicalW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr())) };
    r.cmp(&0)
}

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| crate::EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// All supported images in `dir`, sorted like Explorer's default name sort.
pub fn list_images(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| is_image(p))
        .filter_map(|p| Some((p.file_name()?.to_string_lossy().into_owned(), p)))
        .collect();
    files.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    files.into_iter().map(|(_, p)| p).collect()
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["img10.png", "img2.png", "IMG1.png", "a.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a.png", "IMG1.png", "img2.png", "img10.png"]);
    }
}
