//! The bridge between a script and the shell's own input paths.
//!
//! One step per tick. A `click` resolves its label to a rectangle and
//! synthesizes a real press and release at its centre, so the widget under
//! it handles the press the way it handles a finger's; keys and text go
//! through the same handlers a keyboard reaches. Nothing here has a path of
//! its own — that is the whole point of it.
//!
//! The clock is virtual: the runner is handed a `dt` per tick and counts
//! milliseconds down itself, so `wait 600` is exactly 36 frames whether the
//! machine is idle or running twelve other suites.

use std::path::PathBuf;

use kernel::caps::Shot;
use kernel::e2e::{Runner, Step};
use makepad_widgets::*;

use super::boot::{self, Frame};
use super::keys::ChordExec;
use super::stage::{Shell, Stage};

/// The windowed runner is paced by a real timer at this interval; the
/// runner counts the same milliseconds either way.
pub const E2E_TICK_MS: f64 = 30.0;

/// How many frames a `shot` will wait for a picture before taking whatever
/// the rasterizer has and saying so.
///
/// Enough to outlast a shader compile — that is wall-clock time the virtual
/// clock knows nothing about — and few enough that a run which is never
/// going to get its frame fails in seconds rather than minutes. In practice
/// a shot waits one.
const SHOT_PATIENCE: u32 = 240;

/// The two fingers a script has. Any two numbers: what matters is that they
/// are told apart, exactly as the platform's own uids are.
const FINGER_A: u64 = 1;
const FINGER_B: u64 = 2;

/// How many moves a scripted gesture is made of. Enough that the first one
/// clears the slop and the rest belong to the mode it locked.
const STEPS: u32 = 8;

/// A `shot` that has asked for its frame and is waiting for it.
///
/// The harness runs from the frame event and the draw is *after* it, so at
/// the moment a `shot` step runs, the newest frame on disk is the one before
/// its own. It notes where the rasterizer was, asks for a draw, and copies
/// nothing until a later frame exists — which is the frame its step drew.
pub struct PendingShot {
    name: String,
    path: PathBuf,
    /// Where the rasterizer was when the step ran; `None` when there is no
    /// counter to wait on.
    mark: Option<u64>,
    /// Frames gone by since, for the report and for the patience.
    waited: u32,
}

impl Stage {
    /// Executes at most one step per tick; waits pace the script.
    pub(super) fn e2e_tick(&mut self, cx: &mut Cx, sh: &mut Shell, dt_ms: f64) {
        let Some(mut runner) = self.e2e.take() else {
            return;
        };
        // A `shot` in flight owns the tick: no step is taken and no time
        // passes until the rasterizer has written the frame it asked for,
        // so the picture is the state at that step and not a frame later.
        if self.shot.is_some() {
            self.poll_shot(sh, &mut runner);
            self.e2e = Some(runner);
            return;
        }
        if let Some(step) = runner.next_step(dt_ms) {
            if self.e2e_step(cx, sh, &mut runner, step) {
                // `quit` drops the runner rather than restoring it: a
                // finished run that stayed live would spin the rasterizer
                // flat out until the whole draw budget was gone.
                return;
            }
        }
        self.e2e = Some(runner);
    }

