//! Drawing with egui and no renderer. egui lays out and tessellates as it does
//! in whir's window, with the same theme, controls and fonts; here its triangles
//! are filled on the CPU, into the bitmap a layered window shows.
//!
//! The sums are those of `whir-gfx`'s pixel shader and blend state (gamma space,
//! premultiplied alpha), so the dock looks as the window does, while nothing of
//! the GPU's is loaded into the always-running dock: a Direct3D device would
//! take it from a few megabytes to some sixty, nearly all of it the driver.
//!
//! Each window has a stage. Nothing runs unless something happened: a frame is
//! drawn for an event, or when egui asks for one to carry on a fade.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use egui::epaint::{ClippedPrimitive, ImageData, Primitive, Vertex};
use egui::{Color32, Context, Event, Modifiers, PointerButton, Pos2, TextureId, Ui, ViewportId};
use whir_core::theme::{FONTS, Theme};
use whir_platform::fonts;
use whir_platform::input::InputCollector;

use crate::geom::Point;
use crate::paint::Surface;

/// Longer than any fade: past this a frame isn't waited for.
const FRAME_WAIT_LIMIT: Duration = Duration::from_secs(60);

struct Texture {
    width: usize,
    height: usize,
    texels: Vec<Color32>,
}

/// What came of a frame.
pub struct Drew {
    /// The picture isn't what it was.
    pub changed: bool,
    /// Draw another this much later: something is fading.
    pub again: Option<Duration>,
}

pub struct Stage {
    ctx: Context,
    input: InputCollector,
    textures: HashMap<TextureId, Texture>,
    surface: Option<Surface>,
    /// A fingerprint of what's on the surface.
    drawn: u64,
}

impl Stage {
    pub fn new(theme: &Theme, scale: f32) -> Stage {
        let ctx = Context::default();
        ctx.set_fonts(fonts::definitions(FONTS));
        theme.apply(&ctx);
        ctx.options_mut(|o| {
            // Down and up on a button is a click, however long it took.
            o.input_options.max_click_duration = f64::MAX;
            o.max_passes = std::num::NonZeroUsize::MIN;
        });
        Stage {
            ctx,
            input: InputCollector::new(scale),
            textures: HashMap::new(),
            surface: None,
            drawn: 0,
        }
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        theme.apply(&self.ctx);
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.input.pixels_per_point = scale;
    }

    pub fn surface(&self) -> Option<&Surface> {
        self.surface.as_ref()
    }

    /// The pointer is at `at`, in the surface's pixels.
    pub fn pointer_moved(&mut self, at: Point) {
        self.input.pointer_moved(at.0, at.1);
    }

    pub fn pointer_gone(&mut self) {
        self.input.pointer_gone();
    }

    pub fn button(&mut self, at: Point, pressed: bool) {
        self.input
            .pointer_button(at.0, at.1, PointerButton::Primary, pressed);
    }

    /// `notches` of the wheel, away from the user.
    pub fn wheel(&mut self, notches: f32) {
        self.input.wheel(0.0, notches);
    }

