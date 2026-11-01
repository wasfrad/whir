//! One frame of a loaded table: grid, find bar, footer and banners.

use std::sync::atomic::Ordering;
use std::time::Duration;

use egui::{FontId, Key, Rect, RichText, Sense, Stroke, UiBuilder, pos2, vec2};
use whir_core::find_bar::{FindBar, fmt_int};
use whir_core::icons::{self, Icon};
use whir_core::kit;
use whir_core::theme::Theme;
use whir_core::{DocCx, DocRequest, FindQuery, Waker};

use crate::editor::{Editor, Move};
use crate::grid::{self, GridIo};
use crate::loaded::{Loaded, SaveFailure};
use crate::view::SheetView;

/// What the frame asks the document to do.
pub enum Request {
    Reload,
    /// Save over a file another program changed.
    Overwrite,
    DismissFailure,
}

pub fn show_loaded(
    ui: &mut egui::Ui,
    l: &mut Loaded,
    area: Rect,
    theme: &Theme,
    waker: &Waker,
    failure: Option<&SaveFailure>,
    cx: &mut DocCx<'_>,
) -> Option<Request> {
    let now = ui.input(|i| i.time);
    l.now = now;
    let progress = l.table.progress();
    if progress.done && l.edits.rows().is_none() {
        l.edits.set_base_rows(progress.rows.saturating_sub(1));
    }
    if progress.done
        && let Some(spec) = l.pending_sort.take()
    {
        l.sort(spec.column, Some(spec.direction), waker, now);
    }
    let base_rows = l.base_rows();

    // Column widths from a sample, once the first rows are in (again at 1000 rows).
    let target = base_rows.min(1000);
    if progress.rows > 0
        && (l.measured_rows == 0
            || (l.measured_rows < 1000
                && target > l.measured_rows
                && (target == 1000 || progress.done)))
    {
        let mut view = SheetView {
            table: &mut l.table,
            edits: &l.edits,
            order: l.order.as_ref(),
            base_rows,
        };
        grid::measure_columns(ui, &mut l.grid, &mut view, target, theme);
        l.measured_rows = target.max(1);
    }

    // Only a save in progress, with its Cancel, needs a strip of its own.
    let footer_h = if l.saving.is_some() || l.pending_save.is_some() {
        24.0
    } else {
        0.0
    };
    let grid_rect = Rect::from_min_max(area.min, pos2(area.right(), area.bottom() - footer_h));
    let footer = Rect::from_min_max(pos2(area.left(), grid_rect.bottom()), area.max);

    // Shortcuts owned by the document (they end an edit in progress first).
    let keys = FindBar::consume_keys(ui);
    if keys.open || keys.next || keys.prev {
        l.commit_editor(Some(ui.ctx()), Move::Leave);
    }
    if keys.open {
        l.find.bar.open_with(None);
    }
    if keys.next {
        l.step_match(true);
    }
    if keys.prev {
        l.step_match(false);
    }

    // The cell editor's keys come before everything else that reads keys.
    l.editor_keys(ui);

    let find_has_focus = l.find.bar.has_focus(ui.ctx());
    let escape = !find_has_focus
        && l.editor.is_none()
        && !egui::Popup::is_any_open(ui.ctx())
        && ui.input(|i| i.key_pressed(Key::Escape));
    if escape {
        if l.find.bar.open {
            l.find.bar.close();
        } else {
            l.grid.sel = None;
        }
    }

    let sort_spec = l.sort_spec();
    let sort_dim = l.sorting.is_some() || l.order.as_ref().is_some_and(|o| o.stale);
    let sorted = l.order.is_some();
    let editable = l.editable();
    let loaded = &mut *l;
    let mut view = SheetView {
        table: &mut loaded.table,
        edits: &loaded.edits,
        order: loaded.order.as_ref(),
        base_rows,
    };
    let rows = view.rows();
    let out = loaded.grid.show(
        ui,
        grid_rect,
        GridIo {
            view: &mut view,
            sort: sort_spec,
            sort_dim,
            sorted,
            matches: &loaded.find.display,
            current_match: loaded.find.current,
            keyboard: !find_has_focus && no_editor(&loaded.editor),
            editable,
        },
        theme,
    );
    for action in out.actions {
        match action {
            grid::Action::FindInTabs(text) => cx.requests.push(DocRequest::FindInTabs(FindQuery {
                text: text.trim().to_owned(),
                whole_cell: true,
            })),
            action => l.apply(action, ui, waker),
        }
    }
    for action in std::mem::take(&mut l.queued) {
        l.apply(action, ui, waker);
    }
    // The editor on top of the grid (a press elsewhere commits it).
    l.show_editor(ui, theme);

    if l.find.bar.open {
        let status = l.find.status();
        let out = l.find.bar.show(ui, grid_rect, grid::HEADER_H, &status);
        if let Some(q) = out.search {
            l.find.reveal_first = true;
            l.start_find(q, waker);
        }
        if out.next {
            l.step_match(true);
        }
        if out.prev {
            l.step_match(false);
        }
    }
    if l.sorting.is_some() || l.find.running || !progress.done || l.is_saving() {
        ui.ctx().request_repaint_after(Duration::from_millis(150));
    }

    // Footer.
    let p = ui.painter().clone();
    if footer_h > 0.0 {
        p.rect_filled(footer, 0.0, theme.strip_bg);
        p.line_segment(
            [footer.left_top(), footer.right_top()],
            Stroke::new(1.0, theme.separator),
        );
    }
    let small = FontId::proportional(theme.metrics.small_text_size);
    let cols = l.grid.cols();
    let mut left = format!("{} rows × {} columns", fmt_int(rows), cols);
    if !progress.done && progress.total > 0 {
        let pct = progress.scanned as f64 / progress.total as f64 * 100.0;
        left.push_str(&format!("   ·   indexing {pct:.0}%"));
    }
    if let Some(s) = &l.sorting {
        let column = s.spec.column;
        let name = l.view().header().cell(column).text.to_owned();
        left.push_str(&format!(
            "   ·   sorting by {}…",
            if name.is_empty() { "column" } else { &name }
        ));
    }
    if let Some(sel) = l.grid.sel
        && sel.cell_count() > 1
    {
        left.push_str(&format!("   ·   {} selected", fmt_int(sel.cell_count())));
    }
    if l.read_only_reason().is_some() {
        left.push_str("   ·   read-only (too many rows to edit)");
    }
    let line = left.clone();
    let left_galley = p.layout_no_wrap(left, small.clone(), theme.text_dim);
    let left_end = footer.left() + 10.0 + left_galley.size().x;
    if footer_h > 0.0 {
        p.galley(
            pos2(
                footer.left() + 10.0,
                footer.center().y - left_galley.size().y / 2.0,
            ),
            left_galley,
            theme.text_dim,
        );
    }

    let mut right = format!(
        "{}  ·  {}",
        l.table.dialect.delimiter_label(),
        l.original_encoding.label()
    );
    if progress.quoting_disabled {
        right = format!("Unbalanced quotes: showing raw lines  ·  {right}");
    }
    if let Some(e) = &progress.error {
        right = format!("Stopped reading: {e}  ·  {right}");
    }
    if let Some((msg, until)) = &l.status {
        if now < *until {
            right = format!("{msg}   ·   {right}");
            if until.is_finite() {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64((until - now).min(3.0)));
            }
        } else {
            l.status = None;
        }
    }
    let mut right_x = footer.right() - 10.0;
    if let Some(s) = &l.saving {
        // "Saving 42%  [Cancel]"
        let btn = Rect::from_min_size(
            pos2(right_x - 54.0, footer.top() + 3.0),
            vec2(54.0, footer.height() - 6.0),
        );
        let resp = ui.interact(btn, ui.id().with("csv-save-cancel"), Sense::CLICK);
        if resp.hovered() {
            p.rect_filled(btn, 4.0, theme.tab_hover_bg);
        }
        p.text(
            btn.center(),
            egui::Align2::CENTER_CENTER,
            "Cancel",
            small.clone(),
            theme.text,
        );
        if resp.clicked() {
            s.cancel.store(true, Ordering::Relaxed);
        }
        right_x = btn.left() - 8.0;
        let pct = s.progress.load(Ordering::Relaxed) as f64 / s.total.max(1) as f64 * 100.0;
        right = format!("Saving {:.0}%   ·   {right}", pct.min(100.0));
    } else if l.pending_save.is_some() {
        // Waiting for the index: "Saving… (reading the file)  [Cancel]"
        let btn = Rect::from_min_size(
            pos2(right_x - 54.0, footer.top() + 3.0),
            vec2(54.0, footer.height() - 6.0),
        );
        let resp = ui.interact(btn, ui.id().with("csv-save-cancel"), Sense::CLICK);
        if resp.hovered() {
            p.rect_filled(btn, 4.0, theme.tab_hover_bg);
        }
        p.text(
            btn.center(),
            egui::Align2::CENTER_CENTER,
            "Cancel",
            small.clone(),
            theme.text,
        );
        if resp.clicked() {
            l.pending_save = None;
        }
        right_x = btn.left() - 8.0;
        right = format!("Saving… (reading the whole file first)   ·   {right}");
    } else if failure.is_some() {
        right = format!("Not saved   ·   {right}");
    }
    l.status_line = format!("{line}   \u{b7}   {right}");
    if footer_h > 0.0 {
        let right_galley = p.layout_no_wrap(right, small.clone(), theme.text_dim);
        let rx = (right_x - right_galley.size().x).max(left_end + 16.0);
        p.with_clip_rect(footer).galley(
            pos2(rx, footer.center().y - right_galley.size().y / 2.0),
            right_galley,
            theme.text_dim,
        );
    }

    // Banners (on top of the grid).
    let mut request = None;
    if let Some(f) = failure {
        request = failure_banner(ui, grid_rect, theme, f, cx);
    } else if !l.is_saving() && l.table.source.changed() {
        request = changed_banner(ui, grid_rect, theme, l.is_dirty(), cx);
    }
    request
}

