//! How time is kept: what the timesheet rounds to, when the dock asks about
//! time away, and whether it stays out while a timer runs.

use whir_core::DocCx;
use whir_core::kit;
use whir_core::theme::Theme;
use whir_core::timesheet::{Rounding, Store};

use super::{no_folder, toast};

const AWAY: [u32; 8] = [5, 10, 15, 20, 30, 45, 60, 0];

fn minutes(m: u32) -> String {
    match m {
        0 => "Never".to_owned(),
        m => format!("{m} minutes"),
    }
}

pub fn ui(ui: &mut egui::Ui, store: Option<&mut Store>, cx: &mut DocCx<'_>, theme: &Theme) {
    let Some(store) = store else {
        no_folder(ui, "Time tracking", theme);
        return;
    };
    kit::page_header(ui, "Time tracking", "", theme, |_| {});
    let o = store.state().options;
    let mut options = o;
    kit::group(ui, "", theme, |g| {
        if let Some(rounding) = g.choice(
            "Round each project's day to",
            "For the timesheet only: entries keep the exact time.",
            o.rounding,
            &Rounding::ALL,
            |r| r.label().to_owned(),
        ) {
            options.rounding = rounding;
        }
        if let Some(away) = g.choice(
            "Ask whether time away counts after",
            "Away means no keyboard or mouse, a locked screen, or sleep.",
            o.away_minutes,
            &AWAY,
            minutes,
        ) {
            options.away_minutes = away;
        }
        g.switch(
            "Keep the dock out while a timer runs",
            "Otherwise it slides into the edge of the screen as usual.",
            &mut options.keep_out,
        );
    });
    if options != o
        && let Err(e) = store.update(|tx| tx.state.options = options)
    {
        toast(cx, e);
    }
}
