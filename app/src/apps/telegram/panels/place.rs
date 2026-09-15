//! A place to send: where the device says I am, on the map, with the two
//! ways to share it — once, or live for a while.
//!
//! Opened from the attach panel's bar and joined to it. The receiver is the
//! panel's for as long as the panel is open: it is asked for when the panel
//! opens and let go when it closes, so a chip nobody is reading is not left
//! warm. A share that goes on moving outlives the panel, and that one is the
//! worker's — the panel only starts it and stops it.

use std::any::Any;
use std::rc::Rc;

use kernel::caps::{Fix, Location};
use kernel::effect::World;
use kernel::layout::SlotId;
use kernel::panel::{Opening, Panel, PanelId, PanelKind, Tag, Verb};
use kernel::session::Session;
use kernel::store::Store;

use super::super::draft_toast;
use super::super::model::{self, live_left, PeerId};
use super::super::requests::{self, LIVE_PERIODS};
use super::super::runtime::{self, LiveShare};
use super::told;

/// The place panel.
pub struct Place {
    id: PanelId,
    chat: PeerId,
    world: Rc<World>,
    slot: SlotId,
    /// Which of the four periods the `live` verb offers. `period` walks it.
    period: usize,
    /// Why the receiver refused, where it did — a permission the person said
    /// no to, or a build with no receiver in it. Said in the panel rather
    /// than waited through.
    refused: Option<String>,
}

impl Place {
    pub const TAG: Tag = Tag("place");

    /// The identity of the place to send to one chat.
    #[must_use]
    pub fn id(chat: PeerId) -> PanelId {
        PanelId::new(Self::TAG, [chat.to_string()])
    }

    /// The same, in one of a forum's topics, which is what the place is
    /// sent into.
    #[must_use]
    pub fn in_topic(chat: PeerId, topic: i64) -> PanelId {
        if topic == 0 { return Self::id(chat); }
        PanelId::new(Self::TAG, [chat.to_string(), topic.to_string()])
    }

    fn topic(&self) -> i64 {
        self.id.arg(1).and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    fn store(&self) -> &Rc<Store> {
        self.world.store()
    }

    /// The chat a `place` panel is for; `None` for any other tag.
    #[must_use]
    pub fn of(id: &PanelId) -> Option<PeerId> {
        (id.tag == Self::TAG)
            .then(|| id.arg(0)?.parse().ok())
            .flatten()
    }

    /// Where I am, as the receiver last said. `None` while it is warming up,
    /// and for good where it refused.
    #[must_use]
    pub fn fix(&self) -> Option<Fix> {
        self.world
            .with_cap::<dyn Location, _>(|l| l.fix())
            .ok()
            .flatten()
    }

    /// Why there will be no fix, where that is known: the refusal the panel
    /// opened on, or whatever the receiver is complaining about now.
    ///
    /// Read on every draw rather than once, because the two are different
    /// moments: `want` answers in the frame it is called in, and the
    /// platform's own dialog is answered a second or a minute later. A panel
    /// that read the refusal only at its opening said *finding you…* for as
    /// long as it stood, however plainly the person had said no.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        self.refused.clone().or_else(|| {
            self.world
                .with_cap::<dyn Location, _>(|l: &mut (dyn Location + 'static)| l.trouble())
                .ok()
                .flatten()
        })
    }

    /// The line under the map: the point and how far off the reading may be,
    /// or what the panel is waiting for. Empty where the receiver refused —
    /// there is nothing to wait for then, and the refusal is its own line.
    #[must_use]
    pub fn where_line(&self) -> String {
        if self.refusal().is_some() {
            return String::new();
        }
        match self.fix() {
            None => "finding you…".to_string(),
            Some(fix) => format!(
                "{:.4}, {:.4} · ±{} m",
                fix.lat,
                fix.lon,
                fix.accuracy_m.round().max(0.0) as i64
            ),
        }
    }

    /// This chat's running share, if it has one.
    #[must_use]
    pub fn sharing(&self) -> Option<LiveShare> {
        runtime::of(self.store()).live_share(self.chat, self.world.now())
    }

    /// What the panel says under the line while a share of this chat runs.
    #[must_use]
    pub fn share_line(&self) -> Option<String> {
        self.sharing()
            .map(|s| format!("sharing live · {}", live_left(s.until, self.world.now())))
    }

    /// The chat's title.
    #[must_use]
    pub fn chat_title(&self) -> String {
        super::super::topics::card(self.store(), self.chat, self.topic()).map_or_else(|| "chat".to_string(), |c| c.name)
    }
}

impl Panel for Place {
    fn id(&self) -> &PanelId {
        &self.id
    }

    fn title(&self) -> String {
        format!("place · {}", self.chat_title())
    }

