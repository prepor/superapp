//! The scrub: a press on the strip's hairline, a drag, a release — one
//! seek per step, keeping the bar the press landed on through the whole
//! drag, even outside it, through clipping or a changing time label.

use makepad_widgets::*;

use super::SeekBar;

/// A drag over a seek bar, with the host's own name for what is sought —
/// a message, a clip's key, nothing.
pub struct Scrub<T = ()> {
    held: Option<(T, SeekBar)>,
}

impl<T> Default for Scrub<T> {
    fn default() -> Self {
        Scrub { held: None }
    }
}

impl<T: Clone> Scrub<T> {
    /// Whether a drag is under way.
    #[must_use]
    pub fn active(&self) -> bool {
        self.held.is_some()
    }

    /// A fresh press, a lost window, a source gone: the drag ends.
    pub fn cancel(&mut self) {
        self.held = None;
    }

    /// Reads one event. `bar` answers a primary press with the bar under
    /// it, if the press is this host's to answer; a press elsewhere ends
    /// any drag. Returns what to seek and where to, when the event moves
    /// the position.
    pub fn handle(
        &mut self,
        event: &Event,
        bar: impl FnOnce(DVec2) -> Option<(T, SeekBar)>,
    ) -> Option<(T, f64)> {
        match event {
            Event::MouseDown(e) if e.button == MouseButton::PRIMARY => {
                self.held = None;
                let (what, on) = bar(e.abs)?;
                if !on.rect.contains(e.abs) {
                    return None;
                }
                self.held = Some((what.clone(), on));
                Some((what, on.position(e.abs.x)))
            }
            Event::MouseMove(e) => {
                let (what, on) = self.held.as_ref()?;
                Some((what.clone(), on.position(e.abs.x)))
            }
            Event::MouseUp(e) if e.button == MouseButton::PRIMARY => {
                let (what, on) = self.held.take()?;
                Some((what, on.position(e.abs.x)))
            }
            Event::WindowLostFocus(_) | Event::Background => {
                self.held = None;
                None
            }
            _ => None,
        }
    }
}
