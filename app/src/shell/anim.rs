//! Springs towards the scene's targets.
//!
//! The kernel computes discrete targets ([`kernel::layout::Scene`]); this
//! interpolates towards them. It uses total elapsed time, so dropped frames
//! do not change the path, and retargeting preserves position and speed.

use std::collections::{HashMap, HashSet};

use kernel::layout::{Rect, Scene, SlotId};
use kernel::spring::{Spring, SpringParams};

/// One slot's animated rectangle.
#[derive(Debug, Clone)]
pub struct PanelAnim {
    x: Spring,
    y: Spring,
    w: Spring,
    h: Spring,
    pub alpha: Spring,
    pub title: String,
    /// Which workspace it lives on — its row in the vertical stack.
    pub ws: usize,
}

impl PanelAnim {
    fn spawn(target: Rect, title: String, visible: bool, ws: usize) -> PanelAnim {
        // Born slightly inset and transparent; springs carry it to place. A
        // panel born hidden (an inactive tab) just sits at its rect at rest.
        let inset = if visible { 12.0 } else { 0.0 };
        let mk = |v| Spring::at_rest(v, SpringParams::movement());
        let mut pa = PanelAnim {
            x: mk(target.x + inset),
            y: mk(target.y + inset),
            w: mk(target.w - 2.0 * inset),
            h: mk(target.h - 2.0 * inset),
            alpha: Spring::at_rest(0.0, SpringParams::fade()),
            title,
            ws,
        };
        pa.retarget(target);
        if visible {
            pa.alpha.retarget(1.0);
        }
        pa
    }

    fn retarget(&mut self, t: Rect) {
        self.x.retarget(t.x);
        self.y.retarget(t.y);
        self.w.retarget(t.w);
        self.h.retarget(t.h);
    }

    /// Puts the panel's corner where a finger is holding it. What a
    /// long-pressed panel rides on: the springs still carry it, so it
    /// trails the finger rather than sticking to it.
    pub fn retarget_pos(&mut self, x: f64, y: f64) {
        self.x.retarget(x);
        self.y.retarget(y);
    }

    #[must_use]
    pub fn rect(&self) -> Rect {
        Rect {
            x: self.x.value(),
            y: self.y.value(),
            w: self.w.value(),
            h: self.h.value(),
        }
    }

    fn advance(&mut self, dt: f64) {
        self.x.advance(dt);
        self.y.advance(dt);
        self.w.advance(dt);
        self.h.advance(dt);
        self.alpha.advance(dt);
    }

    fn is_done(&self) -> bool {
        self.x.is_done()
            && self.y.is_done()
            && self.w.is_done()
            && self.h.is_done()
            && self.alpha.is_done()
    }
}

/// A closed panel's chrome, fading out where it stood.
#[derive(Debug, Clone)]
pub struct Ghost {
    pub rect: Rect,
    pub alpha: Spring,
    pub title: String,
    /// The workspace row the panel died on.
    pub ws: usize,
}

/// Drawn state: springs keyed by slot, plus fading ghosts of closed ones.
#[derive(Debug, Default)]
pub struct Anim {
    camera: Option<Spring>,
    /// The camera's place in the vertical workspace stack, in rows — it
    /// springs between numbers on a switch.
    slide: Option<Spring>,
    pub panels: HashMap<SlotId, PanelAnim>,
    pub ghosts: Vec<Ghost>,
    /// The overlay chassis' presence, 0 (away) → 1 (up): the wash, the
    /// sheet and its contents ride it together.
    overlay: Option<Spring>,
}

impl Anim {
    pub fn camera(&mut self) -> &mut Spring {
        self.camera
            .get_or_insert_with(|| Spring::at_rest(0.0, SpringParams::movement()))
    }

    pub fn overlay(&mut self) -> &mut Spring {
        self.overlay
            .get_or_insert_with(|| Spring::at_rest(0.0, SpringParams::overlay()))
    }

    pub fn slide(&mut self) -> &mut Spring {
        self.slide
            .get_or_insert_with(|| Spring::at_rest(0.0, SpringParams::movement()))
    }

    /// Applies a fresh scene: retarget the living, spawn the new, ghost the
    /// gone.
    ///
    /// The kernel computes one scene, for the active workspace. Panels on
    /// the others keep the targets they had, so a switch slides between two
    /// settled pictures rather than ghosting the space being left.
    pub fn apply(&mut self, scene: &Scene, active: usize, titles: &HashMap<SlotId, String>) {
        self.camera().retarget(scene.camera_x);
        self.slide().retarget(active as f64);
        let mut seen: HashSet<SlotId> = HashSet::new();
        for ps in &scene.slots {
            seen.insert(ps.id);
            let title = titles.get(&ps.id).cloned().unwrap_or_default();
            match self.panels.get_mut(&ps.id) {
                Some(pa) => {
                    pa.retarget(ps.rect);
                    // A tab switch is a crossfade in place, never open/close.
                    pa.alpha.retarget(if ps.visible { 1.0 } else { 0.0 });
                    pa.title = title;
                    pa.ws = active;
                }
                None => {
                    self.panels
                        .insert(ps.id, PanelAnim::spawn(ps.rect, title, ps.visible, active));
                }
            }
        }
        // Only what the *active* workspace lost is gone: a slot on another
        // space is simply not in this scene.
        let gone: Vec<SlotId> = self
            .panels
            .iter()
            .filter(|(id, pa)| pa.ws == active && !seen.contains(id))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            let Some(pa) = self.panels.remove(&id) else {
                continue;
            };
            let mut alpha = pa.alpha;
            alpha.retarget(0.0);
            self.ghosts.push(Ghost {
                rect: pa.rect(),
                alpha,
                title: pa.title,
                ws: pa.ws,
            });
        }
    }

    /// Forgets the springs of slots nothing shows any more — what a
    /// workspace switch leaves behind once its panels are closed elsewhere.
    pub fn retain(&mut self, live: &HashSet<SlotId>) {
        self.panels.retain(|id, _| live.contains(id));
    }

    /// Advances every spring; answers whether anything is still moving.
    pub fn advance(&mut self, dt: f64) -> bool {
        let mut active = false;
        for s in [
            self.camera.as_mut(),
            self.slide.as_mut(),
            self.overlay.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            s.advance(dt);
            active |= !s.is_done();
        }
        for pa in self.panels.values_mut() {
            pa.advance(dt);
            active |= !pa.is_done();
        }
        for g in &mut self.ghosts {
            g.alpha.advance(dt);
        }
        self.ghosts.retain(|g| !g.alpha.is_done());
        active |= !self.ghosts.is_empty();
        active
    }
}
