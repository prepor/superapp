//! The worker's half of a call: TDLib's signalling on one side, the engine
//! on the other, and the runtime's row between them.
//!
//! TDLib rings, hands over the key, the servers and the four emoji, and
//! relays whatever the engine wants said to the other client. The engine
//! carries the voice and the picture and says where its connection stands.
//! Neither knows about the other; this module is the joint, and the row it
//! writes is what the call panel draws.
//!
//! Nothing here is persisted. A call is a thing that is happening, and what
//! happened is the line the wire writes into the chat when it ends.
//!
//! Two things beside the media hang off the same two moments — the wire
//! saying *ready* and the wire saying *over*. The camera, where the engine
//! has no camera of its own to open ([`Account::hold_camera`]); and the
//! phone's audio route, which is the app's to set and nothing the engine
//! knows about.
//!
//! One thing holds *ready* back: the microphone's permission
//! ([`Account::start_waiting`]). The engine's capture opens the device
//! itself, so a call started while the platform's dialog is still standing
//! open carries silence for the whole of its length; the wire's *ready*
//! waits on the row until the person answers, or until [`READY_WAIT`].

use kernel::caps::Capture;
use kernel::effect::World;
use serde_json::Value;

use crate::platform::audio_route;

use super::super::calls::{self, Link, Ready, Told};
use super::super::runtime::{self, Call, CallState, CallWish};
use super::super::{requests, updates};
use super::{Account, Td};

/// The longest a call waits for the microphone's permission before it
/// starts anyway, in seconds.
///
/// Shorter than the wire's own ring timeout, which is a minute and more, so
/// a call held here is still a call that can be answered; long enough for a
/// dialog nobody is in a hurry to read. A call that starts without the
/// permission is a call with no voice going out, which is worse than a
/// second of quiet at the beginning — and better than a call that never
/// starts because the person walked away from the glass.
pub(super) const READY_WAIT: f64 = 20.0;

/// What the panel says of a call carried without a microphone. The
/// platform's own refusal names where to go and is the recording's to say;
/// this is the call's, whose device is held by a library that answers
/// nobody here.
const NO_MICROPHONE: &str = "the microphone is not allowed";

impl<T: Td> Account<T> {
    /// One `updateCall`: where the wire says the call now stands.
    pub(super) fn on_call(&self, w: &World, update: &Value) {
        let Some(wire) = updates::call(update) else { return };
        let rt = runtime::of(w.store());
        let known = rt.call(wire.user);
        // A call ended before the wire had named it. The card writes the row
        // with no id, so the panel has *contacting…* to draw on the draw it
        // opens on, and *end* pressed in that moment has nothing to discard
        // by name — it leaves the row *hanging up* instead, and this is
        // where that is made good.
        let ending = known
            .as_ref()
            .is_some_and(|c| c.id == 0 && c.state == CallState::HangingUp);
        let fresh = known.as_ref().is_none_or(|c| c.id != wire.id);
        let mut call = match known.filter(|c| c.id == wire.id) {
            Some(call) => call,
            None => Call::new(wire.id, wire.user, wire.outgoing, wire.video),
        };
        call.video = wire.video;
        // A row on its way out is not stood back up. The wire repeats
        // itself, and a `callStateReady` — or any state that is not an
        // ending — arriving for a call this end has already hung up would
        // start the engine again and reopen the camera on a call nobody is
        // in. Only the wire's own endings may move it from here.
        let ending_state = matches!(
            wire.state,
            updates::CallWire::Discarded { .. } | updates::CallWire::Error(_)
        );
        if call.state.over() && !ending_state {
            return;
        }
        match wire.state {
            // Ringing at this end, and nothing has been answered. A call that
            // arrives while nobody is looking at Telegram is what opens the
            // panel — the one place the worker asks for a panel at all.
            updates::CallWire::Pending { created, received } => {
                call.state = if !wire.outgoing {
                    CallState::Incoming
                } else if !created {
                    CallState::Contacting
                } else if received {
                    CallState::Ringing
                } else {
                    CallState::Waiting
                };
                // The permissions, as early as there is a call to want them
                // for — *contacting…* for a call going out, *incoming* for
                // one coming in — so the person has answered by the time
                // `callStateReady` arrives. The microphone is asked for and
                // not opened: the engine captures through its own library,
                // where this capability cannot see it, so nothing else in
                // the app would ever raise the platform's dialog and the
                // first call on a phone would carry silence.
                if fresh {
                    self.ask_microphone(w);
                    if call.video {
                        self.hold_camera(w, true);
                    }
                }
            }
            updates::CallWire::ExchangingKeys => call.state = CallState::ExchangingKeys,
            // Everything the engine needs, in one update. From here the wire
            // only relays packets until somebody hangs up.
            updates::CallWire::Ready { ready, emoji } => {
                call.emoji = emoji;
                call.state = CallState::Connecting;
                call.camera = ready.video;
                // Unless the microphone's permission is still unanswered.
                // The engine's capture opens the device itself, under
                // whatever dialog is standing at the time, and a session
                // opened then hands over silence for the whole of the call
                // — so the wire's *ready* waits on the row and the pass
                // starts it when the answer comes.
                let answer = self.microphone_allowed(w);
                if answer.is_none() {
                    call.pending_ready = Some((ready, w.now()));
                } else {
                    self.begin(w, &mut call, *ready, answer);
                }
            }
            updates::CallWire::HangingUp => call.state = CallState::HangingUp,
            updates::CallWire::Discarded { reason, need_rating } => {
                call.state = CallState::Ended;
                call.reason = reason;
                call.need_rating = need_rating;
                call.ended_at.get_or_insert(w.now());
                self.over(w, call.user);
            }
            updates::CallWire::Error(error) => {
                call.state = CallState::Failed;
                call.error = Some(error);
                call.ended_at.get_or_insert(w.now());
                self.over(w, call.user);
            }
        }
        if ending && !call.state.over() {
            self.send(w, &requests::discard_call(call.id, false, 0, call.video));
            // The row this discards is not in the runtime under the wire's
            // id yet — it is written a few lines down — so the send's own
            // teardown cannot find it, and it is done by hand here.
            self.over(w, call.user);
            call.state = CallState::HangingUp;
        }
        let show = fresh && !wire.outgoing && !call.state.over();
        rt.put_call(call);
        if show {
            rt.show_call(wire.user);
        }
        rt.operations.changed();
    }

