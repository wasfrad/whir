//! The tab strip, which doubles as the window's title bar: tabs on the left, an
//! open button, empty draggable caption space, and min/max/close on the right.

use egui::epaint::CornerRadius;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, FontId, PointerButton, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use whir_core::DocKind;
use whir_core::icons::{self, Icon};
use whir_core::kit;
use whir_core::theme::Theme;

pub struct TabInfo<'a> {
    pub id: u64,
    pub title: &'a str,
    pub tooltip: Option<String>,
    /// Backed by a file (Copy path, Show in Explorer).
    pub has_path: bool,
    pub kind: DocKind,
    pub loading: bool,
    /// Unsaved changes the user must save: a dot instead of ✕ until hovered.
    pub dirty: bool,
    /// Saving failed.
    pub error: bool,
}

#[derive(Default)]
pub struct TabBarOutput {
    pub activate: Option<usize>,
    pub close: Option<usize>,
    /// Tab menu: close every tab but this one.
    pub close_others: Option<usize>,
    /// Tab menu: close the tabs after this one.
    pub close_to_right: Option<usize>,
    pub close_all: bool,
    pub copy_path: Option<usize>,
    pub reveal: Option<usize>,
    pub open_dialog: bool,
    pub new_note: bool,
    pub new_table: bool,
    pub minimize: bool,
    pub toggle_maximize: bool,
    pub close_window: bool,
    /// Rects (points) inside the strip that need client-area input.
    pub interactive: Vec<Rect>,
    /// The maximize button (points), reported to Windows as HTMAXBUTTON.
    pub maximize: Option<Rect>,
}

