//! The widgets that draw the course's panels. Each borrows its instance
//! from the scope, reads what it shows off the store through the
//! instance's own queries, and hands presses and keys back to it.

use makepad_widgets::*;

use crate::shell::hosted::PanelProps;

mod card;
mod desk;
mod import;
mod lesson;
mod lists;
mod progress;
mod review;
mod topic;

pub use card::CardPanel;
pub use desk::DeskPanel;
pub use import::ImportPanel;
pub use lesson::LessonPanel;
pub use lists::{CardsPanel, GrammarPanel, HistoryPanel};
pub use progress::ProgressPanel;
pub use review::ReviewPanel;
pub use topic::TopicPanel;

/// Runs `f` on the instance. The borrow lasts exactly as long as the call:
/// a navigation taken while it stood would find the session walking the
/// same instance.
pub(super) fn with<P: 'static, R>(props: &PanelProps, f: impl FnOnce(&mut P) -> R) -> Option<R> {
    let mut borrow = props.panel.borrow_mut();
    let p = borrow.as_any().downcast_mut::<P>()?;
    Some(f(p))
}

/// Sets a cell's, a bar's or a strip's `level` uniform, 0 to 1.
pub(super) fn set_level(cx: &mut Cx, view: &ViewRef, level: f64) {
    if let Some(mut v) = view.borrow_mut() {
        v.draw_bg
            .set_uniform(cx, live_id!(level), &[level.clamp(0.0, 1.0) as f32]);
    }
}

/// Five cells for a mastery of 0 to 5.
pub(super) fn set_stars(cx: &mut Cx, stars: &ViewRef, mastery: i64) {
    for (i, id) in [live_id!(s0), live_id!(s1), live_id!(s2), live_id!(s3), live_id!(s4)]
        .iter()
        .enumerate()
    {
        let lit = (i as i64) < mastery;
        set_level(cx, &stars.view(cx, &[*id]), if lit { 1.0 } else { 0.0 });
    }
}

/// Registers a drawn label's text as a hit, so a script can assert on it
/// and a pointer gets the text cursor over it.
pub(super) fn text_hit(cx: &mut Cx, props: &PanelProps, label: &LabelRef, clip: Option<Rect>) {
    let text = label.text();
    if text.trim().is_empty() {
        return;
    }
    let r = label.area().rect(cx);
    if r.size.x <= 0.0 {
        return;
    }
    // Clipped even where nothing clips it: a hit with its full bounds
    // recorded is what a script's `visible` reads.
    props.hits.add_clipped(text, r, clip.unwrap_or(r), MouseCursor::Default, props.slot);
}

/// `1 · 2 · 3` — a list of words on one muted line.
pub(super) fn dots(parts: &[String]) -> String {
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}
