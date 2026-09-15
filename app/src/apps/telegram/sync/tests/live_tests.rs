//! The live shares the worker keeps moving: the requests they are made of,
//! the rule an edit goes out by, and the receiver held while one runs.

use super::*;
use crate::apps::telegram::model;
use crate::apps::telegram::requests::{
    edit_live_location, send_live_location, LIVE_FOREVER, LIVE_PERIODS,
};
use kernel::caps::{FakeLocation, Fix};

const CHAT: i64 = -7001;
const LINE: i64 = 900;

fn v(s: String) -> serde_json::Value {
    serde_json::from_str(&s).unwrap()
}

/// The receiver the account and the panels share in this world.
fn device(w: &World) -> FakeLocation {
    w.with_cap::<FakeLocation, _>(|l| l.clone())
        .expect("the fake receiver")
}

/// One of my own live locations, as the wire sends it back.
fn live_line(chat: i64, id: i64, at: f64, period: i64) -> serde_json::Value {
    json!({
        "@type": "message", "id": id, "chat_id": chat, "date": at, "is_outgoing": true,
        "sender_id": {"@type": "messageSenderUser", "user_id": 2},
        "content": {
            "@type": "messageLiveLocation",
            "location": {
                "@type": "liveLocation",
                "location": {"latitude": 47.0472, "longitude": 8.3164, "horizontal_accuracy": 12.0},
                "live_period": period, "heading": 0, "proximity_alert_radius": 0
            },
            "expires_in": period
        }
    })
}

/// Starts a share on `acc` and answers the clock it started at.
fn share(acc: &Account<FakeTd>, w: &World, period: i64) -> f64 {
    let at = w.now();
    acc.on_update(
        w,
        &json!({"@type": "updateNewMessage", "message": live_line(CHAT, LINE, at, period)})
            .to_string(),
    );
    at
}

/// What the worker last asked Telegram to do about a share, if anything.
fn last_edit(td: &FakeTd) -> Option<serde_json::Value> {
    td.sent()
        .iter()
        .rev()
        .map(|raw| v(raw.clone()))
        .find(|v| v["@type"] == "editMessageLiveLocation")
}

/// The three requests a place is made of, against the installed TDLib's own
/// shapes: the point alone for a live send's `liveLocation`, the period and
/// the heading with it, and a stop that is an edit with the location gone.
#[test]
fn the_live_requests_carry_the_period_the_accuracy_and_the_heading() {
    let mut fix = Fix::at(47.0472, 8.3164, 100.0);
    fix.accuracy_m = 8.5;
    fix.heading_deg = Some(91.4);

    let req = v(send_live_location(CHAT, &fix, 3600));
    assert_eq!(req["@type"], "sendMessage");
    assert_eq!(req["chat_id"], CHAT);
    let content = &req["input_message_content"];
    assert_eq!(content["@type"], "inputMessageLiveLocation");
    assert_eq!(content["location"]["@type"], "liveLocation");
    assert_eq!(content["location"]["live_period"], 3600);
    assert_eq!(content["location"]["heading"], 91);
    assert_eq!(content["location"]["proximity_alert_radius"], 0);
    let point = &content["location"]["location"];
    assert_eq!(point["@type"], "location");
    assert_eq!(point["latitude"], 47.0472);
    assert_eq!(point["longitude"], 8.3164);
    assert_eq!(point["horizontal_accuracy"], 8.5);

    // An edit names the line and carries the new reading; its period is
    // nought, the message's own being what the share runs on.
    let edit = v(edit_live_location(CHAT, LINE, Some(&fix)));
    assert_eq!(edit["@type"], "editMessageLiveLocation");
    assert_eq!(edit["chat_id"], CHAT);
    assert_eq!(edit["message_id"], LINE);
    assert!(edit["reply_markup"].is_null());
    assert_eq!(edit["location"]["@type"], "liveLocation");
    assert_eq!(edit["location"]["live_period"], 0);
    assert_eq!(edit["location"]["location"]["latitude"], 47.0472);

    // And a stop is the same edit with the location gone — never a delete.
    let stop = v(edit_live_location(CHAT, LINE, None));
    assert_eq!(stop["@type"], "editMessageLiveLocation");
    assert!(stop["location"].is_null());

    // Due north is the one direction the wire cannot spell the obvious way.
    let mut north = Fix::at(0.0, 0.0, 0.0);
    north.heading_deg = Some(0.0);
    assert_eq!(
        v(send_live_location(CHAT, &north, 60))["input_message_content"]["location"]["heading"],
        360
    );
    // Standing still is no heading at all.
    assert_eq!(
        v(send_live_location(CHAT, &Fix::at(0.0, 0.0, 0.0), 60))["input_message_content"]["location"]
            ["heading"],
        0
    );

    // The four periods the bar offers, ending in the wire's *until stopped*.
    assert_eq!(LIVE_PERIODS.map(|(secs, _)| secs), [900, 3600, 28800, LIVE_FOREVER]);
    assert_eq!(LIVE_FOREVER, i64::from(i32::MAX));
}

