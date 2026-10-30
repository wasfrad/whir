//! Marks on an image: highlights (rectangles of ink that tint what is under
//! them the way a marker does, so dark text stays dark) and text. They are
//! kept as objects, never baked into the image, so they can be moved,
//! resized or removed later.
//!
//! Every mark has a box in image pixels: selecting, moving, resizing and
//! removing work the same for every kind.

use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_2};

use egui::emath::TSTransform;
use egui::epaint::{TextShape, WHITE_UV};
use egui::{Color32, FontId, Mesh, Pos2, Rect, Vec2, pos2, vec2};

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

/// The colour of a text mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pen {
    #[default]
    Red,
    Blue,
    Green,
    Black,
    White,
}

impl Pen {
    pub const ALL: [Pen; 5] = [Pen::Red, Pen::Blue, Pen::Green, Pen::Black, Pen::White];

    /// The name used in settings and `.marks` files.
    pub fn name(self) -> &'static str {
        match self {
            Pen::Red => "red",
            Pen::Blue => "blue",
            Pen::Green => "green",
            Pen::Black => "black",
            Pen::White => "white",
        }
    }

    pub fn from_name(name: &str) -> Option<Pen> {
        Pen::ALL.into_iter().find(|p| p.name() == name)
    }

    pub fn rgb(self) -> [u8; 3] {
        match self {
            Pen::Red => [0xe0, 0x22, 0x22],
            Pen::Blue => [0x1a, 0x5c, 0xd8],
            Pen::Green => [0x12, 0x88, 0x38],
            Pen::Black => [0x14, 0x14, 0x14],
            Pen::White => [0xff, 0xff, 0xff],
        }
    }

    pub fn color(self) -> Color32 {
        let [r, g, b] = self.rgb();
        Color32::from_rgb(r, g, b)
    }

    /// The outline that keeps it readable on a busy picture: light around
    /// the dark pens, dark around the white one.
    pub fn halo(self) -> Color32 {
        match self {
            Pen::White => Color32::from_rgb(0x20, 0x20, 0x20),
            _ => Color32::WHITE,
        }
    }
}

/// What a text mark says, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    /// Lines separated by `\n`.
    pub body: String,
    /// Font size, in image pixels.
    pub size: u32,
    pub pen: Pen,
    /// Quarter turns the image was turned on screen when this was typed: the
    /// text is upright when the image is turned that way.
    pub turn: u8,
}

impl Text {
    /// The smallest and largest font sizes, in image pixels. Big enough for a
    /// title across a 4K screenshot; small enough that its glyphs can be
    /// rasterised when the marks are baked.
    pub const MIN_SIZE: u32 = 6;
    pub const MAX_SIZE: u32 = 400;

    /// Pixels of halo around the letters (inside the mark's box).
    pub fn halo(&self) -> f32 {
        (self.size as f32 / 16.0).round().max(1.0)
    }

    /// Where the halo is drawn, around the letters (image pixels, whole).
    pub fn halo_offsets(&self) -> [Vec2; 8] {
        let h = self.halo();
        let d = (h * FRAC_1_SQRT_2).round().max(1.0);
        [
            vec2(h, 0.0),
            vec2(-h, 0.0),
            vec2(0.0, h),
            vec2(0.0, -h),
            vec2(d, d),
            vec2(d, -d),
            vec2(-d, d),
            vec2(-d, -d),
        ]
    }

    /// The font, `scale` points per image pixel.
    pub fn font(&self, scale: f32) -> FontId {
        FontId::proportional(self.size as f32 * scale)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Highlight(Ink),
    Text(Text),
}

/// One mark, its box in pixels of the full-resolution image (before any
/// on-screen rotation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub kind: Kind,
}

impl Mark {
    pub fn highlight(x: u32, y: u32, w: u32, h: u32, ink: Ink) -> Mark {
        Mark {
            x,
            y,
            w,
            h,
            kind: Kind::Highlight(ink),
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x as f32
            && y >= self.y as f32
            && x < (self.x + self.w) as f32
            && y < (self.y + self.h) as f32
    }

    pub fn text(&self) -> Option<&Text> {
        match &self.kind {
            Kind::Text(t) => Some(t),
            Kind::Highlight(_) => None,
        }
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
                    set.marks.push(Mark::highlight(x, y, w, h, ink));
                    continue;
                }
            }
            Some("text") => {
                if let Some(mark) = parse_text(line) {
                    set.marks.push(mark);
                    continue;
                }
            }
            _ => {}
        }
        set.other.push(line.to_owned());
    }
    Some(set)
}