    /// One `updateNewCallSignalingData`: a packet the other client sent,
    /// straight through to the engine.
    pub(super) fn on_call_signalling(&self, w: &World, update: &Value) {
        let Some((id, data)) = updates::call_signalling(update) else { return };
        let Some(call) = runtime::of(w.store()).calls().into_iter().find(|c| c.id == id) else {
            return;
        };
        self.engine.signalling(call.user, data);
    }

    /// Once a pass: what the engine has said since the last one, what the
    /// panels have asked for, and the clock — which is the only thing the
    /// fake engine has to connect by.
    pub(super) fn pump_calls(&self, w: &World) {
        let rt = runtime::of(w.store());
        // A *ready* held back by an unanswered microphone, before the tick:
        // a call started in this pass is a call the fake engine begins
        // counting its two seconds from in this pass.
        self.start_waiting(w);
        // The clock first, so whatever the time makes the engine say is
        // drained in the same pass rather than waiting for the next one.
        self.engine.tick(w.now());
        let mut said = Vec::new();
        {
            let mut says = self.engine_says.borrow_mut();
            while let Ok(told) = says.try_recv() {
                said.push(told);
            }
        }
        for told in said {
            match told {
                // The engine wants the other client to hear something, and
                // the wire is the only way there.
                Told::Signalling { user, data } => {
                    if let Some(call) = rt.call(user).filter(|c| !c.state.over()) {
                        self.send(w, &requests::send_call_signaling_data(call.id, &data));
                    }
                }
                Told::Link { user, link } => self.on_link(w, user, &link),
            }
        }
        for (user, wish) in rt.take_call_wishes() {
            match wish {
                CallWish::Mute(on) => self.engine.mute(user, on),
                CallWish::Camera(on) => {
                    self.hold_camera(w, on);
                    self.engine.camera(user, on);
                }
            }
        }
    }

    /// A `discardCall` on its way out.
    ///
    /// Every way of ending a call sends this one request — the bar's *end*
    /// and its *decline*, and the worker's own discard of a call whose media
    /// died — and the wire answers with the terminal `updateCall` in its own
    /// time, which on a poor connection is seconds. Waiting for that answer
    /// to stop the engine meant a person who had hung up went on being heard
    /// and, in a video call, seen. So the media ends here, with the request.
    ///
    /// The row is left exactly as it is: it says *hanging up* until the wire
    /// says the call is over, because which of the five reasons it was is the
    /// wire's to say and not this end's to guess.
    pub(super) fn discarding(&self, w: &World, request: &Value) {
        if request["@type"] != "discardCall" {
            return;
        }
        let Some(id) = request["call_id"].as_i64() else { return };
        let Some(call) = runtime::of(w.store())
            .calls()
            .into_iter()
            .find(|c| i64::from(c.id) == id)
        else {
            return;
        };
        self.over(w, call.user);
    }

    /// Everything the end of a call puts back, however it ended: the engine
    /// let go, the two pictures forgotten, the camera closed and the phone's
    /// route the way it was found.
    ///
    /// Called twice for one call — once as the discard goes out, once when
    /// the wire's terminal update lands — and both times is an answer rather
    /// than a failure: an engine that has already let a call go is told to
    /// let it go again, and the camera is given back only by whoever is
    /// holding it ([`hold_camera`](Account::hold_camera)).
    pub(super) fn over(&self, w: &World, user: i64) {
        self.engine.stop(user);
        self.hold_camera(w, false);
        audio_route::in_call(false);
        calls::forget_frames();
    }

