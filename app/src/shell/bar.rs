//! The bar at a panel's foot: its geometry, its chords, and its labels.
//!
//! A panel's [`verbs`](kernel::panel::Panel::verbs) are pulled on every
//! draw, left to right: the buttons that act on what the panel shows and
//! the links that go somewhere from it. The header wears nothing but the
//! title and the close button.
//!
//! An entry is addressed — by a chord, by a click, by an e2e script — by
//! its label. Its identity is the verb's `id`, which is what the hit
//! carries and what the panel's [`run`](kernel::panel::Panel::run) is
//! handed, so the bar is a view of the panel and never a copy of it.
//!
//! A row that fills wraps to the next one, up to [`MAX_ROWS`]: a narrow
//! panel gets a taller bar rather than fewer verbs. [`height`] is what the
//! wrap costs, and the body is drawn in what is left.

use kernel::panel::Verb;
use kernel::theme;
use makepad_widgets::*;

use super::draw::CellFont;
use super::keys::Letters;

/// The strip's height at one row, rule included.
pub const BAR_H: f64 = 26.0;
/// A bar entry's height.
pub const ENTRY_H: f64 = 18.0;
/// The inset at either end of the strip.
pub const PAD_X: f64 = 8.0;
/// Above the first row of entries, and below the last.
pub const PAD_Y: f64 = (BAR_H - ENTRY_H) / 2.0;
/// Between two entries.
pub const GAP: f64 = 6.0;
/// Between two rows of them, where a bar wrapped.
pub const ROW_GAP: f64 = 4.0;
/// A button's label sits inside this much padding.
pub const BTN_PAD: f64 = 12.0;
/// The most rows a bar wraps to. Past this a panel would be all bar and no
/// body, and what does not fit is dropped, as it was when a bar was one
/// row.
pub const MAX_ROWS: usize = 3;
/// What a bar leaves the body, however many verbs it wears — the same few
/// points below which the drawing gives up on a body altogether.
const MIN_BODY: f64 = 4.0;

/// Where one entry landed, and which verb it draws.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// The verb's index in the panel's own list.
    pub at: usize,
    pub rect: Rect,
    /// A button runs; a link goes.
    pub button: bool,
}

/// The strip at the foot of a panel's rectangle, `h` tall — which is what
/// [`height`] answered for the same verbs.
#[must_use]
pub fn strip(panel: Rect, h: f64) -> Rect {
    Rect {
        pos: dvec2(panel.pos.x + 1.0, panel.pos.y + panel.size.y - h - 1.0),
        size: dvec2((panel.size.x - 2.0).max(0.0), h),
    }
}

/// How tall a bar of these verbs stands in this panel: nothing at all
/// where there are none, and otherwise exactly the rows [`entries`] fills
/// — capped by [`MAX_ROWS`] and by the room the panel has to spare.
#[must_use]
pub fn height(verbs: &[Verb], cell: &CellFont, panel: Rect) -> f64 {
    if verbs.is_empty() {
        return 0.0;
    }
    let width = (panel.size.x - 2.0).max(0.0);
    let room = panel.size.y - theme::HEAD_H - MIN_BODY;
    let max = rows_in(room).clamp(1, MAX_ROWS);
    let rows = flow(verbs, cell, width, max)
        .last()
        .map_or(1, |p| p.row + 1);
    rows_h(rows)
}

/// Lays the verbs out left to right in the strip, wrapping to a further
/// row when one fills. Entries past the last row the strip holds are
/// dropped, and so is anything after a label too long for a row of its own.
#[must_use]
pub fn entries(verbs: &[Verb], cell: &CellFont, strip: Rect) -> Vec<Entry> {
    flow(verbs, cell, strip.size.x, rows_in(strip.size.y))
        .into_iter()
        .map(|p| Entry {
            at: p.at,
            rect: Rect {
                pos: dvec2(
                    strip.pos.x + p.x,
                    strip.pos.y + PAD_Y + p.row as f64 * (ENTRY_H + ROW_GAP),
                ),
                size: dvec2(p.w, ENTRY_H),
            },
            button: verbs[p.at].act.button(),
        })
        .collect()
}

/// One entry, placed in the strip's own coordinates.
struct Placed {
    at: usize,
    row: usize,
    x: f64,
    w: f64,
}

/// The layout both [`entries`] and [`height`] read, so a bar is as tall as
/// what it draws: the verbs that fit `rows` rows of `width`, in order.
fn flow(verbs: &[Verb], cell: &CellFont, width: f64, rows: usize) -> Vec<Placed> {
    let mut out = Vec::new();
    let right = width - PAD_X;
    let (mut x, mut row) = (PAD_X, 0);
    for (at, v) in verbs.iter().enumerate() {
        let w = cell.label_w(v.label.chars().count()) + if v.act.button() { BTN_PAD } else { 0.0 };
        if x + w > right {
            // The row is full: the next one, if this bar has one to give
            // and the label fits a row at all.
            row += 1;
            x = PAD_X;
            if row >= rows || x + w > right {
                break;
            }
        }
        out.push(Placed { at, row, x, w });
        x += w + GAP;
    }
    out
}

