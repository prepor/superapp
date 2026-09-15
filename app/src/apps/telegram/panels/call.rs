//! One call with one person: where it stands, the four emoji, the two
//! pictures, and the bar that follows the state.
//!
//! Opened by *voice call* or *video call* on a person's card, and by the
//! worker itself when somebody rings — the one panel in this app that a
//! background pass asks for. Everything it draws comes off the runtime's
//! call row, which the worker writes from `updateCall` and the engine's
//! answers; nothing about a call is in the store, because a call is a thing
//! that is happening and what happened is the line in the chat.

use std::any::Any;
use std::rc::Rc;

use kernel::caps::{CameraId, Capture};
use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::nav::Nav;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::calls::{self, sounds::Ring};
use super::super::model::{self, fmt_secs, PeerId};
use super::super::requests;
use super::super::runtime::{self, Call as Live, CallState, CallWish, Reason};
use super::told;

/// The call panel.
pub struct Call {
    id: PanelId,
    user: PeerId,
    store: Rc<Store>,
    /// The world the camera is reached through, for the one platform where
    /// the preview is makepad's own session rather than the engine's frames.
    world: Rc<World>,
    slot: SlotId,
    /// Whether a verb has silenced the ring. Any verb does — answering it,
    /// refusing it, or ending it — and a new call rings again.
    hushed: Option<i32>,
}

impl Call {
    pub const TAG: Tag = Tag("call");