    /// A press that won't become a click: the dock is being dragged by it.
    pub fn call_off_press(&mut self) {
        let away = Pos2::new(-1000.0, -1000.0);
        self.input.push_event(Event::PointerMoved(away));
        self.input.push_event(Event::PointerButton {
            pos: away,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        self.input.push_event(Event::PointerGone);
    }

    /// One frame, `size` pixels. `add` draws it, and is told of what's been
    /// clicked the way egui tells anything.
    pub fn run(&mut self, size: (i32, i32), add: impl FnMut(&mut Ui)) -> Drew {
        let (width, height) = (size.0.max(1), size.1.max(1));
        let raw = self.input.take_raw_input([width as u32, height as u32]);
        let mut full = self.ctx.run_ui(raw, add);
        let scale = full.pixels_per_point;
        let again = full
            .viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .filter(|d| *d < FRAME_WAIT_LIMIT);
        let primitives = self.ctx.tessellate(std::mem::take(&mut full.shapes), scale);

        let mut uploaded = false;
        for (id, images) in full.textures_delta.set.drain() {
            for image in &images {
                uploaded = true;
                self.upload(id, image);
            }
        }
        let print = fingerprint(&primitives, width, height);
        let changed = uploaded || print != self.drawn || self.surface.is_none();
        if changed {
            self.draw(&primitives, width, height, scale);
            self.drawn = print;
        }
        for id in full.textures_delta.free.drain() {
            self.textures.remove(&id);
        }
        Drew { changed, again }
    }

    fn upload(&mut self, id: TextureId, image: &egui::epaint::ImageDelta) {
        let ImageData::Color(picture) = &image.image;
        let [width, height] = picture.size;
        match image.pos {
            None => {
                let texels = picture.pixels.clone();
                self.textures.insert(
                    id,
                    Texture {
                        width,
                        height,
                        texels,
                    },
                );
            }
            Some([x, y]) => {
                let Some(texture) = self.textures.get_mut(&id) else {
                    return;
                };
                for row in 0..height {
                    let to = (y + row) * texture.width + x;
                    let from = &picture.pixels[row * width..][..width];
                    texture.texels[to..to + width].copy_from_slice(from);
                }
            }
        }
    }

    fn draw(&mut self, primitives: &[ClippedPrimitive], width: i32, height: i32, scale: f32) {
        let fits = self
            .surface
            .as_ref()
            .is_some_and(|s| s.width == width && s.height == height);
        if !fits {
            self.surface = Surface::new(width, height);
        }
        let Some(surface) = &mut self.surface else {
            return;
        };
        let px = surface.pixels();
        px.fill(0);
        let mut target = Target { px, width, height };
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                continue;
            };
            let Some(texture) = self.textures.get(&mesh.texture_id) else {
                continue;
            };
            let clip = primitive.clip_rect;
            let bounds = [
                (clip.min.x * scale).round().max(0.0) as i32,
                (clip.min.y * scale).round().max(0.0) as i32,
                ((clip.max.x * scale).round() as i32).min(width),
                ((clip.max.y * scale).round() as i32).min(height),
            ];
            for corners in mesh.indices.chunks_exact(3) {
                let corner = |i: usize| &mesh.vertices[corners[i] as usize];
                target.triangle([corner(0), corner(1), corner(2)], texture, bounds, scale);
            }
        }
    }
}

/// Whether two frames would come out the same: moving the pointer over nothing
/// costs a frame of egui, but not a picture.
fn fingerprint(primitives: &[ClippedPrimitive], width: i32, height: i32) -> u64 {
    let mut hash = std::hash::DefaultHasher::new();
    (width, height).hash(&mut hash);
    for primitive in primitives {
        let clip = primitive.clip_rect;
        [clip.min.x, clip.min.y, clip.max.x, clip.max.y]
            .map(f32::to_bits)
            .hash(&mut hash);
        if let Primitive::Mesh(mesh) = &primitive.primitive {
            mesh.indices.hash(&mut hash);
            for v in &mesh.vertices {
                [v.pos.x, v.pos.y, v.uv.x, v.uv.y]
                    .map(f32::to_bits)
                    .hash(&mut hash);
                v.color.to_array().hash(&mut hash);
            }
        }
    }
    hash.finish()
}

struct Target<'a> {
    px: &'a mut [u32],
    width: i32,
    height: i32,
}