    /// One step. Answers whether the run is over.
    fn e2e_step(&mut self, cx: &mut Cx, sh: &mut Shell, r: &mut Runner, step: Step) -> bool {
        // A mount's last step is where it stops: the state on the canvas.
        // The runner goes with it — nothing else to do, and a `quit` here
        // would take the whole window with it.
        if self.is_mount() && matches!(step, Step::Shot(_) | Step::Quit) && r.idx >= r.steps.len() {
            self.arrive(r);
            return true;
        }
        match step {
            Step::Wait(_) => {}

            Step::Shot(name) if self.no_draw => {
                eprintln!("e2e: shot {name} (skipped: --no-draw)");
            }
            Step::Shot(name) => {
                let path = r.out.join(format!("{name}.png"));
                // A mount draws into the canvas's pass, not into a window
                // frame of its own: there is nothing of its own to wait for.
                let mark = (!self.is_mount()).then(boot::frame_mark).flatten();
                self.shot = Some(PendingShot {
                    name,
                    path,
                    mark,
                    waited: 0,
                });
                if mark.is_none() {
                    // Nothing of its own to wait for: no rasterizer counter
                    // to read, or a mount, whose frame is the canvas's.
                    self.take_shot(sh, r);
                } else {
                    // The frame this shot is about is the one this tick's
                    // draw will write. Ask for it.
                    cx.redraw_all();
                }
            }

            // Whole-label matches win over substrings, so an exact name is
            // an exact target (see `hits::label_rank`).
            Step::Click { label, fresh } => match self.hits.by_label(&label) {
                Some(h) => {
                    eprintln!("e2e: click {label:?}{}", if fresh { " (cmd)" } else { "" });
                    self.synth_click(cx, sh, h.rect.pos + h.rect.size / 2.0, fresh);
                }
                None => self.no_such(r, "click", &label),
            },

            // The same path; kept as its own word because a suite says
            // `mouse` when it means "prove the focus side effects too".
            Step::Mouse { label } => match self.hits.by_label(&label) {
                Some(h) => {
                    eprintln!("e2e: mouse {label:?}");
                    self.synth_click(cx, sh, h.rect.pos + h.rect.size / 2.0, false);
                }
                None => self.no_such(r, "mouse", &label),
            },

            Step::Key { chord, times } => match super::keys::parse_chord(&chord) {
                Some(exec) => {
                    eprintln!("e2e: key {chord} ×{times}");
                    for _ in 0..times.max(1) {
                        match &exec {
                            ChordExec::Ev(ev) => self.handle_key_down(cx, sh, ev),
                            ChordExec::Text(s) => self.handle_text(cx, sh, s),
                            ChordExec::Tap(code) => {
                                // A bare modifier press-release, the way
                                // flagsChanged delivers it: the modifier is
                                // set on the down and gone on the up.
                                let down = KeyEvent {
                                    key_code: *code,
                                    modifiers: KeyModifiers {
                                        logo: *code == KeyCode::Logo,
                                        ..Default::default()
                                    },
                                    is_repeat: false,
                                    time: sh.session.now(),
                                };
                                let mut up = down;
                                up.modifiers = KeyModifiers::default();
                                self.handle_key_down(cx, sh, &down);
                                self.handle_key_up(cx, sh, &up);
                            }
                        }
                    }
                }
                None => {
                    eprintln!("{}e2e: FAIL key {chord:?}: cannot parse chord", r.tag);
                    r.failures += 1;
                }
            },

            Step::Type(s) => {
                eprintln!("e2e: type {s:?}");
                self.handle_text(cx, sh, &s);
            }

            Step::Drag { label, dx, dy } => match self.hits.by_label(&label) {
                Some(h) => {
                    // From the left edge, so a horizontal drag sweeps the
                    // run rather than starting halfway through it.
                    let c = dvec2(h.rect.pos.x + 2.0, h.rect.pos.y + h.rect.size.y / 2.0);
                    eprintln!("e2e: drag {label:?} by ({dx}, {dy})");
                    self.synth_drag(cx, sh, c, dvec2(c.x + dx, c.y + dy));
                }
                None => self.no_such(r, "drag", &label),
            },

            Step::SelectAll(label) => match self.hits.by_label(&label) {
                Some(h) => {
                    eprintln!("e2e: selectall {label:?}");
                    // Just inside the run's top-left corner: the middle of
                    // a tall body lies below the viewport, where nothing is.
                    let p = h.rect.pos + dvec2(4.0, 4.0);
                    let w = h.slot.and_then(|s| self.hosted.get(&s).cloned());
                    let mut runs = Vec::new();
                    if let Some(w) = w {
                        w.find_widgets_from_point(cx, p, &mut |x| runs.push(x.clone()));
                    }
                    if runs.is_empty() {
                        eprintln!("{}e2e: FAIL selectall {label:?}: nothing under it", r.tag);
                        r.failures += 1;
                    }
                    // Two ways to select, because there are two kinds of
                    // selectable thing. A text flow or a page answers the
                    // widget call; a field does not — its selection is its
                    // own, and `select_all` is what a human's triple-click
                    // reaches too, so this is the wash a person would see.
                    for run in &runs {
                        run.selection_select_all();
                        if let Some(mut t) = run.as_text_input().borrow_mut() {
                            t.select_all(cx);
                        }
                    }
                    cx.redraw_all();
                }
                None => self.no_such(r, "selectall", &label),
            },

            // The touch steps go down the stage's own finger path — the one
            // android drives — so a suite that asks for a gesture proves the
            // gesture and not a shortcut to its result.
            Step::Swipe {
                label,
                dx,
                dy,
                hold,
            } => match self.hits.by_label(&label) {
                Some(h) => {
                    let c = h.rect.pos + h.rect.size / 2.0;
                    eprintln!("e2e: swipe {label:?} by ({dx}, {dy})");
                    self.touch_start(FINGER_A, c);
                    for i in 1..=STEPS {
                        let f = f64::from(i) / f64::from(STEPS);
                        self.touch_move(cx, sh, FINGER_A, dvec2(c.x + dx * f, c.y + dy * f));
                    }
                    // A whole sweep runs inside one tick and so never draws:
                    // `hold` leaves the finger down long enough to photograph.
                    if !hold {
                        self.touch_stop(cx, sh, FINGER_A, dvec2(c.x + dx, c.y + dy));
                    }
                }
                None => self.no_such(r, "swipe", &label),
            },

            Step::Pan2 { dx, dy } => {
                eprintln!("e2e: pan2 by ({dx}, {dy})");
                let vp = sh.viewport;
                let mid = self.origin + dvec2(vp.x / 2.0, vp.y / 2.0);
                let (a, b) = (mid - dvec2(40.0, 0.0), mid + dvec2(40.0, 0.0));
                self.touch_start(FINGER_A, a);
                self.touch_start(FINGER_B, b);
                for i in 1..=STEPS {
                    let f = f64::from(i) / f64::from(STEPS);
                    self.touch_move(cx, sh, FINGER_A, dvec2(a.x + f * dx, a.y + f * dy));
                    self.touch_move(cx, sh, FINGER_B, dvec2(b.x + f * dx, b.y + f * dy));
                }
                self.touch_stop(cx, sh, FINGER_A, dvec2(a.x + dx, a.y + dy));
                self.touch_stop(cx, sh, FINGER_B, dvec2(b.x + dx, b.y + dy));
            }

            Step::HoldMove {
                label,
                dx,
                dy,
                hold,
            } => match self
                .hits
                .panel_by_label(&label)
                .map(|h| {
                    // A panel is pressed on its header, which is the part
                    // that grabs — and a panel wins over a control wearing
                    // the same word, since this step is about picking things
                    // up rather than about clicking them.
                    dvec2(
                        h.rect.pos.x + h.rect.size.x / 2.0,
                        h.rect.pos.y + kernel::theme::HEAD_H / 2.0,
                    )
                })
                .or_else(|| {
                    self.hits
                        .by_label(&label)
                        .map(|h| h.rect.pos + h.rect.size / 2.0)
                }) {
                Some(c) => {
                    eprintln!("e2e: holdmove {label:?} by ({dx}, {dy})");
                    self.touch_start(FINGER_A, c);
                    self.long_press(cx, sh, FINGER_A, c);
                    if matches!(self.touch.mode, super::touch::Mode::Drag { .. }) {
                        for i in 1..=STEPS {
                            let f = f64::from(i) / f64::from(STEPS);
                            self.touch_move(cx, sh, FINGER_A, dvec2(c.x + dx * f, c.y + dy * f));
                        }
                        if !hold {
                            self.touch_stop(cx, sh, FINGER_A, dvec2(c.x + dx, c.y + dy));
                        }
                    } else if dx != 0.0 || dy != 0.0 {
                        // A step that asked to move something and picked
                        // nothing up moved nothing: that is a failure, not a
                        // long press with a short tail.
                        eprintln!(
                            "{}e2e: FAIL holdmove {label:?}: nothing was picked up",
                            r.tag
                        );
                        r.failures += 1;
                        self.touch_stop(cx, sh, FINGER_A, c);
                    } else {
                        self.touch_stop(cx, sh, FINGER_A, c);
                    }
                }
                None => self.no_such(r, "holdmove", &label),
            },

            Step::Drop => {
                let held = match self.touch.mode {
                    super::touch::Mode::Drag { uid, .. } | super::touch::Mode::Row { uid } => {
                        Some(uid)
                    }
                    _ => None,
                };
                match held {
                    Some(uid) => {
                        let p = self.touch.pts.get(&uid).map_or(self.origin, |&(_, p)| p);
                        eprintln!("e2e: drop");
                        self.touch_stop(cx, sh, uid, p);
                    }
                    None => {
                        eprintln!("{}e2e: FAIL drop: no gesture is being held", r.tag);
                        r.failures += 1;
                    }
                }
            }

            Step::Quit => {
                eprintln!(
                    "e2e: done — {} step(s), {} failure(s)",
                    r.steps.len(),
                    r.failures
                );
                if r.failures > 0 {
                    std::process::exit(1);
                }
                cx.quit();
                return true;
            }
        }
        false
    }

