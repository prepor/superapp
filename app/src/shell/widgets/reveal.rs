//! Bring a virtual list row into view using its measured rectangle. A jump
//! remains pending until a draw confirms it landed; row counts cannot stand
//! in for pixels when rows have different heights.

use makepad_widgets::*;

pub struct Reveal<K> {
    target: Option<K>,
}

impl<K> Default for Reveal<K> {
    fn default() -> Self { Self { target: None } }
}

impl<K: Copy> Reveal<K> {
    pub fn request(&mut self, target: K) { self.target = Some(target); }
    pub fn target(&self) -> Option<K> { self.target }
    pub fn cancel(&mut self) { self.target = None; }

    /// Called after the list draws, with the target's current index and its
    /// rectangle if it was drawn. Returns whether another draw is needed.
    pub fn apply(
        &mut self, cx: &mut Cx, list: &PortalListRef,
        index: Option<usize>, row: Option<Rect>,
    ) -> bool {
        if self.target.is_none() { return false; }
        let Some(index) = index else { self.cancel(); return false; };
        let clip = list.area().rect(cx);
        if clip.size.y <= 0.0 { return false; }
        let row = row.filter(|r| r.size.y > 0.0);
        let shift = row.map(|r| adjustment(r, clip));
        if shift.is_some_and(|d| d.abs() < 0.5) {
            self.cancel();
            return false;
        }
        list.set_tail_range(false);
        if let Some(mut inner) = list.borrow_mut() {
            match shift {
                Some(shift) => {
                    let first = inner.first_id();
                    let scroll = inner.first_scroll();
                    inner.set_first_id_and_scroll(first, scroll + shift);
                }
                None => inner.set_first_id_and_scroll(index, 0.0),
            }
        }
        list.redraw(cx);
        true
    }
}

/// The smallest pixel movement that reveals a row. An oversized row shows
/// its beginning instead of oscillating between its two clipped ends.
fn adjustment(row: Rect, clip: Rect) -> f64 {
    let height = row.size.y.min(clip.size.y);
    if row.pos.y < clip.pos.y {
        clip.pos.y - row.pos.y
    } else {
        (clip.pos.y + clip.size.y - row.pos.y - height).min(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revealing_uses_pixels_and_handles_rows_taller_than_the_view() {
        let rect = |y, h| Rect { pos: dvec2(0.0, y), size: dvec2(300.0, h) };
        let viewport = rect(100.0, 400.0);
        assert_eq!(adjustment(rect(120.0, 30.0), viewport), 0.0);
        assert_eq!(adjustment(rect(80.0, 30.0), viewport), 20.0);
        assert_eq!(adjustment(rect(480.0, 70.0), viewport), -50.0);
        assert_eq!(adjustment(rect(520.0, 200.0), viewport), -220.0);
        assert_eq!(adjustment(rect(120.0, 900.0), viewport), -20.0);
        assert_eq!(adjustment(rect(100.0, 900.0), viewport), 0.0);
    }
}
