//! Highlights on an image: rectangles of ink that tint what is under them the
//! way a marker does (dark text stays dark). They are kept as objects, never
//! baked into the image, so they can be moved or removed later.

use egui::epaint::WHITE_UV;
use egui::{Color32, Mesh, Rect};

use crate::texture::MULTIPLY;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ink {
    #[default]
    Yellow,
    Green,
    Pink,
    Blue,
    Orange,
}

impl Ink {
    pub const ALL: [Ink; 5] = [Ink::Yellow, Ink::Green, Ink::Pink, Ink::Blue, Ink::Orange];

    /// The name used in settings and `.marks` files.
    pub fn name(self) -> &'static str {
        match self {
            Ink::Yellow => "yellow",
            Ink::Green => "green",
            Ink::Pink => "pink",
            Ink::Blue => "blue",
            Ink::Orange => "orange",
        }
    }

    pub fn from_name(name: &str) -> Option<Ink> {
        Ink::ALL.into_iter().find(|i| i.name() == name)
    }

    /// What the image is multiplied by (so white paper takes exactly this colour).
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Ink::Yellow => [0xff, 0xf0, 0x50],
            Ink::Green => [0x8c, 0xf5, 0x82],
            Ink::Pink => [0xff, 0xa0, 0xcd],
            Ink::Blue => [0x82, 0xd7, 0xff],
            Ink::Orange => [0xff, 0xbe, 0x5a],
        }
    }

    pub fn color(self) -> Color32 {
        let [r, g, b] = self.rgb();
        Color32::from_rgb(r, g, b)
    }
}

/// One highlight, in pixels of the full-resolution image (before any
/// on-screen rotation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub ink: Ink,
}

impl Mark {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32
            && y >= self.y as f32
            && x < (self.x + self.w) as f32
            && y < (self.y + self.h) as f32
    }
}

/// The marks on one image, as its `.marks` file holds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkSet {
    /// Pixel size of the image the marks were drawn on.
    pub size: [u32; 2],
    /// Bottom to top.
    pub marks: Vec<Mark>,
    /// Lines this build doesn't understand (a newer whir's), kept as they are.
    pub other: Vec<String>,
}

const HEADER: &str = "whir-marks";

/// `None` if `text` isn't a marks file.
pub fn parse(text: &str) -> Option<MarkSet> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let mut header = lines.next()?.split_whitespace();
    if header.next() != Some(HEADER) {
        return None;
    }
    let mut set = MarkSet::default();
    for line in lines {
        let words: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| words.get(i)?.parse::<u32>().ok();
        match words.first().copied() {
            Some("size") if words.len() == 3 => {
                if let (Some(w), Some(h)) = (num(1), num(2)) {
                    set.size = [w, h];
                    continue;
                }
            }
            Some("rect") if words.len() == 6 => {
                if let (Some(x), Some(y), Some(w), Some(h), Some(ink)) =
                    (num(1), num(2), num(3), num(4), Ink::from_name(words[5]))
                {
                    set.marks.push(Mark { x, y, w, h, ink });
                    continue;
                }
            }
            _ => {}
        }
        set.other.push(line.to_owned());
    }
    Some(set)
}

pub fn serialize(set: &MarkSet) -> String {
    let mut out = format!("{HEADER} 1\nsize {} {}\n", set.size[0], set.size[1]);
    for m in &set.marks {
        out.push_str(&format!(
            "rect {} {} {} {} {}\n",
            m.x,
            m.y,
            m.w,
            m.h,
            m.ink.name()
        ));
    }
    for line in &set.other {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Draw one mark over what is already painted in `rect` (screen points).
pub fn paint(painter: &egui::Painter, rect: Rect, ink: Ink) {
    let mut mesh = Mesh::with_texture(MULTIPLY);
    // Only the colour matters; the painter samples a white texel.
    let white = Rect::from_min_max(WHITE_UV, WHITE_UV);
    mesh.add_rect_with_uv(rect, white, ink.color());
    painter.add(mesh);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_names_round_trip() {
        for ink in Ink::ALL {
            assert_eq!(Ink::from_name(ink.name()), Some(ink));
        }
        assert_eq!(Ink::from_name("mauve"), None);
    }

    #[test]
    fn parse_and_serialize() {
        let text = "whir-marks 1\nsize 2560 1440\nrect 120 340 600 48 yellow\n\
                    rect 0 0 10 10 pink\n";
        let set = parse(text).unwrap();
        assert_eq!(set.size, [2560, 1440]);
        assert_eq!(
            set.marks,
            vec![
                Mark {
                    x: 120,
                    y: 340,
                    w: 600,
                    h: 48,
                    ink: Ink::Yellow
                },
                Mark {
                    x: 0,
                    y: 0,
                    w: 10,
                    h: 10,
                    ink: Ink::Pink
                },
            ]
        );
        assert!(set.other.is_empty());
        assert_eq!(serialize(&set), text);
    }

    #[test]
    fn lines_from_a_newer_whir_survive() {
        let text = "whir-marks 2\n\n  size 100 50  \nrect 1 2 3 4 mauve\narrow 1 2 3 4\n\
                    rect 1 2 3 green\nrect 5 6 7 8 green\n";
        let set = parse(text).unwrap();
        assert_eq!(set.size, [100, 50]);
        assert_eq!(set.marks.len(), 1);
        assert_eq!(
            set.other,
            ["rect 1 2 3 4 mauve", "arrow 1 2 3 4", "rect 1 2 3 green"]
        );
        // Written back, then read again: nothing more is lost.
        assert_eq!(parse(&serialize(&set)).unwrap(), set);
    }

    #[test]
    fn other_files_are_refused() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("# whir settings\na = b\n"), None);
    }

    #[test]
    fn a_mark_contains_its_pixels_only() {
        let m = Mark {
            x: 10,
            y: 20,
            w: 5,
            h: 5,
            ink: Ink::Blue,
        };
        assert!(m.contains(10.0, 20.0));
        assert!(m.contains(14.9, 24.9));
        assert!(!m.contains(15.0, 22.0));
        assert!(!m.contains(9.9, 22.0));
    }
}
