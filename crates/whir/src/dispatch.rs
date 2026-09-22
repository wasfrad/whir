//! Decide which pillar opens a file: extension first, then magic bytes.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use whir_core::{Document, MessageDoc, OpenCx};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Pdf,
    Csv,
}

pub fn kind_from_extension(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if whir_image::EXTENSIONS.contains(&ext.as_str()) {
        Some(Kind::Image)
    } else if whir_pdf::EXTENSIONS.contains(&ext.as_str()) {
        Some(Kind::Pdf)
    } else if whir_csv::EXTENSIONS.contains(&ext.as_str()) {
        Some(Kind::Csv)
    } else {
        None
    }
}

pub fn kind_from_magic(head: &[u8]) -> Option<Kind> {
    let starts = |sig: &[u8]| head.starts_with(sig);
    if starts(b"%PDF-") {
        Some(Kind::Pdf)
    } else if starts(b"\x89PNG\r\n\x1a\n")
        || starts(b"\xFF\xD8\xFF")
        || starts(b"GIF87a")
        || starts(b"GIF89a")
        || starts(b"BM")
        || starts(b"II*\0")
        || starts(b"MM\0*")
        || starts(b"\0\0\x01\0")
        || (head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP")
        || (head.len() >= 12 && &head[4..8] == b"ftyp")
    {
        Some(Kind::Image)
    } else {
        None
    }
}

fn sniff(path: &Path) -> Option<Kind> {
    let mut f = whir_core::file::open_shared(path).ok()?;
    let mut head = [0u8; 16];
    let n = f.read(&mut head).ok()?;
    kind_from_magic(&head[..n])
}

pub fn open(path: PathBuf, cx: &OpenCx) -> Box<dyn Document> {
    let kind = kind_from_extension(&path).or_else(|| sniff(&path));
    match kind {
        Some(Kind::Image) => whir_image::open(path, cx),
        Some(Kind::Pdf) => whir_pdf::open(path, cx),
        Some(Kind::Csv) => whir_csv::open(path, cx),
        None => {
            let detail = if path.exists() {
                "whir opens images, PDFs and CSV files."
            } else {
                "The file doesn't exist."
            };
            MessageDoc::new(Some(path), "Can't open this file", detail).boxed()
        }
    }
}

/// Filters for the Open dialog.
pub fn open_dialog_filters() -> Vec<(String, String)> {
    let pattern = |exts: &[&str]| {
        exts.iter()
            .map(|e| format!("*.{e}"))
            .collect::<Vec<_>>()
            .join(";")
    };
    let all: Vec<&str> = whir_image::EXTENSIONS
        .iter()
        .chain(whir_pdf::EXTENSIONS)
        .chain(whir_csv::EXTENSIONS)
        .copied()
        .collect();
    vec![
        ("Supported files".to_owned(), pattern(&all)),
        ("Images".to_owned(), pattern(whir_image::EXTENSIONS)),
        ("PDF documents".to_owned(), pattern(whir_pdf::EXTENSIONS)),
        ("CSV / TSV".to_owned(), pattern(whir_csv::EXTENSIONS)),
        ("All files".to_owned(), "*.*".to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(kind_from_extension(Path::new("a/B.PNG")), Some(Kind::Image));
        assert_eq!(kind_from_extension(Path::new("x.pdf")), Some(Kind::Pdf));
        assert_eq!(kind_from_extension(Path::new("x.tsv")), Some(Kind::Csv));
        assert_eq!(kind_from_extension(Path::new("x.docx")), None);
        assert_eq!(kind_from_extension(Path::new("noext")), None);
    }

    #[test]
    fn magic() {
        assert_eq!(kind_from_magic(b"%PDF-1.7\n"), Some(Kind::Pdf));
        assert_eq!(kind_from_magic(b"\x89PNG\r\n\x1a\n\0\0"), Some(Kind::Image));
        assert_eq!(kind_from_magic(b"\xFF\xD8\xFF\xE0"), Some(Kind::Image));
        assert_eq!(kind_from_magic(b"RIFF\0\0\0\0WEBPVP8 "), Some(Kind::Image));
        assert_eq!(kind_from_magic(b"a,b,c\n1,2,3"), None);
    }
}