pub fn show(
    ui: &mut Ui,
    tabs: &[TabInfo<'_>],
    active: usize,
    maximized: bool,
    theme: &Theme,
) -> TabBarOutput {
    let m = theme.metrics;
    let strip = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(strip, 0.0, theme.strip_bg);
    let mut out = TabBarOutput::default();

    // --- caption buttons (right) ---
    let bw = m.caption_button_width;
    let close_rect =
        Rect::from_min_max(pos2(strip.right() - bw, strip.top()), strip.right_bottom());
    let max_rect = close_rect.translate(vec2(-bw, 0.0));
    let min_rect = max_rect.translate(vec2(-bw, 0.0));
    if caption_button(ui, min_rect, "min", theme, false, Icon::Minimize).clicked() {
        out.minimize = true;
    }
    let icon = if maximized {
        Icon::Restore
    } else {
        Icon::Maximize
    };
    if caption_button(ui, max_rect, "max", theme, false, icon).clicked() {
        out.toggle_maximize = true;
    }
    if caption_button(ui, close_rect, "close", theme, true, Icon::WindowClose).clicked() {
        out.close_window = true;
    }
    out.interactive.push(min_rect);
    out.interactive.push(close_rect);
    out.maximize = Some(max_rect);

    // --- tabs (left) ---
    let top_gap = if maximized { 4.0 } else { 7.0 };
    let plus_size = 28.0;
    let left = strip.left() + 8.0;
    let tabs_right = min_rect.left() - m.drag_area_min_width - plus_size - 6.0;
    let avail = (tabs_right - left).max(0.0);
    // Tabs stay wide enough to read. Those that don't fit are in a menu at the
    // end of the row, and the active one is always among those showing.
    let room = |width: f32| (((width + m.tab_gap) / (m.tab_min_width + m.tab_gap)) as usize).max(1);
    let overflowing = tabs.len() > room(avail);
    let row = if overflowing {
        avail - plus_size - m.tab_gap
    } else {
        avail
    };
    let showing = room(row).min(tabs.len());
    let first = active
        .saturating_sub(showing.saturating_sub(1))
        .min(tabs.len() - showing);
    let n = showing.max(1) as f32;
    let tab_w = (row / n - m.tab_gap).min(m.tab_max_width);
    let tab_h = strip.height() - top_gap;
    let text_color = |active: bool| {
        if active {
            theme.tab_text
        } else {
            theme.tab_text_inactive
        }
    };

    let mut x = left;
    let mut hovered_index = None;
    let mut rects = Vec::with_capacity(tabs.len());
    for (i, tab) in tabs.iter().enumerate().skip(first).take(showing) {
        let rect = Rect::from_min_size(pos2(x, strip.top() + top_gap), vec2(tab_w, tab_h));
        x += tab_w + m.tab_gap;
        rects.push(rect);
        let id = ui.id().with(("tab", tab.id));
        let resp = ui.interact(rect, id, Sense::CLICK);
        if resp.hovered() {
            hovered_index = Some(i);
        }
        let is_active = i == active;

        // Close button: registered after the tab so it wins hit-testing.
        let show_close = rect.width() >= 60.0 || is_active;
        let close_rect = Rect::from_center_size(
            pos2(rect.right() - 16.0, rect.center().y - 1.0),
            vec2(18.0, 18.0),
        );
        let close_resp = show_close.then(|| ui.interact(close_rect, id.with("x"), Sense::CLICK));
        let close_hovered = close_resp.as_ref().is_some_and(|r| r.hovered());

        if resp.hovered()
            && !close_hovered
            && ui.input(|inp| inp.pointer.button_pressed(PointerButton::Primary))
        {
            out.activate = Some(i);
        }
        if resp.clicked_by(PointerButton::Middle)
            || close_resp.as_ref().is_some_and(|r| r.clicked())
        {
            out.close = Some(i);
        }
        if let Some(tip) = &tab.tooltip {
            resp.clone().on_hover_text_at_pointer(tip);
        }
        tab_menu(&resp, i, tab, tabs.len(), &mut out);

        // Paint.
        let r = m.corner_radius as u8;
        if is_active {
            let body = Rect::from_min_max(rect.min, pos2(rect.right(), strip.bottom()));
            painter.rect_filled(
                body,
                CornerRadius {
                    nw: r,
                    ne: r,
                    sw: 0,
                    se: 0,
                },
                theme.content_bg,
            );
        } else if resp.hovered() {
            painter.rect_filled(
                rect.shrink2(vec2(0.0, 2.0)).translate(vec2(0.0, -1.0)),
                r,
                theme.tab_hover_bg,
            );
        }
        let icon_center = pos2(rect.left() + 17.0, rect.center().y - 1.0);
        if rect.width() >= 44.0 || !show_close {
            if tab.loading {
                paint_spinner(
                    &painter,
                    icon_center,
                    6.0,
                    theme.accent,
                    ui.input(|i| i.time),
                );
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(33));
            } else {
                paint_kind_glyph(&painter, icon_center, tab.kind, theme);
            }
        }
        let text_left = rect.left() + 31.0;
        let text_right = if show_close {
            close_rect.left() - 4.0
        } else {
            rect.right() - 8.0
        };
        if text_right - text_left > 8.0 {
            whir_core::theme::note_text(tab.title);
            let mut job = LayoutJob::single_section(
                tab.title.to_owned(),
                TextFormat::simple(
                    FontId::proportional(m.body_text_size),
                    text_color(is_active),
                ),
            );
            job.wrap = TextWrapping {
                max_width: text_right - text_left,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = painter.layout_job(job);
            let pos = pos2(text_left, icon_center.y - galley.size().y / 2.0);
            painter
                .with_clip_rect(rect)
                .galley(pos, galley, theme.tab_text);
        }
        let flagged = tab.dirty || tab.error;
        if let Some(cr) = &close_resp
            && (is_active || resp.hovered() || cr.hovered() || flagged)
        {
            if cr.hovered() {
                let bg = if cr.is_pointer_button_down_on() {
                    theme.caption_pressed
                } else {
                    theme.caption_hover
                };
                painter.rect_filled(close_rect, 4.0, bg);
            }
            if flagged && !cr.hovered() {
                // Unsaved (or failed to save): a dot where the ✕ goes.
                let color = if tab.error {
                    theme.danger
                } else {
                    text_color(is_active)
                };
                painter.circle_filled(close_rect.center(), 3.5, color);
            } else {
                icons::paint(
                    &painter,
                    close_rect.center(),
                    Icon::Close,
                    12.0,
                    text_color(is_active),
                );
            }
        }
    }

    // Subtle separators between inactive, un-hovered neighbours.
    for (i, rect) in rects.iter().enumerate().skip(1) {
        let quiet = |j: usize| first + j != active && Some(first + j) != hovered_index;
        if quiet(i - 1) && quiet(i) {
            let x = painter.round_to_pixel_center(rect.left() - m.tab_gap / 2.0);
            let y = rect.center().y;
            painter.line_segment(
                [pos2(x, y - 8.0), pos2(x, y + 8.0)],
                Stroke::new(1.0, theme.separator),
            );
        }
    }
    out.interactive.extend(rects.iter().copied());

    // --- new note button (right-click: more) ---
    let mut plus_left = rects.last().map_or(left, |r| r.right() + m.tab_gap + 2.0);
    if overflowing {
        let y = strip.top() + top_gap + (tab_h - plus_size) / 2.0 - 1.0;
        let rect = Rect::from_min_size(pos2(plus_left, y), vec2(plus_size, plus_size));
        overflow_menu(ui, rect, tabs, active, showing, theme, &mut out);
        out.interactive.push(rect);
        plus_left = rect.right() + 2.0;
    }
    let plus = Rect::from_min_size(
        pos2(
            plus_left,
            strip.top() + top_gap + (tab_h - plus_size) / 2.0 - 1.0,
        ),
        vec2(plus_size, plus_size),
    );
    let plus_resp = ui.interact(plus, ui.id().with("new"), Sense::CLICK);
    if plus_resp.hovered() {
        painter.rect_filled(plus, 6.0, theme.tab_hover_bg);
    }
    icons::paint(
        &painter,
        plus.center(),
        Icon::Plus,
        m.icon_size - 2.0,
        theme.tab_text_inactive,
    );
    if plus_resp.clicked() {
        out.new_note = true;
    }
    plus_resp.context_menu(|ui| {
        ui.set_min_width(190.0);
        if ui
            .add(kit::menu_entry(Some(Icon::Note), "New note", "Ctrl+N"))
            .clicked()
        {
            out.new_note = true;
            ui.close();
        }
        if ui
            .add(kit::menu_entry(
                Some(Icon::Table),
                "New table",
                "Ctrl+Shift+N",
            ))
            .clicked()
        {
            out.new_table = true;
            ui.close();
        }
        ui.separator();
        if ui
            .add(kit::menu_entry(Some(Icon::Folder), "Open file…", "Ctrl+O"))
            .clicked()
        {
            out.open_dialog = true;
            ui.close();
        }
    });
    plus_resp.on_hover_text("New note (Ctrl+N) · right-click for more");
    out.interactive.push(plus);
    out
}

/// Every tab, from the end of a row too full to show them all.
fn overflow_menu(
    ui: &mut Ui,
    rect: Rect,
    tabs: &[TabInfo<'_>],
    active: usize,
    showing: usize,
    theme: &Theme,
    out: &mut TabBarOutput,
) {
    let resp = ui.interact(rect, ui.id().with("all_tabs"), Sense::CLICK);
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, theme.tab_hover_bg);
    }
    icons::paint(
        ui.painter(),
        rect.center(),
        Icon::ChevronDown,
        10.0,
        theme.tab_text_inactive,
    );
    let hidden = tabs.len() - showing;
    let resp = resp.on_hover_text(format!("{hidden} more tabs"));
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(240.0);
        ui.set_max_width(360.0);
        for (i, tab) in tabs.iter().enumerate() {
            let entry = kit::menu_entry(Some(kind_icon(tab.kind, theme).0), tab.title, "");
            if ui.add(entry.selected(i == active)).clicked() {
                out.activate = Some(i);
                ui.close();
            }
        }
    });
}

