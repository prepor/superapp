//! The overview swipe is global, including over a viewer that owns raw
//! touches. Observe it before forwarding and end that surface's contacts
//! before handing the gesture to overview.

use std::collections::BTreeMap;

use makepad_widgets::makepad_platform::event::{TouchPoint, TouchState, TouchUpdateEvent};
use makepad_widgets::*;

const SWIPE_DISTANCE: f64 = 24.0;

#[derive(Debug)]
struct Contact {
    start: DVec2,
    latest: TouchPoint,
}

#[derive(Debug, Default)]
pub struct OverviewGesture {
    contacts: BTreeMap<u64, Contact>,
    rejected: bool,
    consumed: bool,
}

#[derive(Debug)]
pub enum Gesture {
    /// Continue through the ordinary content and shell touch routing.
    Forward,
    /// End content's contacts with this event, then open overview.
    Open(TouchUpdateEvent),
    /// The swipe already opened overview; its remaining contacts are inert.
    Consume,
}

impl OverviewGesture {
    /// `enabled` controls starting a new overview swipe. A gesture already
    /// taken remains consumed until every contact lifts, even after Back.
    pub fn update(&mut self, event: &TouchUpdateEvent, enabled: bool) -> Gesture {
        let mut stopped = false;
        for point in &event.touches {
            match point.state {
                TouchState::Start => {
                    self.contacts.insert(
                        point.uid,
                        Contact {
                            start: point.abs,
                            latest: point.clone(),
                        },
                    );
                    if self.contacts.len() == 2 && !self.rejected && !self.consumed {
                        // Only motion made with both fingers down belongs
                        // to this gesture, not an earlier one-finger pan.
                        for contact in self.contacts.values_mut() {
                            contact.start = contact.latest.abs;
                        }
                    }
                    if self.contacts.len() > 2 {
                        self.rejected = true;
                    }
                }
                TouchState::Move | TouchState::Stable | TouchState::Stop => {
                    if let Some(contact) = self.contacts.get_mut(&point.uid) {
                        contact.latest = point.clone();
                    }
                    stopped |= point.state == TouchState::Stop;
                }
            }
        }

        if !enabled || stopped {
            self.rejected = true;
        }
        let mut open = None;
        if !self.consumed && !self.rejected && self.contacts.len() == 2 {
            let mut contacts = self.contacts.values();
            let a = contacts.next().unwrap();
            let b = contacts.next().unwrap();
            let da = a.latest.abs - a.start;
            let db = b.latest.abs - b.start;
            // A horizontal pan or a downward/pinch motion keeps its owner
            // for the remainder of this contact sequence.
            self.rejected = [da, db].iter().any(|d| {
                d.y >= SWIPE_DISTANCE || (d.x.abs() >= SWIPE_DISTANCE && d.x.abs() >= d.y.abs())
            });
            if !self.rejected && parallel_up(da, db) {
                let mut cancellation = event.clone();
                cancellation.touches = self
                    .contacts
                    .values()
                    .map(|contact| {
                        let mut point = contact.latest.clone();
                        point.state = TouchState::Stop;
                        point.time = event.time;
                        point.handled.set(Area::Empty);
                        point.sweep_lock.set(Area::Empty);
                        point
                    })
                    .collect();
                self.consumed = true;
                open = Some(cancellation);
            }
        }

        for point in &event.touches {
            if point.state == TouchState::Stop {
                self.contacts.remove(&point.uid);
            }
        }
        let consumed = self.consumed;
        if self.contacts.is_empty() {
            *self = Self::default();
        }
        match open {
            Some(cancellation) => Gesture::Open(cancellation),
            None if consumed => Gesture::Consume,
            None => Gesture::Forward,
        }
    }
}