    /// A card with a map.
    fn wish(&self, _cols: usize) -> (u32, u32) {
        (4, 3)
    }

    fn placed(&mut self, slot: SlotId) {
        self.slot = slot;
    }

    /// Once, or live for one of the four periods — and, while a share of
    /// this chat runs, the way to end it.
    fn verbs(&self) -> Vec<Verb> {
        let (_, label) = LIVE_PERIODS[self.period];
        let mut verbs = vec![
            Verb::run("telegram.send_place", "send", Some('s')),
            Verb::run("telegram.send_live", format!("live {label}"), Some('v')),
            Verb::run("telegram.live_period", "period", Some('e')),
        ];
        if self.sharing().is_some() {
            verbs.push(Verb::run("telegram.stop_live", "stop live", Some('o')));
        }
        verbs
    }

    fn run(&mut self, verb: &str, s: &mut Session) {
        // The one verb that asks nothing of the device: it walks the label
        // the other one wears.
        if verb == "telegram.live_period" {
            self.period = (self.period + 1) % LIVE_PERIODS.len();
            s.redraw();
            return;
        }
        if verb == "telegram.stop_live" {
            if self.sharing().is_none() {
                return;
            }
            if super::live(self.store()) {
                // The share is the worker's; stopping it is a wish it takes
                // on its next pass and edits the location away.
                runtime::of(self.store()).stop_live(self.chat);
            } else {
                runtime::of(self.store()).forget_draft_share(self.chat);
                s.notify(draft_toast("stop sharing live location"), false);
            }
            s.redraw();
            return;
        }
        if model::peer(self.store(), self.chat).is_some_and(|c| c.blocked) {
            s.notify("unblock this user before sending a message", false);
            return;
        }
        let Some(fix) = self.fix() else {
            s.notify(
                self.refusal()
                    .unwrap_or_else(|| "still finding you — try again in a moment".to_string()),
                true,
            );
            return;
        };
        match verb {
            // The one-off share, over the wire where the build is signed in:
            // the line comes back as its own echo, the way every send's does,
            // so there is nothing local to write. The place answers no
            // message — this panel is opened from the attach panel, which the
            // composer's reply line does not reach.
            "telegram.send_place" => {
                told(
                    s,
                    &requests::in_topic(requests::send_location(self.chat, None, &fix), self.topic()),
                    &format!("place {:.4}, {:.4}", fix.lat, fix.lon),
                );
            }
            // A live share is the worker's from here on: it learns the
            // message from the send's own echo and keeps it moving. Where
            // nothing leaves — a scene, a suite, the demo world — the panel
            // notes the share itself, so the status line and `stop live` are
            // walkable there too.
            "telegram.send_live" => {
                let (period, label) = LIVE_PERIODS[self.period];
                // Into the topic the panel was opened from, as the one-off
                // share and every other send of this chat's is.
                let request = requests::in_topic(
                    requests::send_live_location(self.chat, &fix, period),
                    self.topic(),
                );
                if super::live(self.store()) {
                    told(s, &request, &format!("live location for {label}"));
                } else {
                    runtime::of(self.store()).note_draft_share(LiveShare {
                        chat: self.chat,
                        message: 0,
                        until: self.world.now() + period as f64,
                    });
                    s.notify(
                        draft_toast(&format!(
                            "live location {:.4}, {:.4} for {label}",
                            fix.lat, fix.lon
                        )),
                        false,
                    );
                    s.redraw();
                }
            }
            _ => {}
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// The receiver is held for as long as the panel stands. Closing it is what
/// lets go — the capability counts its holders, so a worker moving a share
/// keeps the chip warm after the panel has gone, and a panel that was
/// refused releases nothing, having taken nothing.
impl Drop for Place {
    fn drop(&mut self) {
        if self.refused.is_some() {
            return;
        }
        let _ = self
            .world
            .with_cap::<dyn Location, _>(kernel::caps::Location::release);
    }
}

/// Its factory.
pub struct PlaceKind;

impl PanelKind for PlaceKind {
    fn tag(&self) -> Tag {
        Place::TAG
    }

    fn open(&self, id: &PanelId, cx: &mut Opening<'_>) -> Box<dyn Panel> {
        let world = cx.session().world().clone();
        // Asking for the receiver is the first thing the panel does; the fix
        // lands a second or two later and the map is drawn then. A refusal is
        // kept and said, never retried on its own.
        let refused = match world.with_cap::<dyn Location, _>(kernel::caps::Location::want) {
            Ok(Ok(())) => None,
            Ok(Err(why)) => Some(why),
            Err(_) => Some("this build has no receiver".to_string()),
        };
        Box::new(Place {
            chat: Place::of(id).unwrap_or_default(),
            id: id.clone(),
            world,
            slot: 0,
            // An hour, the clients' own default.
            period: 1,
            refused,
        })
    }
}