/// The grid owns the keyboard unless a cell is being edited.
fn no_editor(editor: &Option<Editor>) -> bool {
    editor.is_none()
}

fn banner_frame(
    ui: &mut egui::Ui,
    grid_rect: Rect,
    width: f32,
    theme: &Theme,
    add: impl FnOnce(&mut egui::Ui),
) {
    let rect = Rect::from_min_size(
        pos2(grid_rect.center().x - width / 2.0, grid_rect.top() + 34.0),
        vec2(width, 40.0),
    );
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        kit::floating(theme).inner_margin(8.0).show(ui, |ui| {
            ui.horizontal_wrapped(add);
        });
    });
}

/// A save failed: say why, offer Save As (and Overwrite when another program
/// changed the file).
fn failure_banner(
    ui: &mut egui::Ui,
    grid_rect: Rect,
    theme: &Theme,
    f: &SaveFailure,
    cx: &mut DocCx<'_>,
) -> Option<Request> {
    let mut request = None;
    let width = (grid_rect.width() - 40.0).clamp(280.0, 620.0);
    banner_frame(ui, grid_rect, width, theme, |ui| {
        ui.label(RichText::new(&f.message).color(theme.danger));
        if f.changed_on_disk {
            if ui.button("Overwrite").clicked() {
                request = Some(Request::Overwrite);
            }
            if ui.button("Reload (discard edits)").clicked() {
                request = Some(Request::Reload);
            }
        }
        if ui.button("Save As…").clicked() {
            cx.requests.push(DocRequest::SaveAs);
        }
        if ui
            .button(icons::text(Icon::Close, 12.0))
            .on_hover_text("Hide")
            .clicked()
        {
            request = Some(Request::DismissFailure);
        }
    });
    request
}

/// Another program changed the file: offer a reload (confirming first when
/// there are unsaved edits).
fn changed_banner(
    ui: &mut egui::Ui,
    grid_rect: Rect,
    theme: &Theme,
    dirty: bool,
    cx: &mut DocCx<'_>,
) -> Option<Request> {
    let mut request = None;
    let width = if dirty { 520.0 } else { 340.0 };
    banner_frame(ui, grid_rect, width, theme, |ui| {
        if dirty {
            ui.label(
                RichText::new("This file changed on disk. Reloading discards your edits.")
                    .color(theme.text),
            );
            if ui.button("Discard edits and reload").clicked() {
                request = Some(Request::Reload);
            }
            if ui.button("Save As…").clicked() {
                cx.requests.push(DocRequest::SaveAs);
            }
        } else {
            ui.label(RichText::new("This file changed on disk.").color(theme.text));
            if ui.button("Reload").clicked() {
                request = Some(Request::Reload);
            }
        }
    });
    request
}