    /// One frame of waiting for a pending `shot`: the rasterizer either has
    /// written the frame the step drew, or it has not yet, or it has written
    /// a blank one and the shaders it draws with are still compiling.
    fn poll_shot(&mut self, sh: &mut Shell, r: &mut Runner) {
        let Some(shot) = self.shot.as_mut() else {
            return;
        };
        shot.waited += 1;
        let out_of_patience = shot.waited >= SHOT_PATIENCE;
        let state = boot::frame_after(shot.mark);
        match state {
            Frame::Pending if !out_of_patience => return,
            // Blank: the frame was written and the shaders it should have
            // been drawn with are not loaded yet. Wait on *this* one, so the
            // next blank does not answer for it, and let the next draw try
            // again.
            Frame::Blank(n) if !out_of_patience => {
                shot.mark = Some(n);
                return;
            }
            // The patience ran out. Say so: the picture about to be written
            // is the best there is, and it is not what was asked for.
            Frame::Pending | Frame::Blank(_) => eprintln!(
                "{}e2e: shot {}: no {} frame after {} — taking what there is",
                r.tag,
                shot.name,
                if matches!(state, Frame::Blank(_)) {
                    "drawn"
                } else {
                    "new"
                },
                shot.waited
            ),
            Frame::Ready(_) | Frame::Uncounted => {}
        }
        self.take_shot(sh, r);
    }

    /// Copies the frame and reports it: the waiting, if there was any, is
    /// over. The copy is the [`Shot`] effect, as it always was — a world
    /// that may not photograph anything still refuses out loud.
    fn take_shot(&mut self, sh: &mut Shell, r: &mut Runner) {
        let Some(PendingShot {
            name,
            path,
            mark,
            waited,
        }) = self.shot.take()
        else {
            return;
        };
        match sh.session.world().run(&Shot(&path)) {
            Ok(()) if mark.is_some() => {
                eprintln!("e2e: shot {} (after {waited} frame(s))", path.display());
            }
            Ok(()) => eprintln!("e2e: shot {}", path.display()),
            Err(e) => {
                eprintln!("{}e2e: FAIL shot {name}: {e}", r.tag);
                r.failures += 1;
            }
        }
    }

    /// A label that resolved to nothing, and what was on offer instead.
    fn no_such(&self, r: &mut Runner, verb: &str, label: &str) {
        eprintln!(
            "{}e2e: FAIL {verb} {label:?}: no matching element — on offer: {}",
            r.tag,
            self.hits.labels().join(" · ")
        );
        r.failures += 1;
    }
}