/// Right-click on a tab. Hidden (overflowed) tabs count as "others" and
/// "to the right" too.
fn tab_menu(resp: &egui::Response, i: usize, tab: &TabInfo<'_>, n: usize, out: &mut TabBarOutput) {
    resp.context_menu(|ui| {
        ui.set_min_width(190.0);
        if ui
            .add(kit::menu_entry(Some(Icon::Close), "Close", "Ctrl+W"))
            .clicked()
        {
            out.close = Some(i);
            ui.close();
        }
        if ui
            .add_enabled(n > 1, kit::menu_entry(None, "Close others", ""))
            .clicked()
        {
            out.close_others = Some(i);
            ui.close();
        }
        if ui
            .add_enabled(
                i + 1 < n,
                kit::menu_entry(None, "Close tabs to the right", ""),
            )
            .clicked()
        {
            out.close_to_right = Some(i);
            ui.close();
        }
        if ui.add(kit::menu_entry(None, "Close all", "")).clicked() {
            out.close_all = true;
            ui.close();
        }
        if tab.has_path {
            ui.separator();
            if ui
                .add(kit::menu_entry(Some(Icon::Copy), "Copy path", ""))
                .clicked()
            {
                out.copy_path = Some(i);
                ui.close();
            }
            if ui
                .add(kit::menu_entry(Some(Icon::Folder), "Show in Explorer", ""))
                .clicked()
            {
                out.reveal = Some(i);
                ui.close();
            }
        }
    });
}

fn caption_button(
    ui: &mut Ui,
    rect: Rect,
    id: &str,
    theme: &Theme,
    is_close: bool,
    icon: Icon,
) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with(("caption", id)), Sense::CLICK);
    let (bg, fg) = if resp.is_pointer_button_down_on() {
        (
            Some(if is_close {
                theme.close_pressed
            } else {
                theme.caption_pressed
            }),
            if is_close {
                Color32::WHITE
            } else {
                theme.tab_text
            },
        )
    } else if resp.hovered() {
        (
            Some(if is_close {
                theme.close_hover
            } else {
                theme.caption_hover
            }),
            if is_close {
                Color32::WHITE
            } else {
                theme.tab_text
            },
        )
    } else {
        (None, theme.tab_text)
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, 0.0, bg);
    }
    icons::paint(ui.painter(), rect.center(), icon, 10.0, fg);
    resp
}

fn kind_icon(kind: DocKind, theme: &Theme) -> (Icon, Color32) {
    match kind {
        DocKind::Image => (Icon::Picture, theme.image_glyph),
        DocKind::Pdf => (Icon::Pdf, theme.pdf_glyph),
        DocKind::Csv => (Icon::Table, theme.csv_glyph),
        DocKind::Note => (Icon::Note, theme.note_glyph),
        DocKind::Settings => (Icon::Settings, theme.text_dim),
        DocKind::Error => (Icon::Warning, theme.danger),
    }
}

pub(crate) fn paint_kind_glyph(painter: &egui::Painter, c: Pos2, kind: DocKind, theme: &Theme) {
    let (icon, color) = kind_icon(kind, theme);
    icons::paint(painter, c, icon, theme.metrics.icon_size, color);
}

fn paint_spinner(painter: &egui::Painter, c: Pos2, radius: f32, color: Color32, time: f64) {
    let start = (time * 5.0) as f32;
    let n = 20;
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = start + i as f32 / n as f32 * std::f32::consts::TAU * 0.7;
            c + radius * vec2(a.cos(), a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.6, color)));
}
