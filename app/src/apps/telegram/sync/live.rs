//! The live shares this account keeps moving.
//!
//! A live location is a message that goes on being edited for as long as its
//! period runs. Nothing on the wire moves it: the client that sent it reads
//! the device every second and edits the message when the fix has changed
//! enough to be worth a request. So a share is the worker's, and while one
//! runs the worker holds the receiver on.
//!
//! Nothing here is written down. A share lives in memory and is restored on
//! sign-in from `updateActiveLiveLocationMessages`, which is the wire's own
//! list of my running shares — so a restart re-registers them without a
//! table of ours to fall out of step with the server's answer.

use kernel::caps::{Fix, Location};

use super::*;

/// How far the device must have moved before an edit is worth sending, in
/// metres. The phone's number: under it the pin would jitter on GPS noise.
const MOVED_M: f64 = 1.0;

/// And how old the last edit must be. Ten seconds, the clients' floor, so a
/// walk does not become a request a second.
const EDIT_GAP: f64 = 10.0;

/// One share: the line it is, when it ends, and what was last sent for it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Share {
    pub chat: PeerId,
    pub message: MsgId,
    /// When the sharing ends, in unix seconds.
    pub until: f64,
    /// The fix the pin last stood at, and when it was put there. Seeded
    /// from the message itself — a send's echo carries the fix that went
    /// with it — so the first pass has something to measure the phone's
    /// rule against and does not edit a share to say what it already says.
    sent: Option<(Fix, f64)>,
}

/// What the account keeps: the shares, and whether the receiver is on for
/// them.
#[derive(Default)]
pub(super) struct Shares {
    live: Vec<Share>,
    /// Whether this worker has asked for the receiver. The capability counts
    /// its holders, so the worker asks once for the first share and lets go
    /// once for the last — never once per share, and never again because the
    /// first ask was refused.
    holding: bool,
}

impl Shares {
    fn of(&self, chat: PeerId) -> Option<&Share> {
        self.live.iter().find(|s| s.chat == chat)
    }

    /// What the panels read.
    fn published(&self) -> Vec<runtime::LiveShare> {
        self.live
            .iter()
            .map(|s| runtime::LiveShare {
                chat: s.chat,
                message: s.message,
                until: s.until,
            })
            .collect()
    }
}

impl<T: Td> Account<T> {
    /// A share this account is keeping moving, learned from one of my own
    /// messages — the echo of a live send, or one of the lines
    /// `updateActiveLiveLocationMessages` names on sign-in.
    ///
    /// A message still on its way carries a temporary id no edit can name,
    /// so a share is learned when the send has succeeded and not before. One
    /// whose period has already run out is a line in a history page and not
    /// a share: registering it would warm the receiver for the one pass it
    /// took to drop it again.
    pub(super) fn note_live_share(&self, w: &World, message: &Value) {
        if message["sending_state"]["@type"].as_str() == Some("messageSendingStatePending") {
            return;
        }
        let Some((chat, id, until)) = updates::live_share(message).filter(|s| s.2 > w.now()) else {
            return;
        };
        let mut shares = self.live_shares.borrow_mut();
        if shares.live.iter().any(|s| s.chat == chat && s.message == id) {
            return;
        }
        // One share a chat, as the clients have it: starting a second one
        // replaces the first, whose message the server has already ended.
        shares.live.retain(|s| s.chat != chat);
        shares.live.push(Share {
            chat,
            message: id,
            until,
            sent: updates::live_pin(message),
        });
        self.hold_receiver(w, &mut shares);
        runtime::of(w.store()).set_live_shares(shares.published());
    }

    /// A line that is no longer the share it was: the temporary id a send
    /// wore before Telegram gave it its own.
    pub(super) fn forget_live_share(&self, w: &World, chat: PeerId, message: MsgId) {
        let mut shares = self.live_shares.borrow_mut();
        let before = shares.live.len();
        shares.live.retain(|s| !(s.chat == chat && s.message == message));
        if shares.live.len() != before {
            self.hold_receiver(w, &mut shares);
            runtime::of(w.store()).set_live_shares(shares.published());
        }
    }

