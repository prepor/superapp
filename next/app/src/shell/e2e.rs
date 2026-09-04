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

use kernel::caps::Shot;
use kernel::e2e::{Runner, Step};
use makepad_widgets::*;

use super::keys::ChordExec;
use super::stage::{Shell, Stage};

/// The windowed runner is paced by a real timer at this interval; the
/// runner counts the same milliseconds either way.
pub const E2E_TICK_MS: f64 = 30.0;

impl Stage {
    /// Executes at most one step per tick; waits pace the script.
    pub(super) fn e2e_tick(&mut self, cx: &mut Cx, sh: &mut Shell, dt_ms: f64) {
        let Some(mut runner) = self.e2e.take() else {
            return;
        };
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
                match sh.session.world().run(&Shot(&path)) {
                    Ok(()) => eprintln!("e2e: shot {}", path.display()),
                    Err(e) => {
                        eprintln!("{}e2e: FAIL shot {name}: {e}", r.tag);
                        r.failures += 1;
                    }
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

            // The same path; kept as its own word because a suite written
            // against the shipping tree says `mouse` when it means "prove
            // the focus side effects too".
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
                    for run in runs {
                        run.selection_select_all();
                    }
                    cx.redraw_all();
                }
                None => self.no_such(r, "selectall", &label),
            },

            // Touch is out of the prototype: the interfaces do not preclude
            // it, and a suite that asks for it says so out loud rather than
            // failing on a label that was never drawn.
            Step::Swipe { .. } | Step::Pan2 { .. } | Step::HoldMove { .. } | Step::Drop => {
                eprintln!("e2e: {step:?} (not in the prototype)");
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