/// What `rows` rows of entries stand in, padding and rule included.
fn rows_h(rows: usize) -> f64 {
    PAD_Y * 2.0 + rows as f64 * ENTRY_H + (rows.saturating_sub(1)) as f64 * ROW_GAP
}

/// The other way about: how many rows a strip that tall holds.
fn rows_in(h: f64) -> usize {
    if h < BAR_H {
        return 0;
    }
    ((h - PAD_Y * 2.0 + ROW_GAP) / (ENTRY_H + ROW_GAP)) as usize
}

/// The verb a letter fires on this bar, by id. The bar's own half of the
/// chord routing order; who is asked, and in what order, is
/// [`keys`](super::keys)'.
#[must_use]
pub fn chord(verbs: &[Verb], c: char) -> Option<&'static str> {
    verbs
        .iter()
        .find(|v| v.accel.is_some_and(|a| a.eq_ignore_ascii_case(&c)))
        .map(|v| v.id)
}

/// The letters a bar wears, whether or not it draws them bold.
#[must_use]
pub fn letters(verbs: &[Verb]) -> Letters {
    verbs
        .iter()
        .filter_map(|v| v.accel)
        .fold(Letters::NONE, Letters::with)
}

/// Where a bar stands in the chord routing order — which is the whole of
/// what decides its bold letters.
#[derive(Debug, Clone, Copy)]
pub enum Reach<'a> {
    /// The focused panel's own bar: the third step of the order, reached
    /// unless the widget above it keeps the letter.
    Focused { kept: Letters },
    /// The bar of the panel the focused one previews: the fourth step, and
    /// so reached only by what the two above it leave free.
    Preview { kept: Letters, driver: &'a [Verb] },
    /// Every other bar. A chord never arrives here at all.
    Away,
}

/// The letters this bar draws bold: the promise that the chord fires that
/// verb *now*, and so exactly what [`keys`](super::keys) would route to it.
///
/// A verb whose letter is not in the set is still drawn, and still fires on
/// click — the bar is the same bar either way. What goes is the mark that
/// says a key would do it.
#[must_use]
pub fn bold(verbs: &[Verb], reach: Reach<'_>) -> Letters {
    let mine = letters(verbs);
    let free = match reach {
        Reach::Away => return Letters::NONE,
        Reach::Focused { kept } => mine.minus(kept),
        // The focused bar takes the chord whether or not it drew the letter
        // bold, so what it wears is gone from here as surely as what the
        // widget keeps.
        Reach::Preview { kept, driver } => mine.minus(kept).minus(letters(driver)),
    };
    // A reserved letter never arrives, whatever a bar wears: `check` is a
    // debug assertion, and this holds in a release build too.
    free.minus(Letters::RESERVED)
}

