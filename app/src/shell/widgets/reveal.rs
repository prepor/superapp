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
        // Ordinary widget redraws are ignored during a draw event. This
        // measured adjustment needs another paint even when no input,
        // caret timer or background update wakes the application again.
        cx.redraw_area_in_draw(list.area());
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

    /// Exercise Makepad's real draw context without a replay timer or a
    /// stream of input events to accidentally wake up a stalled reveal.
    #[cfg(headless)]
    #[test]
    fn revealing_schedules_its_own_draws() {
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;

        for target in [1, 7] {
            let finished = Rc::new(Cell::new(false));
            let seen = finished.clone();
            let mut list: Option<PortalListRef> = None;
            let mut pass = None;
            let mut draw_list: Option<DrawList> = None;
            let mut reveal = Reveal::default();
            let mut frames = 0;
            let cx = Rc::new(RefCell::new(Cx::new(Box::new(move |cx, event| {
                match event {
                    Event::Startup => {
                        let widget = cx.with_vm(|vm| {
                            WidgetRef::new_with_inner(Box::new(PortalList::script_new(vm)))
                        });
                        makepad_widgets::widget_tree::set_ui_root(cx, &widget);
                        let portal = widget.as_portal_list();
                        portal.set_flow(cx, Flow::Down);
                        list = Some(portal);
                        let p = DrawPass::new(cx);
                        p.set_size(cx, dvec2(300.0, 300.0));
                        pass = Some(p);
                        draw_list = Some(DrawList::new(cx));
                        reveal.request(target);
                        cx.redraw_all();
                    }
                    Event::Draw(event) => {
                        frames += 1;
                        let mut draw = CxDraw::new(cx, event);
                        let pass = pass.as_ref().unwrap();
                        draw.begin_pass(pass, Some(1.0));
                        let draw_list = draw_list.as_mut().unwrap();
                        draw_list.begin_always(&mut draw);
                        let mut cx = Cx2d::new(&mut draw);
                        cx.begin_root_turtle(dvec2(300.0, 300.0), Layout::default());
                        let list = list.as_ref().unwrap();
                        let mut row = None;
                        while list.draw_walk(&mut cx, &mut Scope::empty(), Walk::fixed(300.0, 300.0)).is_step() {
                            let mut list = list.borrow_mut().unwrap();
                            list.set_item_range(&mut cx, 0, 10);
                            while let Some(index) = list.next_visible_item(&mut cx) {
                                let height = match index { 0 => 220.0, 1 => 120.0, _ => 48.0 };
                                let rect = cx.walk_turtle(Walk::fixed(300.0, height));
                                if index == target { row = Some(rect); }
                            }
                        }
                        cx.end_turtle();
                        if reveal.apply(&mut cx, list, Some(target), row) {
                            assert!(cx.new_draw_event.draw_lists.contains(&list.area().draw_list_id().unwrap()),
                                "a reveal must request its next draw from inside the current draw");
                        } else {
                            assert!(row.is_some());
                            assert_eq!(reveal.target(), None);
                            assert!(frames <= 3, "revealing a row must not wait for a later input event");
                            seen.set(true);
                        }
                        draw_list.end(&mut draw);
                        draw.end_pass(pass);
                    }
                    _ => {}
                }
            }))));
            Cx::headless_event_loop_for_draw_cycles(cx, 4);
            assert!(finished.get(), "the reveal stalled with no input or timer events");
        }
    }

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