/// The phone's rule: an edit goes out when the fix has moved more than a
/// metre *and* the pin was put down at least ten seconds ago. Neither half
/// alone is enough — and the send that started the share put the pin down,
/// so the first pass has nothing to say.
#[test]
fn a_share_is_edited_only_when_the_fix_moved_and_the_last_edit_is_old() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let device = device(&w);
    device.set_fix(Fix::at(47.0472, 8.3164, w.now()));
    share(&acc, &w, 3600);

    // The share was learned from its own echo, which carries the fix the
    // send carried: editing it back in would be the same message twice.
    acc.drain(&w);
    assert!(
        last_edit(&td).is_none(),
        "the send's own fix is not sent again a moment later"
    );
    let sent = td.sent().len();

    // A pass a moment later, from the same place: nothing to say.
    clock.advance(30.0);
    acc.drain(&w);
    assert_eq!(td.sent().len(), sent, "a device that has not moved sends nothing");

    // Half a metre is not a move.
    device.set_fix(Fix::at(47.047_204, 8.3164, w.now()));
    acc.drain(&w);
    assert_eq!(td.sent().len(), sent, "under a metre the pin would jitter");

    // Twenty metres, and thirty seconds since the pin went down: an edit.
    device.set_fix(Fix::at(47.047_38, 8.3164, w.now()));
    acc.drain(&w);
    let moved = last_edit(&td).expect("a move is worth an edit");
    assert_eq!(moved["location"]["location"]["latitude"], 47.047_38);
    let sent = td.sent().len();

    // Moving again at once is too soon: the edit before it is seconds old.
    clock.advance(3.0);
    device.set_fix(Fix::at(47.047_6, 8.3164, w.now()));
    acc.drain(&w);
    assert_eq!(td.sent().len(), sent, "ten seconds between edits, as the clients have it");
    clock.advance(8.0);
    acc.drain(&w);
    assert!(td.sent().len() > sent, "and once they are past, the move goes");
}

/// A share is the receiver's one holder while it runs: wanted on the first,
/// released on the last, and never twice for two.
#[test]
fn the_worker_holds_the_receiver_for_as_long_as_a_share_runs() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let device = device(&w);
    assert_eq!(device.wanted(), 0);

    share(&acc, &w, 3600);
    assert_eq!(device.wanted(), 1, "the first share turns the receiver on");

    // A second chat's share is one more share and not one more holder.
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": live_line(-7002, 901, w.now(), 3600)})
            .to_string(),
    );
    assert_eq!(device.wanted(), 1);
    assert_eq!(runtime::of(w.store()).live_shares().len(), 2);

    // Stopping both lets it go once.
    for chat in [CHAT, -7002] {
        runtime::of(w.store()).stop_live(chat);
    }
    acc.drain(&w);
    assert_eq!(device.wanted(), 0, "the last share lets the receiver go");
    assert!(runtime::of(w.store()).live_shares().is_empty());
}

/// A refused receiver is asked once and not again, and the share simply
/// stands still: the panel is where a person hears about a permission, not
/// a pass that runs three times a second.
#[test]
fn a_refused_receiver_is_asked_once_and_the_share_sends_nothing() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let device = device(&w);
    // A receiver that was said no to answers nothing and holds nothing.
    device.deny("location is not allowed");
    device.clear();
    share(&acc, &w, 3600);
    for _ in 0..3 {
        acc.drain(&w);
    }
    assert!(last_edit(&td).is_none(), "nothing to say without a reading");
    assert_eq!(device.wanted(), 0, "a refusal takes no hold");

    // The ask is not made again on every pass: allowing it now would turn a
    // retried ask into a hold, and there is none.
    device.allow();
    acc.drain(&w);
    assert_eq!(
        device.wanted(),
        0,
        "the worker asks once, and the panel is where a refusal is said"
    );

    // A fresh share is a fresh ask, and now it is granted.
    runtime::of(w.store()).stop_live(CHAT);
    acc.drain(&w);
    device.set_fix(Fix::at(47.0472, 8.3164, w.now()));
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": live_line(CHAT, 902, w.now(), 3600)})
            .to_string(),
    );
    assert_eq!(device.wanted(), 1);
    // And a reading the granted receiver gives, a move past the metre and
    // the ten seconds later, is what goes out — the first pass having
    // nothing to add to where the send put the pin.
    clock.advance(11.0);
    device.set_fix(Fix::at(47.047_38, 8.3164, w.now()));
    acc.drain(&w);
    let moved = last_edit(&td).expect("a granted receiver moves the pin");
    assert_eq!(moved["message_id"], 902);
    assert_eq!(moved["location"]["location"]["latitude"], 47.047_38);
}