impl Target<'_> {
    /// Fill a triangle: row by row, only the pixels in it (a rounded shape
    /// tessellates to a fan of long triangles, most of whose boxes are empty).
    fn triangle(
        &mut self,
        [a, b, c]: [&Vertex; 3],
        texture: &Texture,
        bounds: [i32; 4],
        scale: f32,
    ) {
        let [(ax, ay), (bx, by), (cx, cy)] = [a, b, c].map(|v| (v.pos.x * scale, v.pos.y * scale));
        let area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        if area == 0.0 {
            return;
        }
        let s = area.signum();
        let area = area.abs();
        // Each side, opposite a corner: a point on it and its direction, turned
        // so that inside is positive. A pixel exactly on a side two triangles
        // share is drawn by one of them, or text would be doubled along it.
        let sides = [
            (bx, by, s * (cx - bx), s * (cy - by)),
            (cx, cy, s * (ax - cx), s * (ay - cy)),
            (ax, ay, s * (bx - ax), s * (by - ay)),
        ];
        let owns = sides.map(|(_, _, dx, dy)| (dy == 0.0 && dx > 0.0) || dy < 0.0);

        let colours = [a.color, b.color, c.color].map(|c| c.to_normalized_gamma_f32());
        let plain_colour = a.color == b.color && b.color == c.color;
        let plain_texel = a.uv == b.uv && b.uv == c.uv;
        let same_texel = sample(texture, a.uv);

        let top = (ay.min(by).min(cy).floor() as i32).max(bounds[1]).max(0);
        let bottom = (ay.max(by).max(cy).ceil() as i32)
            .min(bounds[3])
            .min(self.height);
        for y in top..bottom {
            let py = y as f32 + 0.5;
            // Where this row is inside all three sides.
            let (mut from, mut to) = (f32::NEG_INFINITY, f32::INFINITY);
            let mut none = false;
            for (x0, y0, dx, dy) in sides {
                let k = dx * (py - y0) + dy * x0;
                if dy > 0.0 {
                    to = to.min(k / dy);
                } else if dy < 0.0 {
                    from = from.max(k / dy);
                } else if k < 0.0 {
                    none = true;
                }
            }
            if none || from > to {
                continue;
            }
            // A pixel's slack either way: the test below is what's exact.
            let left = ((from - 1.5).floor().max(-1.0) as i32)
                .max(bounds[0])
                .max(0);
            let right = (((to + 1.5).ceil()).min(1e7) as i32)
                .min(bounds[2])
                .min(self.width);
            for x in left..right {
                let px = x as f32 + 0.5;
                let mut w = [0.0; 3];
                let mut inside = true;
                for (i, (x0, y0, dx, dy)) in sides.into_iter().enumerate() {
                    w[i] = dx * (py - y0) - dy * (px - x0);
                    inside &= w[i] > 0.0 || (w[i] == 0.0 && owns[i]);
                }
                if !inside {
                    continue;
                }
                let l = w.map(|v| v / area);
                let tint = if plain_colour {
                    colours[0]
                } else {
                    std::array::from_fn(|i| {
                        l[0] * colours[0][i] + l[1] * colours[1][i] + l[2] * colours[2][i]
                    })
                };
                let texel = if plain_texel {
                    same_texel
                } else {
                    let u = l[0] * a.uv.x + l[1] * b.uv.x + l[2] * c.uv.x;
                    let v = l[0] * a.uv.y + l[1] * b.uv.y + l[2] * c.uv.y;
                    sample(texture, Pos2::new(u, v))
                };
                let source: [f32; 4] = std::array::from_fn(|i| texel[i] * tint[i]);
                let at = &mut self.px[(y * self.width + x) as usize];
                *at = over(*at, source);
            }
        }
    }
}

