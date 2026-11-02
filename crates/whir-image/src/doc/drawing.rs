//! The pen and arrow style panels, and their in-progress marks.

use super::*;

pub(super) fn preview(drag: &Drag, at: &Placement, min: f32) -> Option<Mark> {
    match drag {
        Drag::Draw { from, to } => drawn(*from, *to, at.size, min, edit::current_ink()),
        Drag::Freehand { points, pen, width } => tools::freehand(points, at.size, *pen, *width),
        Drag::Arrow {
            from,
            to,
            pen,
            width,
        } => tools::arrow(*from, *to, at.size, min, *pen, *width),
        _ => None,
    }
}

/// Drop subpixel jitter while moving; a zero threshold preserves the release
/// position even when it is closer than the normal sampling distance.
pub(super) fn sample(points: &mut Vec<Pos2>, point: Pos2, min: f32) {
    if points
        .last()
        .is_none_or(|last| *last != point && last.distance(point) >= min)
    {
        points.push(point);
    }
}

impl ImageDoc {
    pub(super) fn drawing_tool(&mut self, ui: &mut egui::Ui, tool: Tool, theme: &Theme) {
        let arrow = tool == Tool::Arrow;
        let (icon, tip) = if arrow {
            (
                Icon::Arrow,
                "Arrow (A) · drag from tail to tip · hold or right-click for colour and width",
            )
        } else {
            (
                Icon::Pen,
                "Pen (P) · draw freely · hold or right-click for colour and width",
            )
        };
        let selected = self.editor().and_then(|e| e.marks().get(e.selected?));
        let selected_style = selected.and_then(|m| match (&m.kind, arrow) {
            (Kind::Arrow(d), true) | (Kind::Freehand(d), false) => Some(d),
            _ => None,
        });
        let pen = selected_style.map_or_else(|| edit::current_drawing_pen(arrow), |d| d.pen);
        let width = selected_style.map_or_else(|| edit::current_width(arrow), |d| d.width);
        let pressed = toolbar::tool_button(
            ui,
            icon,
            self.tool == Some(tool),
            Some(pen.color()),
            true,
            theme,
        );
        if pressed.clicked {
            self.toggle_tool(tool);
        }
        let id = egui::Id::new("whir.image.drawing").with(arrow);
        if pressed.menu {
            toolbar::open_flyout(ui.ctx(), id);
        }
        let button = pressed.response.rect;
        if !egui::Popup::is_id_open(ui.ctx(), id) {
            pressed.response.on_hover_text(tip);
        }
        let was_open = egui::Popup::is_id_open(ui.ctx(), id);
        toolbar::flyout(ui, id, button, |ui| {
            let (mut colour, mut thickness) = (None, None);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(130.0);
                    ui.label("Colour");
                    let colours = Pen::ALL.map(Pen::color);
                    let current = Pen::ALL.iter().position(|&p| p == pen).unwrap_or(0);
                    colour = toolbar::swatches(ui, &colours, current, theme).map(|i| Pen::ALL[i]);
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(130.0);
                    ui.label("Width");
                    thickness = widths(ui, width, pen, theme);
                });
            });
            if let Some(pen) = colour {
                edit::set_drawing_pen(arrow, pen);
            }
            if let Some(width) = thickness {
                edit::set_width(arrow, width);
            }
            if colour.is_some() || thickness.is_some() {
                self.arm(tool);
                self.recolour(|kind| {
                    let d = match (kind, arrow) {
                        (Kind::Arrow(d), true) | (Kind::Freehand(d), false) => d,
                        _ => return,
                    };
                    if let Some(pen) = colour {
                        d.pen = pen;
                    }
                    if let Some(width) = thickness {
                        d.width = width;
                    }
                });
            }
            // Both choices belong to one panel: keep it open to pick the other.
            // The usual outside press or Esc dismisses it.
            false
        });
        if was_open
            && !egui::Popup::is_id_open(ui.ctx(), id)
            && ui.input(|i| i.pointer.primary_pressed())
        {
            // Dismissing the style panel on the image must not also leave a dot.
            self.swallow = true;
        }
    }
}

fn widths(ui: &mut egui::Ui, current: u32, pen: Pen, theme: &Theme) -> Option<u32> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for width in edit::WIDTHS {
            let (rect, response) = ui.allocate_exact_size(vec2(30.0, 24.0), Sense::CLICK);
            if let Some(fill) = toolbar::fill_of(ui, &response, width == current, theme) {
                ui.painter()
                    .rect_filled(rect, theme.metrics.control_radius, fill);
            }
            let a = rect.center() - vec2(7.0, 0.0);
            let b = rect.center() + vec2(7.0, 0.0);
            // Use the interface colour so even white ink stays visible here.
            let color = if pen == Pen::White {
                theme.text
            } else {
                pen.color()
            };
            ui.painter()
                .line_segment([a, b], Stroke::new(width as f32, color));
            for p in [a, b] {
                ui.painter().circle_filled(p, width as f32 * 0.5, color);
            }
            if response.clicked() {
                picked = Some(width);
            }
            response.on_hover_text(format!("{width} px"));
        }
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_ink_is_clipped_to_the_image_not_just_the_viewport() {
        let at = Placement {
            rect: Rect::from_min_size(pos2(50.0, 50.0), vec2(100.0, 100.0)),
            size: vec2(100.0, 100.0),
            turns: 0,
        };
        for arrow in [false, true] {
            let mark =
                Mark::drawing(&[pos2(0.0, 10.0), pos2(0.0, 90.0)], Pen::Red, 12, arrow).unwrap();
            let ctx = egui::Context::default();
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint_mark(&ui.ctx().debug_painter(), at.mark_rect(&mark), &mark, &at);
            });
            assert!(!output.shapes.is_empty());
            assert!(output.shapes.iter().all(|s| s.clip_rect == at.rect));
            output.drop_without_applying_deltas();
        }
    }

    #[test]
    fn sampling_skips_jitter_but_keeps_the_release_and_returning_points() {
        let mut points = vec![pos2(10.0, 20.0)];
        sample(&mut points, pos2(10.2, 20.0), 0.5);
        assert_eq!(points.len(), 1);
        sample(&mut points, pos2(11.0, 20.0), 0.5);
        sample(&mut points, pos2(10.0, 20.0), 0.5);
        sample(&mut points, pos2(10.2, 20.0), 0.0);
        sample(&mut points, pos2(10.2, 20.0), 0.0);
        assert_eq!(points.len(), 4);
        assert_eq!(points.last(), Some(&pos2(10.2, 20.0)));
    }
}
