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

use kernel::panel::Verb;
use makepad_widgets::*;

use super::draw::CellFont;
use super::keys::Letters;

/// The strip's height, rule included.
pub const BAR_H: f64 = 26.0;
/// A bar entry's height.
pub const ENTRY_H: f64 = 18.0;
/// The inset at either end of the strip.
pub const PAD_X: f64 = 8.0;
/// Between two entries.
pub const GAP: f64 = 6.0;
/// A button's label sits inside this much padding.
pub const BTN_PAD: f64 = 12.0;

/// Where one entry landed, and which verb it draws.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// The verb's index in the panel's own list.
    pub at: usize,
    pub rect: Rect,
    /// A button runs; a link goes.
    pub button: bool,
}

/// The strip at the foot of a panel's rectangle.
#[must_use]
pub fn strip(panel: Rect) -> Rect {
    Rect {
        pos: dvec2(panel.pos.x + 1.0, panel.pos.y + panel.size.y - BAR_H - 1.0),
        size: dvec2((panel.size.x - 2.0).max(0.0), BAR_H),
    }
}

/// Lays the verbs out left to right in the strip. Entries that would run
/// past the right edge are dropped: a bar is a row, never a wrap.
#[must_use]
pub fn entries(verbs: &[Verb], cell: &CellFont, strip: Rect) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut x = strip.pos.x + PAD_X;
    let y = strip.pos.y + (BAR_H - ENTRY_H) / 2.0;
    for (at, v) in verbs.iter().enumerate() {
        let button = v.act.button();
        let w = cell.label_w(v.label.chars().count()) + if button { BTN_PAD } else { 0.0 };
        if x + w > strip.pos.x + strip.size.x - PAD_X {
            break;
        }
        out.push(Entry {
            at,
            rect: Rect {
                pos: dvec2(x, y),
                size: dvec2(w, ENTRY_H),
            },
            button,
        });
        x += w + GAP;
    }
    out
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
