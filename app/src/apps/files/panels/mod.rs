//! The two panels: a directory as a list, a file as a card.

use kernel::layout::SlotId;
use kernel::session::Session;

pub mod card;
pub mod dir;

pub use card::Card;
pub use dir::Dir;

/// Lists every files panel again — every one but `by`, which is the
/// instance that is calling and is borrowed by whoever called it.
///
/// A verb that wrote says so itself rather than waiting to be told: the
/// answer is here, now, and on every platform, where a watch is a round
/// later and only on the two that have one. It says it to all of them,
/// since a copy changes the directory it came from as well as the one it
/// landed in.
pub fn refresh(s: &mut Session, by: Option<SlotId>) {
    for (slot, inst) in s.panels() {
        if by == Some(slot) {
            continue;
        }
        let mut p = inst.borrow_mut();
        if let Some(d) = p.as_any().downcast_mut::<Dir>() {
            d.relist();
        } else if let Some(c) = p.as_any().downcast_mut::<Card>() {
            c.restat();
        }
    }
    s.redraw();
}
