//! Scrolling in `f64`.
//!
//! egui's scroll areas use `f32` offsets, which lose whole-pixel precision past
//! ~16M points (≈ 800k CSV rows, or a long PDF at high zoom). Viewers with
//! huge content keep their own offset in `f64` and draw with [`scrollbar`].

use egui::{Rect, Sense, Ui, pos2};

use crate::theme::Theme;

/// One scroll axis, in points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollAxis {
    pub offset: f64,
    pub content: f64,
    pub viewport: f64,
}

impl ScrollAxis {
    pub fn max_offset(&self) -> f64 {
        (self.content - self.viewport).max(0.0)
    }

    pub fn clamp(&mut self) {
        self.offset = self.offset.clamp(0.0, self.max_offset());
    }

    pub fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.offset;
        self.offset += delta;
        self.clamp();
        self.offset != before
    }

    pub fn scroll_to(&mut self, offset: f64) {
        self.offset = offset;
        self.clamp();
    }

    /// Minimal scroll so `[start, end)` is visible.
    pub fn reveal(&mut self, start: f64, end: f64) {
        if start < self.offset {
            self.offset = start;
        } else if end > self.offset + self.viewport {
            self.offset = end - self.viewport;
        }
        self.clamp();
    }

    pub fn is_scrollable(&self) -> bool {
        self.content > self.viewport + 0.5
    }
}

/// A thin overlay scrollbar. Returns true if the user moved it.
///
/// `track` is the full rect the bar may occupy (e.g. a 10pt strip at the right).
pub fn scrollbar(
    ui: &mut Ui,
    id: egui::Id,
    track: Rect,
    axis: &mut ScrollAxis,
    vertical: bool,
) -> bool {
    if !axis.is_scrollable() || track.width() <= 0.0 || track.height() <= 0.0 {
        return false;
    }
    let theme = Theme::current();
    let len = if vertical {
        track.height()
    } else {
        track.width()
    } as f64;
    let min_thumb = 28.0f64.min(len);
    let thumb_len = (axis.viewport / axis.content * len).clamp(min_thumb, len);
    let travel = (len - thumb_len).max(1.0);
    let t = axis.offset / axis.max_offset().max(1e-9);
    let thumb_start = t * travel;

    let resp = ui.interact(track, id, Sense::click_and_drag());
    let mut changed = false;

    // Dragging the thumb: remember where it was grabbed.
    let grab_id = id.with("grab");
    let pointer = resp.interact_pointer_pos();
    let along = |p: egui::Pos2| {
        (if vertical {
            p.y - track.top()
        } else {
            p.x - track.left()
        }) as f64
    };
    if resp.drag_started()
        && let Some(p) = pointer
    {
        let pos = along(p);
        let grab = if pos >= thumb_start && pos <= thumb_start + thumb_len {
            pos - thumb_start
        } else {
            thumb_len / 2.0 // clicked the track: center the thumb on the pointer
        };
        ui.data_mut(|d| d.insert_temp(grab_id, grab));
    }
    if resp.dragged()
        && let Some(p) = pointer
    {
        let grab: f64 = ui.data(|d| d.get_temp(grab_id)).unwrap_or(thumb_len / 2.0);
        let new_t = ((along(p) - grab) / travel).clamp(0.0, 1.0);
        let before = axis.offset;
        axis.scroll_to(new_t * axis.max_offset());
        changed |= axis.offset != before;
    } else if resp.clicked()
        && let Some(p) = pointer
    {
        // Page towards the click.
        let pos = along(p);
        let page = axis.viewport * 0.9;
        changed |= axis.scroll_by(if pos < thumb_start { -page } else { page });
    }

    let active = resp.hovered() || resp.dragged();
    let thickness = if active { 8.0 } else { 5.0 };
    let (thumb_start, thumb_len) = {
        let t = axis.offset / axis.max_offset().max(1e-9);
        (t * travel, thumb_len)
    };
    let thumb = if vertical {
        Rect::from_min_max(
            pos2(
                track.right() - thickness - 2.0,
                track.top() + thumb_start as f32,
            ),
            pos2(
                track.right() - 2.0,
                track.top() + (thumb_start + thumb_len) as f32,
            ),
        )
    } else {
        Rect::from_min_max(
            pos2(
                track.left() + thumb_start as f32,
                track.bottom() - thickness - 2.0,
            ),
            pos2(
                track.left() + (thumb_start + thumb_len) as f32,
                track.bottom() - 2.0,
            ),
        )
    };
    let color = if active {
        theme.text_dim
    } else {
        theme.text_dim.gamma_multiply(0.55)
    };
    ui.painter().rect_filled(thumb, thickness / 2.0, color);
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_and_reveals() {
        let mut a = ScrollAxis {
            offset: 0.0,
            content: 1000.0,
            viewport: 100.0,
        };
        assert!(a.scroll_by(5000.0));
        assert_eq!(a.offset, 900.0);
        a.reveal(10.0, 20.0);
        assert_eq!(a.offset, 10.0);
        a.reveal(500.0, 520.0);
        assert_eq!(a.offset, 420.0);
        // Precision far beyond f32's integer range.
        let mut big = ScrollAxis {
            offset: 0.0,
            content: 4.0e9,
            viewport: 800.0,
        };
        big.scroll_to(3_000_000_001.0);
        assert_eq!(big.offset, 3_000_000_001.0);
    }
}