/// A line out of a history page whose period has already run out is a place
/// that was shared, not a share to keep moving.
#[test]
fn an_expired_live_location_of_mine_is_not_taken_up_again() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let started = w.now() - 4000.0;
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": live_line(CHAT, 903, started, 3600)})
            .to_string(),
    );
    assert!(runtime::of(w.store()).live_shares().is_empty());
    assert_eq!(device(&w).wanted(), 0);
}

/// A stop is an edit with the location gone, and the share is the worker's
/// to forget.
#[test]
fn stopping_a_share_ends_it_with_an_edit_and_not_a_delete() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    device(&w).set_fix(Fix::at(47.0472, 8.3164, w.now()));
    share(&acc, &w, 3600);

    runtime::of(w.store()).stop_live(CHAT);
    acc.drain(&w);
    let stop = last_edit(&td).expect("the stop went out");
    assert_eq!(stop["message_id"], LINE);
    assert!(stop["location"].is_null());
    assert!(!td.sent_types().contains(&"deleteMessages".to_string()));
    assert!(runtime::of(w.store()).live_share(CHAT, w.now()).is_none());

    // And nothing more is sent for it.
    let sent = td.sent().len();
    acc.drain(&w);
    assert_eq!(td.sent().len(), sent);
}

/// At the period's end the share is dropped and nothing is sent, as the
/// phone does: the server has ended it on its own clock.
#[test]
fn a_share_past_its_period_is_dropped_without_a_request() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let device = device(&w);
    device.set_fix(Fix::at(47.0472, 8.3164, w.now()));
    share(&acc, &w, 900);
    acc.drain(&w);
    let sent = td.sent().len();

    clock.advance(901.0);
    device.set_fix(Fix::at(47.05, 8.32, w.now()));
    acc.drain(&w);
    assert_eq!(td.sent().len(), sent, "an ended share is not edited goodbye");
    assert!(runtime::of(w.store()).live_shares().is_empty());
    assert_eq!(device.wanted(), 0, "and the receiver goes with it");
}

/// A restart re-registers what is still running from the wire's own list, and
/// a share that ended elsewhere is simply not in it.
#[test]
fn sign_in_restores_the_shares_the_wire_still_has() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    device(&w).set_fix(Fix::at(47.0472, 8.3164, w.now()));
    share(&acc, &w, 3600);
    assert_eq!(runtime::of(w.store()).live_shares().len(), 1);

    // Signing in forgets what this run believed…
    acc.on_ready(&w);
    assert!(runtime::of(w.store()).live_shares().is_empty());
    assert_eq!(device(&w).wanted(), 0);

    // …and the wire's list is what puts it back — with the pin where the
    // message says it last stood, which for a share that outlived a restart
    // was a while ago.
    acc.on_update(
        &w,
        &json!({"@type": "updateActiveLiveLocationMessages",
                "messages": [live_line(CHAT, LINE, w.now() - 600.0, 3600)]})
        .to_string(),
    );
    let shares = runtime::of(w.store()).live_shares();
    assert_eq!(shares.len(), 1);
    assert_eq!((shares[0].chat, shares[0].message), (CHAT, LINE));
    assert_eq!(device(&w).wanted(), 1);
    acc.drain(&w);
    assert!(
        last_edit(&td).is_none(),
        "the pin is already where the wire says it is"
    );
    device(&w).set_fix(Fix::at(47.047_38, 8.3164, w.now()));
    acc.drain(&w);
    assert!(last_edit(&td).is_some(), "a restored share goes on moving");

    // An empty list is the wire saying nothing of mine is running.
    acc.on_update(
        &w,
        &json!({"@type": "updateActiveLiveLocationMessages", "messages": []}).to_string(),
    );
    assert!(runtime::of(w.store()).live_shares().is_empty());
    assert_eq!(device(&w).wanted(), 0);
}