/// `text <x> <y> <w> <h> <size> <turn> <pen> "<body>"`.
fn parse_text(line: &str) -> Option<Mark> {
    let mut rest = line.strip_prefix("text")?;
    let mut field = || {
        let s = rest.trim_start();
        let end = s.find(char::is_whitespace)?;
        rest = &s[end..];
        Some(&s[..end])
    };
    let mut nums = [0u32; 6];
    for n in &mut nums {
        *n = field()?.parse().ok()?;
    }
    let pen = Pen::from_name(field()?)?;
    let body = unquote(rest.trim())?;
    let [x, y, w, h, size, turn] = nums;
    let sizes = Text::MIN_SIZE..=Text::MAX_SIZE;
    if body.is_empty() || !sizes.contains(&size) || turn > 3 {
        return None;
    }
    Some(Mark {
        x,
        y,
        w,
        h,
        kind: Kind::Text(Text {
            body,
            size,
            pen,
            turn: turn as u8,
        }),
    })
}

/// `"…"` with `\\`, `\"`, `\n` and `\r` escaped: one line, whatever is in it.
pub(crate) fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(crate) fn unquote(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(match chars.next()? {
                '\\' => '\\',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            }),
            // Unescaped, it would have ended the string.
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

pub fn serialize(set: &MarkSet) -> String {
    let mut out = format!("{HEADER} 1\nsize {} {}\n", set.size[0], set.size[1]);
    for m in &set.marks {
        let (x, y, w, h) = (m.x, m.y, m.w, m.h);
        let line = match &m.kind {
            Kind::Highlight(ink) => format!("rect {x} {y} {w} {h} {}\n", ink.name()),
            Kind::Text(t) => format!(
                "text {x} {y} {w} {h} {} {} {} {}\n",
                t.size,
                t.turn,
                t.pen.name(),
                quote(&t.body)
            ),
        };
        out.push_str(&line);
    }
    for line in &set.other {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Draw one highlight over what is already painted in `rect` (screen points).
pub fn paint(painter: &egui::Painter, rect: Rect, ink: Ink) {
    let mut mesh = Mesh::with_texture(MULTIPLY);
    // Only the colour matters; the painter samples a white texel.
    let white = Rect::from_min_max(WHITE_UV, WHITE_UV);
    mesh.add_rect_with_uv(rect, white, ink.color());
    painter.add(mesh);
}

/// The corner of a mark's box (on screen) where its text starts, when the
/// text is turned `turns` quarters clockwise from upright.
pub fn text_origin(rect: Rect, turns: u8) -> Pos2 {
    match turns % 4 {
        0 => rect.min,
        1 => pos2(rect.max.x, rect.min.y),
        2 => rect.max,
        _ => pos2(rect.min.x, rect.max.y),
    }
}

/// `v` turned `turns` quarters clockwise (y points down).
pub fn turn_vec(v: Vec2, turns: u8) -> Vec2 {
    match turns % 4 {
        0 => v,
        1 => vec2(-v.y, v.x),
        2 => -v,
        _ => vec2(v.y, -v.x),
    }
}

/// The largest text on screen that is drawn from glyphs of its own size;
/// bigger (zoomed in a long way) is these glyphs scaled up. Each size is
/// rasterised into egui's font atlas, and a glyph wider than the atlas would
/// be fatal.
pub const SCREEN_TEXT_MAX: f32 = 128.0;

/// Draw a text mark whose box is `rect` on screen, turned `turns` quarters
/// clockwise from upright, `scale` points per image pixel: the halo, then
/// the letters.
pub fn paint_text(painter: &egui::Painter, rect: Rect, text: &Text, turns: u8, scale: f32) {
    let points = text.size as f32 * scale;
    let laid_out = points.min(SCREEN_TEXT_MAX);
    let font = FontId::proportional(laid_out);
    let galley = painter.layout_no_wrap(text.body.clone(), font, text.pen.color());
    let angle = (turns % 4) as f32 * FRAC_PI_2;
    let inset = turn_vec(Vec2::splat(text.halo() * scale), turns);
    let at = text_origin(rect, turns) + inset;
    let grow = points / laid_out;
    let place = |mut shape: TextShape| {
        if grow > 1.0 {
            // Scaled about its own corner.
            let p = shape.pos.to_vec2();
            shape.transform(TSTransform::new(p - p * grow, grow));
        }
        painter.add(shape);
    };
    let halo = text.pen.halo();
    for offset in text.halo_offsets() {
        let shape = TextShape::new(at + offset * scale, galley.clone(), halo)
            .with_override_text_color(halo)
            .with_angle(angle);
        place(shape);
    }
    place(TextShape::new(at, galley, text.pen.color()).with_angle(angle));
}

/// The upright size (image pixels) of the box around `body` at `size`
/// pixels, halo included. Laid out with the fonts on screen.
pub fn measure(ctx: &egui::Context, text: &Text) -> [u32; 2] {
    // Laid out no bigger than text on screen is (see `paint_text`): text
    // scales evenly, and big glyphs only crowd the font atlas.
    let laid_out = (text.size as f32).min(SCREEN_TEXT_MAX);
    let galley = ctx.fonts_mut(|f| {
        let font = FontId::proportional(laid_out);
        f.layout_no_wrap(text.body.clone(), font, Color32::WHITE)
    });
    let pad = 2.0 * text.halo();
    let size = galley.size() * (text.size as f32 / laid_out);
    [
        (size.x + pad).ceil().max(1.0) as u32,
        (size.y + pad).ceil().max(1.0) as u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ink_and_pen_names_round_trip() {
        for ink in Ink::ALL {
            assert_eq!(Ink::from_name(ink.name()), Some(ink));
        }
        assert_eq!(Ink::from_name("mauve"), None);
        for pen in Pen::ALL {
            assert_eq!(Pen::from_name(pen.name()), Some(pen));
        }
        assert_eq!(Pen::from_name("mauve"), None);
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
                Mark::highlight(120, 340, 600, 48, Ink::Yellow),
                Mark::highlight(0, 0, 10, 10, Ink::Pink),
            ]
        );
        assert!(set.other.is_empty());
        assert_eq!(serialize(&set), text);
    }

    #[test]
    fn text_survives_the_file_whatever_it_says() {
        let tricky = [
            "Hello",
            "two\nlines",
            "  spaced out  ",
            r#"a "quote" and a \ backslash"#,
            "trailing backslash \\",
            "\"",
            "ends with a newline\n",
            "carriage\r\nreturn",
        ];
        let set = MarkSet {
            size: [800, 600],
            marks: tricky
                .iter()
                .enumerate()
                .map(|(i, body)| Mark {
                    x: i as u32,
                    y: 2,
                    w: 30,
                    h: 40,
                    kind: Kind::Text(Text {
                        body: body.to_string(),
                        size: 24,
                        pen: Pen::ALL[i % 5],
                        turn: (i % 4) as u8,
                    }),
                })
                .collect(),
            other: Vec::new(),
        };
        let text = serialize(&set);
        assert_eq!(text.lines().count(), 2 + tricky.len(), "one line each");
        assert_eq!(parse(&text).unwrap(), set);
        assert!(text.contains("text 0 2 30 40 24 0 red \"Hello\"\n"));
        assert!(text.contains(r#"text 1 2 30 40 24 1 blue "two\nlines""#));
    }

    #[test]
    fn a_text_line_that_doesnt_make_sense_is_kept_as_it_is() {
        let bad = [
            "text 1 2 3 4 24 0 red Hello",
            "text 1 2 3 4 24 0 red \"unterminated",
            "text 1 2 3 4 24 0 red \"stray \" quote\"",
            "text 1 2 3 4 24 0 red \"bad \\x escape\"",
            "text 1 2 3 4 24 0 red \"\"",
            "text 1 2 3 4 24 4 red \"too turned\"",
            "text 1 2 3 4 0 0 red \"no size\"",
            "text 1 2 3 4 9000 0 red \"too big to draw\"",
            "text 1 2 3 4 24 0 mauve \"no such pen\"",
            "text 1 2 3 4 24 0",
        ];
        let file = format!("whir-marks 1\nsize 10 10\n{}\n", bad.join("\n"));
        let set = parse(&file).unwrap();
        assert!(set.marks.is_empty());
        assert_eq!(set.other, bad);
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
        let m = Mark::highlight(10, 20, 5, 5, Ink::Blue);
        assert!(m.contains(10.0, 20.0));
        assert!(m.contains(14.9, 24.9));
        assert!(!m.contains(15.0, 22.0));
        assert!(!m.contains(9.9, 22.0));
    }

    #[test]
    fn turned_text_starts_at_the_corner_its_first_letter_is_in() {
        let r = Rect::from_min_max(pos2(10.0, 20.0), pos2(30.0, 60.0));
        assert_eq!(text_origin(r, 0), pos2(10.0, 20.0));
        assert_eq!(text_origin(r, 1), pos2(30.0, 20.0));
        assert_eq!(text_origin(r, 2), pos2(30.0, 60.0));
        assert_eq!(text_origin(r, 3), pos2(10.0, 60.0));
        // Right turns into down, and so on round.
        assert_eq!(turn_vec(vec2(1.0, 0.0), 1), vec2(0.0, 1.0));
        assert_eq!(turn_vec(vec2(0.0, 1.0), 1), vec2(-1.0, 0.0));
        assert_eq!(turn_vec(vec2(1.0, 2.0), 2), vec2(-1.0, -2.0));
        assert_eq!(turn_vec(vec2(1.0, 0.0), 3), vec2(0.0, -1.0));
    }

    #[test]
    fn the_halo_grows_with_the_text() {
        let t = |size| Text {
            body: "x".into(),
            size,
            pen: Pen::Red,
            turn: 0,
        };
        assert_eq!(t(8).halo(), 1.0);
        assert_eq!(t(32).halo(), 2.0);
        assert_eq!(t(160).halo(), 10.0);
        assert!(
            t(160)
                .halo_offsets()
                .iter()
                .all(|o| o.length() <= 10.0 * 1.01)
        );
    }
}
