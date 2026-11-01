//! Finding things by a few letters of their name: the command palette, and the
//! timesheet's list of projects.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, FontId};

/// How `query` fits a piece of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// Higher is better.
    pub score: i32,
    /// The characters it landed on, in order.
    pub at: Vec<usize>,
}

const MATCHED: i32 = 1;
const AT_START: i32 = 12;
const AT_WORD: i32 = 8;
const NEXT_TO_LAST: i32 = 6;
const MOST_GAP: i32 = 3;
/// What each letter of a query has to earn on average for [`rank`] to count it
/// as found: letters merely in order, scattered through a name, aren't a match.
const WORTH_SHOWING: i32 = 3;

/// The best way the letters of `query` fall, in order, in `text`, ignoring
/// case: `None` if they don't all. An empty query fits anything, equally.
pub fn find(query: &str, text: &str) -> Option<Match> {
    let lower = |s: &str| -> Vec<char> { s.chars().flat_map(char::to_lowercase).collect() };
    let query: Vec<char> = lower(query).into_iter().filter(|c| *c != ' ').collect();
    if query.is_empty() {
        return Some(Match {
            score: 0,
            at: Vec::new(),
        });
    }
    let chars = lower(text);
    if chars.len() < query.len() {
        return None;
    }
    let bonus = |i: usize| {
        if i == 0 {
            AT_START
        } else if !chars[i - 1].is_alphanumeric() {
            AT_WORD
        } else {
            0
        }
    };
    // best[j]: the best score with the query so far ending on character `j`;
    // from[q][j]: the character before it.
    const NONE: i32 = i32::MIN / 2;
    let mut best = vec![NONE; chars.len()];
    let mut from = vec![vec![usize::MAX; chars.len()]; query.len()];
    for (q, &c) in query.iter().enumerate() {
        let mut next = vec![NONE; chars.len()];
        for j in q..chars.len() {
            if chars[j] != c {
                continue;
            }
            if q == 0 {
                next[j] = MATCHED + bonus(j) - (j as i32).min(MOST_GAP);
                continue;
            }
            for (k, &before) in best.iter().enumerate().take(j).skip(q - 1) {
                if before == NONE {
                    continue;
                }
                let gap = (j - k - 1) as i32;
                let joined = if gap == 0 { NEXT_TO_LAST } else { 0 };
                let score = before + MATCHED + bonus(j) + joined - gap.min(MOST_GAP);
                if score > next[j] {
                    next[j] = score;
                    from[q][j] = k;
                }
            }
        }
        best = next;
    }
    let (mut j, score) = best
        .iter()
        .copied()
        .enumerate()
        .filter(|&(_, s)| s != NONE)
        .max_by_key(|&(j, s)| (s, usize::MAX - j))?;
    let mut at = vec![j; query.len()];
    for q in (1..query.len()).rev() {
        j = from[q][j];
        at[q - 1] = j;
    }
    Some(Match { score, at })
}

/// `text` on one line, its matched letters picked out.
pub fn job(
    text: &str,
    at: &[usize],
    font: &FontId,
    colors: (Color32, Color32),
    width: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let (plain, found) = colors;
    let mut buffer = [0u8; 4];
    for (i, c) in text.chars().enumerate() {
        let color = if at.contains(&i) { found } else { plain };
        let format = TextFormat::simple(font.clone(), color);
        job.append(c.encode_utf8(&mut buffer), 0.0, format);
    }
    job.wrap = TextWrapping {
        max_width: width.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job
}

/// Indices of `texts` that fit `query`, best first; the shorter of two equals,
/// then the earlier. With its match for each.
pub fn rank<'a>(query: &str, texts: impl Iterator<Item = &'a str>) -> Vec<(usize, Match)> {
    let letters = query.chars().filter(|c| *c != ' ').count() as i32;
    let mut found: Vec<(usize, Match, usize)> = texts
        .enumerate()
        .filter_map(|(i, text)| Some((i, find(query, text)?, text.chars().count())))
        .filter(|(_, hit, _)| hit.score >= letters * WORTH_SHOWING)
        .collect();
    let empty = query.trim().is_empty();
    found.sort_by_key(|(i, m, len)| {
        if empty {
            (0, 0, *i)
        } else {
            (-m.score, *len, *i)
        }
    });
    found.into_iter().map(|(i, m, _)| (i, m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first<'a>(query: &str, texts: &[&'a str]) -> &'a str {
        texts[rank(query, texts.iter().copied())[0].0]
    }

    #[test]
    fn letters_have_to_be_there_in_order() {
        assert!(find("fit", "Fit page").is_some());
        assert!(find("FIT", "fit page").is_some());
        assert!(find("fp", "Fit page").is_some());
        assert_eq!(find("tif", "Fit page"), None);
        assert_eq!(find("fit pages!", "Fit page"), None);
        assert_eq!(find("x", ""), None);
    }

    #[test]
    fn the_start_of_a_name_beats_a_word_beats_the_middle() {
        assert_eq!(first("set", &["Reset zoom", "Settings"]), "Settings");
        assert_eq!(first("page", &["Webpage", "Fit page"]), "Fit page");
        assert_eq!(first("fw", &["Flowchart", "Fit width"]), "Fit width");
        assert_eq!(first("nn", &["Finance", "New note"]), "New note");
    }

    #[test]
    fn letters_together_beat_letters_apart() {
        assert_eq!(first("save", &["Slide a view", "Save as"]), "Save as");
        assert_eq!(first("zoom", &["Zero room", "Zoom in"]), "Zoom in");
    }

    #[test]
    fn of_two_equals_the_shorter_then_the_earlier_comes_first() {
        assert_eq!(first("find", &["Find in all tabs", "Find"]), "Find");
        assert_eq!(first("a", &["alpha", "apple"]), "alpha");
    }

    #[test]
    fn letters_scattered_through_a_name_arent_a_match() {
        let texts = ["New note", "Terminal outage plan", "Startup & window"];
        let found = rank("note", texts.iter().copied());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 0);
        // But initials are.
        let found = rank("sw", ["Startup & window"].iter().copied());
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn it_says_which_letters_it_landed_on() {
        assert_eq!(find("fw", "Fit width").unwrap().at, vec![0, 4]);
        assert_eq!(find("note", "New note").unwrap().at, vec![4, 5, 6, 7]);
        // Spaces in a query are nothing to match.
        assert_eq!(find("new n", "New note").unwrap().at, vec![0, 1, 2, 4]);
    }

    #[test]
    fn no_query_leaves_everything_in_order() {
        let texts = ["Zebra", "Apple", "Mango"];
        let order: Vec<usize> = rank("", texts.iter().copied())
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        assert_eq!(order, [0, 1, 2]);
    }

    #[test]
    fn text_that_changes_length_in_lower_case_is_no_trouble() {
        assert!(find("i", "İstanbul").is_some());
        let _ = job(
            "İstanbul",
            &[0],
            &FontId::proportional(13.0),
            (Color32::WHITE, Color32::RED),
            100.0,
        );
    }
}
