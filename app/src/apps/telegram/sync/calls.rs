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

use kernel::effect::World;
use serde_json::Value;

use super::super::calls::{self, Link, Told};
use super::super::runtime::{self, Call, CallState, CallWish};
use super::super::{requests, updates};
use super::{Account, Td};

impl<T: Td> Account<T> {
    /// One `updateCall`: where the wire says the call now stands.
    pub(super) fn on_call(&self, w: &World, update: &Value) {
        let Some(wire) = updates::call(update) else { return };
        let rt = runtime::of(w.store());
        let known = rt.call(wire.user);
        let fresh = known.as_ref().is_none_or(|c| c.id != wire.id);
        let mut call = match known.filter(|c| c.id == wire.id) {
            Some(call) => call,
            None => Call::new(wire.id, wire.user, wire.outgoing, wire.video),
        };
        call.unique_id = wire.unique_id;
        call.video = wire.video;
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
            }
            updates::CallWire::ExchangingKeys => call.state = CallState::ExchangingKeys,
            // Everything the engine needs, in one update. From here the wire
            // only relays packets until somebody hangs up.
            updates::CallWire::Ready { ready, emoji } => {
                call.emoji = emoji;
                call.state = CallState::Connecting;
                call.camera = ready.video;
                self.engine.start(*ready);
            }
            updates::CallWire::HangingUp => call.state = CallState::HangingUp,
            updates::CallWire::Discarded { reason, need_rating } => {
                call.state = CallState::Ended;
                call.reason = reason;
                call.need_rating = need_rating;
                call.ended_at.get_or_insert(w.now());
                self.engine.stop(call.user);
                calls::forget_frames();
            }
            updates::CallWire::Error(error) => {
                call.state = CallState::Failed;
                call.error = Some(error);
                call.ended_at.get_or_insert(w.now());
                self.engine.stop(call.user);
                calls::forget_frames();
            }
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
                CallWish::Camera(on) => self.engine.camera(user, on),
            }
        }
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