/// A send's echo is learned only once Telegram has given the line its own id:
/// the temporary one a pending message wears is not a line an edit can name.
#[test]
fn a_pending_send_is_not_a_share_until_its_id_is_real() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    device(&w).set_fix(Fix::at(47.0472, 8.3164, w.now()));

    let mut pending = live_line(CHAT, 1_000_000_001, w.now(), 3600);
    pending["sending_state"] = json!({"@type": "messageSendingStatePending"});
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": pending}).to_string(),
    );
    assert!(runtime::of(w.store()).live_shares().is_empty());

    acc.on_update(
        &w,
        &json!({"@type": "updateMessageSendSucceeded",
                "old_message_id": 1_000_000_001,
                "message": live_line(CHAT, LINE, w.now(), 3600)})
        .to_string(),
    );
    let shares = runtime::of(w.store()).live_shares();
    assert_eq!(shares.len(), 1);
    assert_eq!(shares[0].message, LINE);
}

/// Somebody else's live location is a line to draw, not a share of mine to
/// keep moving.
#[test]
fn an_incoming_live_location_is_drawn_and_never_moved() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let at = w.now();
    let mut theirs = live_line(CHAT, 77, at, 3600);
    theirs["is_outgoing"] = json!(false);
    acc.on_update(&w, &dialog_snapshot(CHAT, 0, 0));
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": theirs}).to_string(),
    );
    assert!(runtime::of(w.store()).live_shares().is_empty());
    assert_eq!(device(&w).wanted(), 0);

    let row = model::line(w.store(), CHAT, 77).expect("the line");
    let media = row.media.expect("a live location");
    assert_eq!(media.kind, "live");
    assert_eq!(media.lat, Some(47.0472));
    assert_eq!(media.until, Some(at + 3600.0));
    assert_eq!(
        media.updated,
        Some(at),
        "a share that has not moved was last put down when it was sent"
    );
}

/// A live location that moves: the row's coordinates follow the edit, its
/// *updated* time is when the edit landed, and the expiry comes from what the
/// wire says is left rather than from a date the edit does not carry.
#[test]
fn an_edited_live_location_moves_the_row_and_says_when_it_last_moved() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let at = w.now();
    let mut theirs = live_line(CHAT, 78, at, 3600);
    theirs["is_outgoing"] = json!(false);
    acc.on_update(&w, &dialog_snapshot(CHAT, 0, 0));
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": theirs}).to_string(),
    );

    clock.advance(120.0);
    acc.on_update(
        &w,
        &json!({"@type": "updateMessageContent", "chat_id": CHAT, "message_id": 78,
            "new_content": {
                "@type": "messageLiveLocation",
                "location": {
                    "@type": "liveLocation",
                    "location": {"latitude": 55.7512, "longitude": 37.6184, "horizontal_accuracy": 9.0},
                    "live_period": 3600, "heading": 12, "proximity_alert_radius": 0
                },
                // Apple's clients send a second short of the hour; what is
                // left is the truer end than a period added to a date.
                "expires_in": 3479
            }})
        .to_string(),
    );
    let media = model::line(w.store(), CHAT, 78).expect("the line").media.expect("media");
    assert_eq!(media.kind, "live");
    assert_eq!(media.lat, Some(55.7512));
    assert_eq!(media.lon, Some(37.6184));
    assert_eq!(media.updated, Some(at + 120.0));
    assert_eq!(media.until, Some(at + 120.0 + 3479.0));
    assert_eq!(
        media.line(at + 240.0),
        "live location 55.7512, 37.6184 · 56 min left · updated 2 min ago"
    );
}

/// While a share runs the pass never sleeps longer than a second, however
/// quiet the wire is.
#[test]
fn a_running_share_keeps_the_pass_awake() {
    assert_eq!(super::super::next_pass_sharing(0, false), Wake::After(POLL));
    assert_eq!(
        super::super::next_pass_sharing(0, true),
        Wake::After(POLL.min(super::super::LIVE_TICK))
    );
    assert!(
        matches!(super::super::next_pass_sharing(super::super::UPDATES_PER_PASS, true),
            Wake::After(d) if d <= super::super::LIVE_TICK),
        "a backlog is drained no slower for a share"
    );
}