/// A bar wears no reserved chord and no letter twice. Checked on every
/// draw in a debug build, which is where a new verb is written.
pub fn check(verbs: &[Verb]) {
    if !cfg!(debug_assertions) {
        return;
    }
    let mut seen: Vec<char> = Vec::new();
    for v in verbs {
        let Some(c) = v.accel else { continue };
        let c = c.to_ascii_lowercase();
        assert!(
            !super::keys::is_reserved(c),
            "the verb {} wears cmd+{c}, which the workspace keeps",
            v.id
        );
        assert!(
            !seen.contains(&c),
            "two verbs on one bar wear cmd+{c}; {} is the second",
            v.id
        );
        seen.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bar as an app writes one: the label matters only for the drawing.
    fn bar(letters: &[char]) -> Vec<Verb> {
        letters
            .iter()
            .map(|c| Verb::run("v", c.to_string(), Some(*c)))
            .collect()
    }

    fn set(verbs: &[Verb], reach: Reach<'_>) -> String {
        let bold = bold(verbs, reach);
        ('a'..='z').filter(|c| bold.has(*c)).collect()
    }

    /// The same label `n` times, so every entry on the bar is one width.
    fn same(label: &str, n: usize) -> Vec<Verb> {
        (0..n).map(|_| Verb::run("v", label, None)).collect()
    }

    /// What one button of `chars` characters takes.
    fn entry_w(cell: &CellFont, chars: usize) -> f64 {
        cell.label_w(chars) + BTN_PAD
    }

    /// A panel narrow enough that two `copy` buttons fill a row, and tall
    /// enough that only [`MAX_ROWS`] bounds the bar.
    fn narrow(cell: &CellFont) -> Rect {
        let w = PAD_X * 2.0 + entry_w(cell, 4) * 2.0 + GAP + 3.0;
        rect(0.0, 0.0, w, 400.0)
    }

    /// A verb that will not fit stays on the bar: it takes the next row,
    /// which starts back at the left inset one entry's height down.
    #[test]
    fn a_full_row_wraps_to_the_next() {
        let cell = CellFont::default();
        let verbs = same("copy", 3);
        let sr = strip(narrow(&cell), rows_h(2));
        let out = entries(&verbs, &cell, sr);
        assert_eq!(out.len(), 3, "nothing is dropped: the third wrapped");
        assert_eq!(out[0].rect.pos.y, out[1].rect.pos.y, "two to a row");
        assert_eq!(out[2].rect.pos.x, sr.pos.x + PAD_X);
        assert_eq!(out[2].rect.pos.y, out[0].rect.pos.y + ENTRY_H + ROW_GAP);
    }

    /// And the panel pays for the row: a bar is as tall as what it draws,
    /// so the body is clipped to what is left.
    #[test]
    fn the_bar_is_as_tall_as_the_rows_it_needs() {
        let cell = CellFont::default();
        let verbs = same("copy", 3);
        let wide = rect(0.0, 0.0, 600.0, 400.0);
        assert_eq!(height(&verbs, &cell, wide), BAR_H, "one row, as ever");
        assert_eq!(height(&verbs, &cell, narrow(&cell)), rows_h(2));
        assert_eq!(height(&[], &cell, wide), 0.0, "no verbs, no bar");
    }

    /// It grows only so far. Past the last row a bar has to give — the
    /// three of [`MAX_ROWS`], or the one a short panel can spare — what is
    /// left over is dropped, exactly as it was when a bar was one row.
    #[test]
    fn a_bar_stops_at_the_last_row() {
        let cell = CellFont::default();
        let verbs = same("copy", 9);
        let panel = narrow(&cell);
        let h = height(&verbs, &cell, panel);
        assert_eq!(h, rows_h(MAX_ROWS), "three rows and no more");
        let out = entries(&verbs, &cell, strip(panel, h));
        assert_eq!(out.len(), MAX_ROWS * 2, "two to a row, and the rest gone");

        let short = rect(0.0, 0.0, panel.size.x, theme::HEAD_H + BAR_H);
        assert_eq!(
            height(&verbs, &cell, short),
            BAR_H,
            "a panel with no room for a second row keeps the one it had"
        );
    }

    /// A label no row is wide enough for ends the bar rather than being
    /// skipped: what is drawn is always a prefix of what the panel answered.
    #[test]
    fn a_label_too_long_for_a_row_ends_the_bar() {
        let cell = CellFont::default();
        let verbs = vec![
            Verb::run("v", "copy", None),
            Verb::run("v", "a label no panel is wide enough for", None),
            Verb::run("v", "copy", None),
        ];
        let panel = narrow(&cell);
        let out = entries(&verbs, &cell, strip(panel, rows_h(MAX_ROWS)));
        assert_eq!(out.len(), 1);
    }

    /// The focused panel promises everything it wears — that is what being
    /// the third step of the order means.
    #[test]
    fn the_focused_bar_wears_its_own_letters() {
        let v = bar(&['s', 'a', 'd']);
        let reach = Reach::Focused {
            kept: Letters::NONE,
        };
        assert_eq!(set(&v, reach), "ads");
    }

    /// What the widget above it keeps never arrives, so it is never bold:
    /// a caret in a field keeps the text chords at the least, and every
    /// field in this build keeps the lot.
    #[test]
    fn a_live_field_takes_letters_off_the_focused_bar() {
        let v = bar(&['s', 'a', 'd']);
        let text = Reach::Focused {
            kept: Letters::TEXT,
        };
        assert_eq!(set(&v, text), "ds", "cmd+a is select-all while it blinks");
        let all = Reach::Focused {
            kept: Letters::ALL,
        };
        assert_eq!(set(&v, all), "", "a field that answers every chord itself");
    }

    /// The previewed panel is the *fourth* step: it gets what the two above
    /// it leave, whether or not the focused bar drew its own letter bold.
    #[test]
    fn a_preview_wears_what_the_focused_bar_leaves_free() {
        let driver = bar(&['n', 'g', 'h']);
        let child = bar(&['n', 'g', 'h', 'p', 'm', 'd']);
        let free = Reach::Preview {
            kept: Letters::NONE,
            driver: &driver,
        };
        assert_eq!(set(&child, free), "dmp");
        // The driver's own widget has the keyboard: nothing reaches either
        // bar, so the preview promises nothing either.
        let none = Reach::Preview {
            kept: Letters::ALL,
            driver: &driver,
        };
        assert_eq!(set(&child, none), "");
    }

    /// Every other bar draws no letter at all, and no bar ever draws a
    /// reserved one — `check` asserts a bar may not wear one, and this
    /// holds even where it does.
    #[test]
    fn nothing_else_is_bold() {
        let v = bar(&['s', 'a', 'd']);
        assert_eq!(set(&v, Reach::Away), "");
        let reserved = bar(&['w', 'z', 'u', 't', 'i', 'l', 'o']);
        let reach = Reach::Focused {
            kept: Letters::NONE,
        };
        assert_eq!(set(&reserved, reach), "o");
    }
}