    /// Starts the media of a call the wire has made ready: the phone's
    /// route, the camera where the engine has none of its own, and then the
    /// engine — in that order, because android decides where a stream goes
    /// as the stream opens, and a camera asked for afterwards is a first
    /// second with no picture in it.
    ///
    /// `allowed` is what the platform said about the microphone: `Some(false)`
    /// is a call that goes ahead and carries no voice out, and the panel
    /// says so; `None` is the wait having run out with nobody answering,
    /// which may yet turn into a yes and so says nothing.
    fn begin(&self, w: &World, call: &mut Call, ready: Ready, allowed: Option<bool>) {
        call.pending_ready = None;
        if allowed == Some(false) {
            call.error = Some(NO_MICROPHONE.to_string());
        }
        audio_route::in_call(true);
        self.hold_camera(w, call.camera);
        self.engine.start(ready);
    }

    /// The calls whose *ready* is waiting on the microphone's permission,
    /// once a pass. The answer starts them, either way; so does the wait
    /// running out, because a person who walked away from the dialog should
    /// come back to a call rather than to a row that says *connecting* for
    /// ever.
    fn start_waiting(&self, w: &World) {
        let rt = runtime::of(w.store());
        let now = w.now();
        for mut call in rt.calls() {
            let Some((ready, since)) = call.pending_ready.take() else { continue };
            // Hung up while the dialog was open: there is nothing left to
            // start, and the row keeps whatever ending it is going to get.
            if !call.state.over() {
                let answer = self.microphone_allowed(w);
                if answer.is_none() && now - since < READY_WAIT {
                    continue;
                }
                self.begin(w, &mut call, *ready, answer);
            }
            rt.put_call(call);
            rt.operations.changed();
        }
    }

    /// What the platform says about the microphone's permission. A world
    /// with no capture at all — a library mount, a world built without the
    /// senses — answers *allowed*: there is no dialog there to wait for.
    fn microphone_allowed(&self, w: &World) -> Option<bool> {
        w.with_cap::<dyn Capture, _>(|c: &mut (dyn Capture + 'static)| c.microphone_allowed())
            .unwrap_or(Some(true))
    }

    /// The microphone's permission, asked for and nothing opened.
    ///
    /// A no-op wherever the capability has no dialog to raise, which is
    /// every scripted run; on a device it is the difference between a call
    /// that carries a voice and one that carries silence.
    fn ask_microphone(&self, w: &World) {
        let _ = w.with_cap::<dyn Capture, _>(|c: &mut (dyn Capture + 'static)| {
            c.ask_microphone();
        });
    }

    /// The camera, while the engine has a picture to send and no camera of
    /// its own to make it with.
    ///
    /// On a Mac this does nothing at all: the library opens a capture session
    /// itself, so [`CallEngine::frames_wanted`](calls::CallEngine::frames_wanted)
    /// answers `None` and there is no tap to leave. On the phone the call
    /// sees through makepad's camera, and the panel's own preview is that
    /// same session drawn.
    ///
    /// One hold at a time, and the worker remembers whether it has it: the
    /// camera is asked for when the call appears and again when it is ready,
    /// and given back when the discard goes out and again when the wire says
    /// the call is over — and a hold taken twice or given back twice would
    /// leave the phone's camera on for the rest of the run.
    fn hold_camera(&self, w: &World, on: bool) {
        let Some(tap) = self.engine.frames_wanted() else { return };
        if self.camera_held.get() == on {
            return;
        }
        self.camera_held.set(on);
        let _ = w.with_cap::<dyn Capture, _>(move |c: &mut (dyn Capture + 'static)| {
            if on {
                // A camera that is refused is a call without a picture, not
                // a call that fails: the voice goes on either way.
                let _ = c.open_camera();
                c.watch_frames(Some(tap));
            } else {
                c.watch_frames(None);
                c.close_camera();
            }
        });
    }

    /// Where the media's connection stands. *Connecting* after it had once
    /// connected is *reconnecting* — the same word the reference clients use,
    /// and the reason the row remembers when it first connected rather than
    /// only that it did.
    fn on_link(&self, w: &World, user: i64, link: &Link) {
        let rt = runtime::of(w.store());
        let now = w.now();
        let mut lost = None;
        rt.change_call(user, |call| {
            if call.state.over() {
                return;
            }
            match link {
                Link::Connecting if call.connected_at.is_some() => {
                    call.state = CallState::Reconnecting;
                }
                Link::Connecting => call.state = CallState::Connecting,
                Link::Connected => {
                    call.state = CallState::Connected;
                    call.connected_at.get_or_insert(now);
                }
                // The media died. Every client discards the call itself here
                // rather than leaving it ringing on the other side.
                Link::Failed(error) => {
                    call.error = Some(error.clone());
                    lost = Some((call.id, call.secs(now), call.video));
                }
            }
        });
        if let Some((id, secs, video)) = lost {
            self.send(w, &requests::discard_call(id, true, secs, video));
        }
        rt.operations.changed();
    }
}