    /// The identity of the call with one person. One panel a person: the wire
    /// will not ring twice at once.
    #[must_use]
    pub fn id(user: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [user.to_string()])
    }

    /// The person a `call` panel is with; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG).then(|| id.arg(0)?.parse().ok()).flatten()
    }

    /// The call as it stands, where there is one.
    #[must_use]
    pub fn call(&self) -> Option<Live> {
        runtime::of(&self.store).call(self.user)
    }

    /// Who it is with.
    #[must_use]
    pub fn name(&self) -> String {
        model::peer(&self.store, self.user).map_or_else(|| self.user.to_string(), |p| p.name)
    }

    /// Where it stands, in the reference clients' words. The timer while it
    /// is connected, which is why this takes the clock.
    #[must_use]
    pub fn line(&self, now: f64) -> String {
        let Some(call) = self.call() else {
            return "no call".to_string();
        };
        let video = if call.video { "incoming video call" } else { "incoming call" };
        match call.state {
            CallState::Contacting => "contacting…".to_string(),
            CallState::Waiting => "waiting".to_string(),
            CallState::Ringing => "ringing".to_string(),
            CallState::Incoming => video.to_string(),
            CallState::ExchangingKeys => "exchanging encryption keys".to_string(),
            CallState::Connecting => "connecting".to_string(),
            CallState::Reconnecting => "reconnecting".to_string(),
            CallState::Connected => fmt_secs(call.secs(now)),
            CallState::HangingUp | CallState::Ended => ended(&call, now),
            CallState::Failed => match call.error.as_deref() {
                Some(error) => format!("failed to connect · {error}"),
                None => "failed to connect".to_string(),
            },
        }
    }

    /// The four the wire computed from the key, once there are any. What
    /// makes two people sure of each other: they read them out and compare.
    #[must_use]
    pub fn emoji(&self) -> String {
        self.call().map(|c| c.emoji.join(" ")).unwrap_or_default()
    }

    /// Which sound the call should be making, and for which call — the panel
    /// says, and its widget plays. `None` once a verb has silenced a ring, or
    /// where there is nothing to say.
    #[must_use]
    pub fn ring(&self) -> Option<(Ring, i32)> {
        let call = self.call()?;
        let ring = match call.state {
            CallState::Incoming => Ring::Incoming,
            CallState::Contacting | CallState::Waiting | CallState::Ringing => Ring::Ringback,
            CallState::Ended if call.reason == Some(Reason::Declined) && call.outgoing => Ring::Busy,
            CallState::Ended | CallState::Failed => Ring::Ended,
            _ => return None,
        };
        // A verb stops the ringing; the short notes are what a call ending
        // sounds like and are not a thing to be silenced.
        if ring.repeats() && self.hushed == Some(call.id) {
            return None;
        }
        Some((ring, call.id))
    }

    /// Which camera my own picture comes out of, where the camera is ours to
    /// hold ([`calls::camera_is_ours`]). `None` everywhere else, and until
    /// the platform has said which camera it opened — the preview then draws
    /// nothing, as the attach panel's does while it waits.
    #[must_use]
    pub fn camera(&self) -> Option<CameraId> {
        if !calls::camera_is_ours() {
            return None;
        }
        let live = self.call().is_some_and(|c| c.camera && !c.state.over());
        if !live {
            return None;
        }
        self.world
            .with_cap::<dyn Capture, _>(|c: &mut (dyn Capture + 'static)| c.camera())
            .ok()
            .flatten()
    }

    /// Where the sounds are written, which is the store's own directory. A
    /// fixture has none, and so makes no sound at all.
    #[must_use]
    pub fn sounds_dir(&self) -> Option<&std::path::Path> {
        self.store.dir()
    }
}

/// What a call that is over says: how long it lasted, or the one word for
/// why it never did.
fn ended(call: &Live, now: f64) -> String {
    match call.reason {
        Some(Reason::Declined) if call.outgoing => "line busy".to_string(),
        Some(Reason::Declined) => "declined".to_string(),
        Some(Reason::Missed) => "missed".to_string(),
        Some(Reason::Disconnected) if call.connected_at.is_none() => "failed to connect".to_string(),
        _ => match call.connected_at {
            Some(_) => format!("call ended · {}", fmt_secs(call.secs(now))),
            None => "call ended".to_string(),
        },
    }
}

/// Rings somebody, and opens the panel on it. Used by the person's card,
/// which is the only way to start one.
pub(super) fn place(s: &mut Session, from: SlotId, user: PeerId, video: bool) {
    let rt = runtime::of(s.store());
    // The row first, so the panel has *contacting…* to draw on the very draw
    // it opens on. The wire's own `updateCall` replaces it a moment later
    // with the id every later request names the call by.
    rt.put_call(Live::new(0, user, true, video));
    let what = if video { "video call" } else { "call" };
    told(s, &requests::create_call(user, &calls::protocol(), video), what);
    s.nav(Nav::Open { from, id: Call::id(user), fresh: true });
}

impl Panel for Call {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        self.name()
    }

    /// A card with two pictures in it.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 3)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// The bar follows the state, as the reference clients' does: one way out
    /// while it rings, two while it is being offered, the three that matter
    /// while it runs, and the way off the panel once it is over.
    fn verbs(&self) -> Vec<Verb> {
        let Some(call) = self.call() else {
            return vec![Verb::run("telegram.call_close", "close", Some('c'))];
        };
        match call.state {
            CallState::Incoming => vec![
                Verb::run("telegram.call_accept", "accept", Some('a')),
                Verb::run("telegram.call_decline", "decline", Some('d')),
            ],
            CallState::Connecting | CallState::Connected | CallState::Reconnecting => {
                let mut v = vec![
                    Verb::run(
                        "telegram.call_mute",
                        if call.muted { "unmute" } else { "mute" },
                        Some('m'),
                    ),
                    Verb::run(
                        "telegram.call_camera",
                        if call.camera { "camera off" } else { "camera on" },
                        Some('c'),
                    ),
                ];
                // The route is the phone's alone: a Mac plays a call through
                // whatever the system is playing through, and has nothing to
                // choose between.
                if cfg!(target_os = "android") {
                    v.push(Verb::run(
                        "telegram.call_speaker",
                        if call.speaker { "speaker off" } else { "speaker on" },
                        Some('p'),
                    ));
                }
                v.push(Verb::run("telegram.call_end", "end", Some('e')));
                v
            }
            state if state.over() => {
                let mut v = vec![Verb::run("telegram.call_close", "close", Some('c'))];
                if call.need_rating {
                    v.push(Verb::run("telegram.call_rate", "rate", Some('r')));
                }
                v
            }
            _ => vec![Verb::run("telegram.call_end", "end", Some('e'))],
        }
    }

    /// Two of these reach Telegram — *accept* and the three ways of ending
    /// one — and two reach the engine, which only the worker can speak to, so
    /// they go through the runtime as everything else a panel wants does.
    fn run(&mut self, verb: &str, s: &mut Session) {
        let rt = runtime::of(&self.store);
        let now = s.now();
        let user = self.user;
        let Some(call) = self.call() else {
            if verb == "telegram.call_close" {
                s.nav(Nav::Close { slot: self.slot, label: Some(self.name()) });
            }
            return;
        };
        self.hushed = Some(call.id);
        match verb {
            "telegram.call_accept" => {
                if !super::can_call(&self.store) {
                    s.notify(calls::NO_ENGINE, true);
                    return;
                }
                told(s, &requests::accept_call(call.id, &calls::protocol()), "accept");
                rt.change_call(user, |c| c.state = CallState::ExchangingKeys);
            }
            "telegram.call_end" | "telegram.call_decline" => {
                let word = if verb == "telegram.call_decline" { "decline" } else { "end" };
                told(s, &requests::discard_call(call.id, false, call.secs(now), call.video), word);
                // On the wire the call goes *hanging up* and then *ended*, and
                // the wire says which of the five reasons it was. With no
                // wire there is nobody to say it, so it ends here.
                let live = super::live(&self.store);
                let declined = verb == "telegram.call_decline";
                rt.change_call(user, |c| {
                    if live {
                        c.state = CallState::HangingUp;
                    } else {
                        c.state = CallState::Ended;
                        c.reason = Some(if declined { Reason::Declined } else { Reason::HungUp });
                        c.ended_at = Some(now);
                    }
                });
            }
            "telegram.call_mute" => {
                let on = !call.muted;
                rt.change_call(user, |c| c.muted = on);
                rt.wish_call(user, CallWish::Mute(on));
            }
            "telegram.call_camera" => {
                let on = !call.camera;
                rt.change_call(user, |c| c.camera = on);
                rt.wish_call(user, CallWish::Camera(on));
            }
            // The one verb that reaches neither Telegram nor the engine: the
            // route is the phone's own, and the phone answers at once.
            "telegram.call_speaker" => {
                let on = !call.speaker;
                rt.change_call(user, |c| c.speaker = on);
                crate::platform::audio_route::speaker(on);
            }
            "telegram.call_rate" => {
                told(s, &requests::send_call_rating(call.id, 5), "rate");
                rt.change_call(user, |c| c.need_rating = false);
            }
            "telegram.call_close" => {
                calls::forget_frames();
                s.nav(Nav::Close { slot: self.slot, label: Some(self.name()) });
                return;
            }
            _ => {}
        }
        s.redraw();
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Its factory.
pub struct CallKind;

impl PanelKind for CallKind {
    fn tag(&self) -> Tag {
        Call::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        Box::new(Call {
            user: Call::of(id).unwrap_or_default(),
            id: id.clone(),
            store: cx.session().store().clone(),
            world: cx.session().world().clone(),
            slot: 0,
            hushed: None,
        })
    }
}