fn parallel_up(a: DVec2, b: DVec2) -> bool {
    a.y < -SWIPE_DISTANCE
        && b.y < -SWIPE_DISTANCE
        && a.x.abs() < -a.y
        && b.x.abs() < -b.y
        // Translation moves both fingers by about the same vector. A
        // pinch may send both upward by very different distances.
        && (a - b).length() <= (-a.y).min(-b.y) * 0.5 + 8.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn touches(points: &[(u64, DVec2, TouchState)]) -> TouchUpdateEvent {
        TouchUpdateEvent {
            time: 1.0,
            window_id: CxWindowPool::id_zero(),
            modifiers: KeyModifiers::default(),
            touches: points
                .iter()
                .map(|&(uid, abs, state)| TouchPoint {
                    uid,
                    abs,
                    state,
                    time: 1.0,
                    rotation_angle: 0.0,
                    force: 1.0,
                    radius: DVec2::default(),
                    handled: Cell::new(Area::Empty),
                    sweep_lock: Cell::new(Area::Empty),
                })
                .collect(),
        }
    }

    fn start(gesture: &mut OverviewGesture) {
        assert!(matches!(
            gesture.update(
                &touches(&[
                    (1, dvec2(100.0, 300.0), TouchState::Start),
                    (2, dvec2(200.0, 300.0), TouchState::Start),
                ]),
                true
            ),
            Gesture::Forward
        ));
    }

    #[test]
    fn upward_swipe_cancels_every_content_contact_once_and_eats_the_release() {
        let mut gesture = OverviewGesture::default();
        start(&mut gesture);
        let moved = touches(&[
            (1, dvec2(101.0, 260.0), TouchState::Move),
            (2, dvec2(202.0, 261.0), TouchState::Move),
        ]);
        let Gesture::Open(cancel) = gesture.update(&moved, true) else {
            panic!("parallel upward movement opens overview");
        };
        assert_eq!(cancel.touches.len(), 2);
        for (stop, original) in cancel.touches.iter().zip(&moved.touches) {
            assert_eq!(stop.uid, original.uid);
            assert_eq!(stop.abs, original.abs);
            assert_eq!(stop.state, TouchState::Stop);
        }
        assert!(matches!(gesture.update(&moved, false), Gesture::Consume));
        assert!(matches!(
            gesture.update(
                &touches(&[(1, dvec2(101.0, 260.0), TouchState::Stop),]),
                true
            ),
            Gesture::Consume
        ));
        assert!(matches!(
            gesture.update(
                &touches(&[(2, dvec2(202.0, 261.0), TouchState::Stop),]),
                true
            ),
            Gesture::Consume
        ));
        assert!(matches!(
            gesture.update(
                &touches(&[(3, dvec2(100.0, 300.0), TouchState::Start),]),
                true
            ),
            Gesture::Forward
        ));
    }

    #[test]
    fn pinch_horizontal_pan_and_downward_swipe_remain_with_content() {
        for (a, b) in [
            (dvec2(0.0, -60.0), dvec2(0.0, 60.0)),
            (dvec2(0.0, -100.0), dvec2(0.0, -30.0)),
            (dvec2(-60.0, -30.0), dvec2(60.0, -30.0)),
            (dvec2(60.0, -10.0), dvec2(60.0, -10.0)),
            (dvec2(0.0, 60.0), dvec2(0.0, 60.0)),
        ] {
            let mut gesture = OverviewGesture::default();
            start(&mut gesture);
            assert!(matches!(
                gesture.update(
                    &touches(&[
                        (1, dvec2(100.0, 300.0) + a, TouchState::Move),
                        (2, dvec2(200.0, 300.0) + b, TouchState::Move),
                    ]),
                    true
                ),
                Gesture::Forward
            ));
        }
    }

    #[test]
    fn both_fingers_must_move_after_the_second_lands() {
        let mut gesture = OverviewGesture::default();
        gesture.update(
            &touches(&[(1, dvec2(100.0, 300.0), TouchState::Start)]),
            true,
        );
        gesture.update(
            &touches(&[(1, dvec2(100.0, 200.0), TouchState::Move)]),
            true,
        );
        gesture.update(
            &touches(&[(2, dvec2(200.0, 200.0), TouchState::Start)]),
            true,
        );
        assert!(matches!(
            gesture.update(
                &touches(&[(2, dvec2(200.0, 160.0), TouchState::Move),]),
                true
            ),
            Gesture::Forward
        ));
        assert!(matches!(
            gesture.update(
                &touches(&[(1, dvec2(100.0, 160.0), TouchState::Move),]),
                true
            ),
            Gesture::Open(_)
        ));
    }

    #[test]
    fn lifting_or_adding_a_third_finger_cannot_turn_into_an_overview_swipe() {
        for interrupt in [TouchState::Stop, TouchState::Start] {
            let mut gesture = OverviewGesture::default();
            start(&mut gesture);
            let uid = if interrupt == TouchState::Stop { 2 } else { 3 };
            gesture.update(&touches(&[(uid, dvec2(200.0, 300.0), interrupt)]), true);
            assert!(matches!(
                gesture.update(
                    &touches(&[
                        (1, dvec2(100.0, 250.0), TouchState::Move),
                        (2, dvec2(200.0, 250.0), TouchState::Move),
                    ]),
                    true
                ),
                Gesture::Forward
            ));
        }
    }
}