    /// The whole list, as the wire has it. TDLib pushes this on sign-in and
    /// whenever the set changes, so it — and not a table of ours — is what a
    /// restart restores from: a share ended from another device is simply
    /// not in it.
    pub(super) fn on_active_live(&self, w: &World, update: &Value) {
        let Some(messages) = update["messages"].as_array() else {
            return;
        };
        let now = w.now();
        let mut shares = self.live_shares.borrow_mut();
        let known = std::mem::take(&mut shares.live);
        for message in messages {
            let Some((chat, id, until)) = updates::live_share(message).filter(|s| s.2 > now) else {
                continue;
            };
            shares.live.push(Share {
                chat,
                message: id,
                until,
                // What this run has already sent for a share it already
                // knew; for one it is meeting for the first time — a
                // restart's — where the wire says the pin last stood.
                sent: known
                    .iter()
                    .find(|s| s.chat == chat && s.message == id)
                    .and_then(|s| s.sent)
                    .or_else(|| updates::live_pin(message)),
            });
        }
        self.hold_receiver(w, &mut shares);
        runtime::of(w.store()).set_live_shares(shares.published());
    }

    /// Whether a share is running, which is what tells the pass not to sleep
    /// long.
    pub(super) fn sharing_live(&self) -> bool {
        !self.live_shares.borrow().live.is_empty()
    }

    /// Forgets every share and lets the receiver go — what a sign-in and a
    /// shutdown each do. What is still running comes back with the wire's
    /// own list a moment later; what this run merely remembered does not.
    pub(super) fn drop_live_shares(&self, w: &World) {
        let mut shares = self.live_shares.borrow_mut();
        shares.live.clear();
        self.hold_receiver(w, &mut shares);
        drop(shares);
        runtime::of(w.store()).set_live_shares(Vec::new());
    }

    /// One pass over the shares: the stops the panels asked for, the ones
    /// whose period has run out, and an edit for each that has moved far
    /// enough and waited long enough.
    ///
    /// A share past its end is dropped and nothing is sent, as the phone
    /// does — the server ends it on its own clock, and a client that sent a
    /// last edit would be editing a message that is no longer live.
    pub(super) fn tick_live(&self, w: &World) {
        let stops = runtime::of(w.store()).take_live_stops();
        if !self.sharing_live() && stops.is_empty() {
            return;
        }
        let now = w.now();
        let mut requests: Vec<String> = Vec::new();
        let mut shares = self.live_shares.borrow_mut();
        // A stop is an edit with the location gone, never a delete: the
        // line stays in the chat as the place it last was.
        for chat in stops {
            if let Some(share) = shares.of(chat) {
                requests.push(edit_live_location(chat, share.message, None));
            }
            shares.live.retain(|s| s.chat != chat);
        }
        shares.live.retain(|s| s.until > now);
        let fix = (!shares.live.is_empty())
            .then(|| w.with_cap::<dyn Location, _>(|l| l.fix()).ok().flatten())
            .flatten();
        if let Some(fix) = fix {
            for share in &mut shares.live {
                if !worth_sending(share.sent.as_ref(), &fix, now) {
                    continue;
                }
                requests.push(edit_live_location(share.chat, share.message, Some(&fix)));
                share.sent = Some((fix, now));
            }
        }
        self.hold_receiver(w, &mut shares);
        let published = shares.published();
        drop(shares);
        runtime::of(w.store()).set_live_shares(published);
        for request in requests {
            self.send(w, &request);
        }
    }

    /// Holds the receiver on while there is a share to move, and lets it go
    /// when there is not.
    ///
    /// A refusal — a permission the person said no to — is not said here and
    /// is not asked about twice: the place panel is where a person asked for
    /// the receiver and is where a refusal belongs, and a worker that cannot
    /// read the device simply sends no edit. So the ask counts as the hold
    /// whether or not it was granted, and the release that follows is the
    /// one this worker owes.
    fn hold_receiver(&self, w: &World, shares: &mut Shares) {
        let wanted = !shares.live.is_empty();
        if wanted == shares.holding {
            return;
        }
        if wanted {
            let _ = w.with_cap::<dyn Location, _>(kernel::caps::Location::want);
        } else {
            let _ = w.with_cap::<dyn Location, _>(kernel::caps::Location::release);
        }
        shares.holding = wanted;
    }
}

/// The phone's rule: an edit goes out when the device has moved more than a
/// metre *away from where the pin stands* and the pin was put there at least
/// ten seconds ago.
///
/// Where the pin stands is known from the message before any edit has gone
/// out — the send carried a fix, and the wire echoed it back ([`Share::sent`])
/// — so the first pass after a share is learned repeats nothing. A share
/// whose message said nothing about where it is has no baseline at all, and
/// the first fix is worth sending.
fn worth_sending(sent: Option<&(Fix, f64)>, fix: &Fix, now: f64) -> bool {
    match sent {
        None => true,
        Some((was, at)) => fix.metres_from(was) > MOVED_M && now - at >= EDIT_GAP,
    }
}