/// Bilinear, as the GPU samples it.
fn sample(texture: &Texture, uv: Pos2) -> [f32; 4] {
    let (w, h) = (texture.width as i64, texture.height as i64);
    let (x, y) = (uv.x * w as f32 - 0.5, uv.y * h as f32 - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let texel = |dx: i64, dy: i64| {
        let tx = (x0 as i64 + dx).clamp(0, w - 1);
        let ty = (y0 as i64 + dy).clamp(0, h - 1);
        texture.texels[(ty * w + tx) as usize].to_normalized_gamma_f32()
    };
    let (a, b, c, d) = (texel(0, 0), texel(1, 0), texel(0, 1), texel(1, 1));
    std::array::from_fn(|i| {
        let upper = a[i] + (b[i] - a[i]) * fx;
        let lower = c[i] + (d[i] - c[i]) * fx;
        upper + (lower - upper) * fy
    })
}

/// `source` (red, green, blue, alpha, premultiplied, 0-1) over a pixel.
fn over(pixel: u32, source: [f32; 4]) -> u32 {
    let keep = 1.0 - source[3];
    let channel = |value: f32, shift: u32| {
        let under = ((pixel >> shift) & 0xff) as f32;
        (((value * 255.0 + under * keep) + 0.5) as u32).min(255) << shift
    };
    channel(source[3], 24) | channel(source[0], 16) | channel(source[1], 8) | channel(source[2], 0)
}

#[cfg(test)]
impl Stage {
    /// Write the picture to `path` as a PNG (to look at).
    pub fn save_png(&mut self, path: &std::path::Path) {
        self.surface.as_mut().expect("drawn").save_png(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use egui::{Button, Rect, StrokeKind, pos2, vec2};

    fn stage() -> Stage {
        Stage::new(&Theme::dark(), 1.0)
    }

    fn premultiplied(p: u32) -> bool {
        let a = p >> 24;
        (0..3).all(|c| (p >> (c * 8)) & 0xff <= a)
    }

    #[test]
    fn nothing_drawn_is_see_through_and_shapes_have_soft_corners() {
        let mut s = stage();
        s.run((40, 40), |ui| {
            let r = Rect::from_min_size(pos2(4.0, 4.0), vec2(32.0, 32.0));
            ui.painter()
                .rect_filled(r, 8.0, Color32::from_rgb(200, 100, 50));
        });
        let surface = s.surface.as_mut().unwrap();
        let px = surface.pixels();
        let at = |x: usize, y: usize| px[y * 40 + x];
        assert_eq!(at(0, 0), 0, "outside the shape");
        assert_eq!(at(4, 4), 0, "the rounded corner");
        assert_eq!(at(20, 20), 0xffc8_6432, "the shape");
        assert!(
            px.iter().any(|&p| (1..255).contains(&(p >> 24))),
            "soft edge"
        );
        assert!(px.iter().all(|&p| premultiplied(p)));
    }

    #[test]
    fn a_translucent_shape_over_another_blends() {
        let mut s = stage();
        s.run((20, 20), |ui| {
            let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(20.0, 20.0));
            ui.painter()
                .rect_filled(r, 0.0, Color32::from_rgb(10, 20, 30));
            ui.painter()
                .rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 128));
        });
        let px = s.surface.as_mut().unwrap().pixels()[10 * 20 + 10];
        assert_eq!(px >> 24, 0xff);
        assert!(((px >> 16) & 0xff).abs_diff(133) <= 1, "{px:08x}");
        assert!(premultiplied(px));
    }

    #[test]
    fn text_lands_in_the_picture() {
        let mut s = stage();
        s.run((120, 30), |ui| {
            ui.label("Aardvark Oil");
        });
        let lit = s.surface.as_mut().unwrap().pixels().iter();
        assert!(lit.filter(|p| **p >> 24 > 0x80).count() > 40);
    }

    #[test]
    fn a_hairline_stays_on_its_pixels_at_any_scale() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let mut s = Stage::new(&Theme::dark(), scale);
            let size = (60.0 * scale) as i32;
            s.run((size, size), |ui| {
                let r = Rect::from_min_size(Pos2::ZERO, vec2(60.0, 60.0));
                let stroke = egui::Stroke::new(1.0, Color32::WHITE);
                ui.painter().rect_stroke(r, 0.0, stroke, StrokeKind::Inside);
            });
            let px = s.surface.as_mut().unwrap().pixels();
            let row = (size / 2 * size) as usize;
            assert_eq!(px[row + size as usize / 2] >> 24, 0, "{scale}: the middle");
            assert!(px[row] >> 24 > 0, "{scale}: the line");
        }
    }

    #[test]
    fn an_unchanged_frame_isnt_drawn_again() {
        let mut s = stage();
        let frame = |s: &mut Stage, text: &str| {
            let text = text.to_owned();
            s.run((120, 30), move |ui| {
                ui.label(&text);
            })
            .changed
        };
        assert!(frame(&mut s, "one"));
        assert!(!frame(&mut s, "one"));
        s.pointer_moved((60, 10));
        assert!(!frame(&mut s, "one"), "the pointer moved over nothing");
        assert!(frame(&mut s, "two"));
    }

    #[test]
    fn a_press_and_release_is_a_click_and_a_called_off_press_isnt() {
        let mut s = stage();
        let frame = |s: &mut Stage| {
            let mut clicked = false;
            s.run((120, 40), |ui| {
                clicked |= ui.add(Button::new("Keep it")).clicked();
            });
            clicked
        };
        let at = (20, 12);
        frame(&mut s);
        s.pointer_moved(at);
        frame(&mut s);
        s.button(at, true);
        assert!(!frame(&mut s));
        s.button(at, false);
        assert!(frame(&mut s));
        s.button(at, true);
        frame(&mut s);
        s.call_off_press();
        assert!(!frame(&mut s));
        assert!(!frame(&mut s));
    }

    #[test]
    fn hover_fades_ask_for_frames_and_then_stop_asking() {
        let mut s = stage();
        let frame = |s: &mut Stage| {
            s.run((120, 40), |ui| {
                let id = egui::Id::new("fade");
                let hover = ui.rect_contains_pointer(ui.max_rect());
                ui.ctx().animate_bool_with_time(id, hover, 0.05);
            })
            .again
        };
        frame(&mut s);
        s.pointer_moved((5, 5));
        frame(&mut s);
        assert!(frame(&mut s).is_some(), "fading in");
        std::thread::sleep(Duration::from_millis(80));
        frame(&mut s);
        frame(&mut s);
        assert!(frame(&mut s).is_none(), "idle");
    }
}
