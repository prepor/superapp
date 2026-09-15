//! A call walked through the wire's states, against the fake transport and
//! the fake engine.

use super::*;
use crate::apps::telegram::calls::{Doing, Link, Ready, Told, FAKE_CONNECTS_AFTER};
use crate::apps::telegram::requests;
use crate::apps::telegram::runtime::{CallState, CallWish, Reason};
use crate::apps::telegram::sync::calls::READY_WAIT;

/// The other side, in every test here.
const VERA: i64 = 7;

/// How many times the engine was told to carry a call.
fn starts(acc: &Account<FakeTd>) -> usize {
    acc.engine.heard().iter().filter(|d| matches!(d, Doing::Start(_))).count()
}

/// What the engine was started with, the last time it was.
fn started(acc: &Account<FakeTd>) -> Ready {
    acc.engine
        .heard()
        .into_iter()
        .rev()
        .find_map(|d| match d {
            Doing::Start(ready) => Some(*ready),
            _ => None,
        })
        .expect("the engine was never started")
}

/// The fake camera and microphone this world was built with, which a test
/// answers the permission dialog through.
fn fake_capture(w: &World) -> kernel::caps::FakeCapture {
    w.caps(|c| c.get::<kernel::caps::FakeCapture>().expect("the fake capture").clone())
}

fn update(state: serde_json::Value, outgoing: bool, video: bool) -> String {
    json!({"@type": "updateCall", "call": {
        "@type": "call", "id": 42, "unique_id": 9_001_i64, "user_id": VERA,
        "is_outgoing": outgoing, "is_video": video, "state": state,
    }})
    .to_string()
}

fn pending(created: bool, received: bool) -> serde_json::Value {
    json!({"@type": "callStatePending", "is_created": created, "is_received": received})
}

/// What `callStateReady` carries: a key in base64, one reflector and one
/// WebRTC server, the versions and the four emoji.
fn ready() -> serde_json::Value {
    json!({
        "@type": "callStateReady",
        "protocol": {"@type": "callProtocol", "udp_p2p": true, "udp_reflector": true,
                     "min_layer": 65, "max_layer": 92, "library_versions": ["11.0.0", "2.7.7"]},
        "servers": [
            {"@type": "callServer", "id": 1, "ip_address": "1.2.3.4", "ipv6_address": "::1",
             "port": 595, "type": {"@type": "callServerTypeTelegramReflector",
                                   "peer_tag": "AQID", "is_tcp": true}},
            {"@type": "callServer", "id": 2, "ip_address": "5.6.7.8", "ipv6_address": "",
             "port": 3478, "type": {"@type": "callServerTypeWebrtc", "username": "u",
                                    "password": "p", "supports_turn": true, "supports_stun": false}},
        ],
        "config": "{}",
        "encryption_key": "AAEC",
        "emojis": ["🦊", "🍀", "🎈", "🛰"],
        "allow_p2p": true,
        "is_group_call_supported": false,
        "custom_parameters": "",
    })
}

fn discarded(reason: &str, need_rating: bool) -> serde_json::Value {
    json!({"@type": "callStateDiscarded", "reason": {"@type": reason},
           "need_rating": need_rating, "need_debug_information": false, "need_log": false})
}

#[test]
fn an_outgoing_call_walks_the_wires_states_and_the_engine_connects_it() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let rt = runtime::of(w.store());

    acc.on_update(&w, &update(pending(false, false), true, false));
    assert_eq!(rt.call(VERA).expect("a call").state, CallState::Contacting);
    acc.on_update(&w, &update(pending(true, false), true, false));
    assert_eq!(rt.call(VERA).expect("a call").state, CallState::Waiting);
    acc.on_update(&w, &update(pending(true, true), true, false));
    assert_eq!(rt.call(VERA).expect("a call").state, CallState::Ringing);
    acc.on_update(&w, &update(json!({"@type": "callStateExchangingKeys"}), true, false));
    assert_eq!(rt.call(VERA).expect("a call").state, CallState::ExchangingKeys);

    // The one update that carries everything. The engine is handed the key
    // out of its base64 and the servers in its own shape.
    acc.on_update(&w, &update(ready(), true, false));
    let call = rt.call(VERA).expect("a call");
    assert_eq!(call.state, CallState::Connecting);
    assert_eq!(call.emoji, vec!["🦊", "🍀", "🎈", "🛰"]);
    let Some(Doing::Start(started)) = acc.engine.heard().into_iter().next() else {
        panic!("the engine was never started");
    };
    assert_eq!(started.key, vec![0, 1, 2], "the key comes out of its base64");
    assert!(started.outgoing && started.allow_p2p);
    assert_eq!(started.versions, vec!["11.0.0", "2.7.7"]);
    let reflector = &started.servers[0];
    assert_eq!((reflector.port, reflector.tcp, reflector.peer_tag.clone()), (595, true, vec![1, 2, 3]));
    assert!(!reflector.turn && !reflector.stun);
    let webrtc = &started.servers[1];
    assert_eq!((webrtc.username.as_str(), webrtc.password.as_str()), ("u", "p"));
    assert!(webrtc.turn && !webrtc.stun && !webrtc.tcp);

    // The fake takes two of the world's seconds to connect, and the timer
    // runs from there.
    acc.drain(&w);
    assert_eq!(rt.call(VERA).expect("a call").state, CallState::Connecting);
    clock.advance(FAKE_CONNECTS_AFTER);
    acc.drain(&w);
    let call = rt.call(VERA).expect("a call");
    assert_eq!(call.state, CallState::Connected);
    assert_eq!(call.secs(w.now()), 0);
    clock.advance(151.0);
    assert_eq!(rt.call(VERA).expect("a call").secs(w.now()), 151);

    // Hung up: the row says so, the engine is let go, and the line in the
    // chat is the wire's to write.
    acc.on_update(&w, &update(discarded("callDiscardReasonHungUp", true), true, false));
    let call = rt.call(VERA).expect("a call");
    assert_eq!(call.state, CallState::Ended);
    assert_eq!(call.reason, Some(Reason::HungUp));
    assert!(call.need_rating);
    assert_eq!(call.secs(w.now()), 151, "a finished call's length stands still");
    clock.advance(60.0);
    assert_eq!(rt.call(VERA).expect("a call").secs(w.now()), 151);
    assert!(acc.engine.heard().contains(&Doing::Stop(VERA)));
}

/// Ending a call stops the media with the request, not with the wire's
/// answer to it. Between the two is a person who has hung up and is still
/// being heard — seconds of it on a poor connection.
#[test]
fn hanging_up_stops_the_engine_before_the_wire_answers() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let rt = runtime::of(w.store());

    acc.on_update(&w, &update(ready(), true, true));
    assert!(!acc.engine.heard().contains(&Doing::Stop(VERA)), "a live call carries");

    // What *end* on the bar leaves behind: the row goes *hanging up*, and
    // the discard is a command the worker sends.
    rt.change_call(VERA, |c| c.state = CallState::HangingUp);
    acc.send(&w, &requests::discard_call(42, false, 7, true));
    assert!(
        acc.engine.heard().contains(&Doing::Stop(VERA)),
        "the engine is let go with the request, not with its answer"
    );
    assert_eq!(last_request(&td, "discardCall")["call_id"], 42);
    assert_eq!(
        rt.call(VERA).expect("a call").state,
        CallState::HangingUp,
        "and the row waits for the wire to say which ending it was"
    );

    // The wire's terminal update finds an engine already let go and a
    // camera already given back, and both are answers rather than failures.
    acc.on_update(&w, &update(discarded("callDiscardReasonHungUp", false), true, true));
    let call = rt.call(VERA).expect("a call");
    assert_eq!((call.state, call.reason), (CallState::Ended, Some(Reason::HungUp)));
}

/// A call asks for the microphone the moment it appears — ringing, or
/// contacting — rather than when the media is ready. The engine captures
/// through its own library, so nothing else would ever ask, and a phone that
/// was never asked hands over silence.
#[test]
fn a_call_appearing_asks_for_the_microphone() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    let capture = fake_capture(&w);
    assert_eq!(capture.microphone_asked(), 0);

    acc.on_update(&w, &update(pending(true, true), false, false));
    assert_eq!(
        runtime::of(w.store()).call(VERA).expect("a call").state,
        CallState::Incoming
    );
    assert!(capture.microphone_asked() > 0, "asked while it is still ringing");

    // An outgoing one asks at *contacting…*, which is as early as there is
    // a call to ask for.
    let w = world();
    let acc = account(FakeTd::new(), None);
    let capture = fake_capture(&w);
    acc.on_update(&w, &update(pending(false, false), true, false));
    assert_eq!(
        runtime::of(w.store()).call(VERA).expect("a call").state,
        CallState::Contacting
    );
    assert!(capture.microphone_asked() > 0);
}

#[test]
fn an_incoming_call_asks_for_its_panel_once_and_a_decline_needs_no_engine() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let rt = runtime::of(w.store());

    acc.on_update(&w, &update(pending(true, true), false, true));
    let call = rt.call(VERA).expect("a call");
    assert_eq!(call.state, CallState::Incoming);
    assert!(call.video && !call.outgoing);
    assert_eq!(rt.take_show_call(), Some(VERA), "somebody ringing opens their panel");
    // Once: the wire repeats an update and the panel is already up.
    acc.on_update(&w, &update(pending(true, true), false, true));
    assert_eq!(rt.take_show_call(), None);

    acc.on_update(&w, &update(discarded("callDiscardReasonDeclined", false), false, true));
    let call = rt.call(VERA).expect("a call");
    assert_eq!((call.state, call.reason), (CallState::Ended, Some(Reason::Declined)));
    assert!(!call.need_rating);
    assert!(
        !acc.engine.heard().iter().any(|d| matches!(d, Doing::Start(_))),
        "a refused call never starts an engine"
    );
}

#[test]
fn a_call_the_other_side_refuses_or_the_wire_fails_says_which() {
    for (state, want) in [
        (discarded("callDiscardReasonDeclined", false), CallState::Ended),
        (discarded("callDiscardReasonMissed", false), CallState::Ended),
        (json!({"@type": "callStateError", "error": {"code": 400, "message": "PARTICIPANT_VERSION_OUTDATED"}}),
         CallState::Failed),
        (json!({"@type": "callStateHangingUp"}), CallState::HangingUp),
    ] {
        let w = world();
        let acc = account(FakeTd::new(), None);
        acc.on_update(&w, &update(pending(true, true), true, false));
        acc.on_update(&w, &update(state, true, false));
        let call = runtime::of(w.store()).call(VERA).expect("a call");
        assert_eq!(call.state, want);
        if want == CallState::Failed {
            assert_eq!(call.error.as_deref(), Some("PARTICIPANT_VERSION_OUTDATED"));
        }
    }
}

#[test]
fn signalling_is_relayed_both_ways_and_stops_when_the_call_does() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);

    acc.on_update(&w, &update(ready(), true, false));
    // Inwards: the wire's packet reaches the engine, matched to the call by
    // the only id the wire names it by.
    acc.on_update(&w, &json!({"@type": "updateNewCallSignalingData",
        "call_id": 42, "data": "AAEC"}).to_string());
    assert!(acc.engine.heard().contains(&Doing::Signalling(VERA, vec![0, 1, 2])));
    // A packet for a call nobody has is dropped rather than guessed at.
    let heard = acc.engine.heard().len();
    acc.on_update(&w, &json!({"@type": "updateNewCallSignalingData",
        "call_id": 99, "data": "AAEC"}).to_string());
    assert_eq!(acc.engine.heard().len(), heard);

    // Outwards: what the engine says goes as one request, base64 and all.
    acc.said(Told::Signalling { user: VERA, data: vec![9, 8, 7] });
    acc.drain(&w);
    let sent = last_request(&td, "sendCallSignalingData");
    assert_eq!(sent["call_id"], 42);
    assert_eq!(sent["data"], "CQgH");

    // Once the call is over there is nowhere to send it.
    acc.on_update(&w, &update(discarded("callDiscardReasonHungUp", false), true, false));
    acc.said(Told::Signalling { user: VERA, data: vec![1] });
    acc.drain(&w);
    assert_eq!(last_request(&td, "sendCallSignalingData")["data"], "CQgH");
}

#[test]
fn a_connection_that_comes_back_is_reconnecting_and_one_that_dies_discards_the_call() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let rt = runtime::of(w.store());

    // Pending, not ready: the link is driven by hand here, so the fake
    // engine's own two seconds do not answer for it.
    acc.on_update(&w, &update(pending(true, true), true, true));
    acc.said(Told::Link { user: VERA, link: Link::Connected });
    acc.drain(&w);
    assert_eq!(rt.call(VERA).expect("a call").state, CallState::Connected);
    clock.advance(10.0);
    // *Connecting* after it had once connected is the reference clients'
    // *reconnecting*; no engine says that word.
    acc.said(Told::Link { user: VERA, link: Link::Connecting });
    acc.drain(&w);
    assert_eq!(rt.call(VERA).expect("a call").state, CallState::Reconnecting);
    acc.said(Told::Link { user: VERA, link: Link::Connected });
    acc.drain(&w);
    let call = rt.call(VERA).expect("a call");
    assert_eq!(call.state, CallState::Connected);
    assert_eq!(call.secs(w.now()), 10, "the timer runs from the first connection, not the second");

    acc.said(Told::Link { user: VERA, link: Link::Failed("the connection failed".into()) });
    acc.drain(&w);
    let sent = last_request(&td, "discardCall");
    assert_eq!(sent["call_id"], 42);
    assert_eq!(sent["is_disconnected"], true);
    assert_eq!(sent["duration"], 10);
    assert_eq!(sent["is_video"], true);
}

#[test]
fn what_a_panel_asks_the_engine_for_reaches_it_on_the_next_pass() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    let rt = runtime::of(w.store());
    acc.on_update(&w, &update(ready(), true, true));
    rt.wish_call(VERA, crate::apps::telegram::runtime::CallWish::Mute(true));
    rt.wish_call(VERA, crate::apps::telegram::runtime::CallWish::Camera(false));
    acc.drain(&w);
    let heard = acc.engine.heard();
    assert!(heard.contains(&Doing::Mute(VERA, true)));
    assert!(heard.contains(&Doing::Camera(VERA, false)));
    assert!(rt.take_call_wishes().is_empty(), "a wish is passed on once");
}

/// *end* pressed in the moment between the card placing a call and the wire
/// naming it. There is no id to discard by, so nothing goes out then — and
/// the discard goes the instant the wire says which call it is, rather than
/// the call standing up and ringing the other side again.
#[test]
fn a_call_ended_before_the_wire_named_it_is_discarded_the_moment_it_is() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let rt = runtime::of(w.store());

    // What the person's card writes, and what pressing *end* leaves on it.
    rt.put_call(crate::apps::telegram::runtime::Call::new(0, VERA, true, false));
    rt.change_call(VERA, |c| c.state = CallState::HangingUp);
    assert!(td.sent().is_empty(), "a call with no id is not discarded by name");

    acc.on_update(&w, &update(pending(false, false), true, false));
    let call = rt.call(VERA).expect("a call");
    assert_eq!(call.id, 42, "the wire's id is the row's from here on");
    assert_eq!(call.state, CallState::HangingUp, "it does not stand back up");
    let sent = last_request(&td, "discardCall");
    assert_eq!(sent["call_id"], 42);
    assert_eq!(sent["is_disconnected"], false);
    assert!(
        acc.engine.heard().contains(&Doing::Stop(VERA)),
        "and the media stops with it"
    );
}

#[test]
fn signing_in_forgets_whatever_was_happening_before() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    let rt = runtime::of(w.store());
    acc.on_update(&w, &update(pending(true, true), false, false));
    assert!(rt.call(VERA).is_some());
    assert_eq!(rt.take_show_call(), Some(VERA));
    acc.on_ready(&w);
    assert!(rt.calls().is_empty(), "a call is this run's, like a phantom send");
    assert_eq!(rt.take_show_call(), None);
    assert!(
        acc.engine.heard().contains(&Doing::Stop(VERA)),
        "the engine is let go before the row it belonged to"
    );
}

/// Which way a call went is the *message's* and not the content's, so an
/// edit — which carries the content and no message — takes it from the row
/// it is editing. TDLib does not edit a call's content today; a line of mine
/// that began reading *incoming* when it did would be a bug nobody was
/// looking for.
#[test]
fn an_edited_call_keeps_the_way_it_went() {
    use crate::apps::telegram::model;

    let w = world();
    let acc = account(FakeTd::new(), None);
    let content = |secs: i64| {
        json!({"@type": "messageCall", "is_video": false, "duration": secs,
               "discard_reason": {"@type": "callDiscardReasonHungUp"}})
    };
    acc.on_update(&w, &dialog_snapshot(VERA, 0, 0));
    acc.on_update(
        &w,
        &json!({"@type": "updateNewMessage", "message": {
            "@type": "message", "id": 500, "chat_id": VERA, "date": w.now(), "is_outgoing": true,
            "sender_id": {"@type": "messageSenderUser", "user_id": 2},
            "content": content(151)}})
        .to_string(),
    );
    let said = |w: &World| {
        model::line(w.store(), VERA, 500)
            .expect("the line")
            .media
            .expect("a call")
            .line(0.0)
    };
    assert_eq!(said(&w), "outgoing call · 2:31");

    acc.on_update(
        &w,
        &json!({"@type": "updateMessageContent", "chat_id": VERA, "message_id": 500,
                "new_content": content(160)})
        .to_string(),
    );
    assert_eq!(said(&w), "outgoing call · 2:40", "and it is still mine");
}

/// The engine is not started while the microphone's dialog is still
/// standing open. It captures through its own library, and a capture opened
/// under an unanswered dialog hands over silence for the whole of the call —
/// so the wire's *ready* waits on the row, and the answer is what starts it.
#[test]
fn a_ready_waits_for_the_microphones_permission() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    let rt = runtime::of(w.store());
    fake_capture(&w).microphone_unanswered();

    acc.on_update(&w, &update(ready(), true, false));
    let call = rt.call(VERA).expect("a call");
    assert_eq!(call.state, CallState::Connecting, "the row says what the wire said");
    assert!(call.pending_ready.is_some(), "and the media waits on it");
    assert_eq!(starts(&acc), 0, "nothing is carried under an open dialog");
    acc.drain(&w);
    assert_eq!(starts(&acc), 0, "and a pass alone does not start it");

    // Answered: the next pass carries the call.
    fake_capture(&w).answer_microphone(true);
    acc.drain(&w);
    assert_eq!(starts(&acc), 1, "the answer is what starts it");
    let call = rt.call(VERA).expect("a call");
    assert!(call.pending_ready.is_none());
    assert_eq!(call.error, None, "and a granted microphone says nothing");
    assert!(call.microphone && started(&acc).microphone, "the call carries one");

    // Refused, and the call goes ahead anyway: the other side is still
    // heard, and the panel is where a person learns why nobody hears them.
    // The engine is started with no microphone named at all — the library
    // opens whatever a description names and throws where it cannot, and a
    // call discarded on its first step is worse than a call with no voice
    // going out.
    let w = world();
    let acc = account(FakeTd::new(), None);
    fake_capture(&w).answer_microphone(false);
    acc.on_update(&w, &update(ready(), true, false));
    assert_eq!(starts(&acc), 1, "a refusal is not a call that never starts");
    let call = runtime::of(w.store()).call(VERA).expect("a call");
    assert_eq!(call.error.as_deref(), Some("the microphone is not allowed"));
    assert!(!call.microphone, "and the row says the call has none");
    assert!(!started(&acc).microphone, "nor does what the engine was handed");
}

/// And a dialog nobody ever answers does not hold a call for ever: the wire
/// rings for longer than this wait, so a person coming back to the glass
/// finds a call rather than a row that says *connecting* and means nothing.
#[test]
fn a_ready_nobody_answers_for_starts_when_the_wait_runs_out() {
    let clock = FakeClock::default();
    let w = timed_world(&clock);
    let acc = account(FakeTd::new(), None);
    fake_capture(&w).microphone_unanswered();

    acc.on_update(&w, &update(ready(), true, false));
    clock.advance(READY_WAIT - 1.0);
    acc.drain(&w);
    assert_eq!(starts(&acc), 0, "still waiting for the person");
    clock.advance(2.0);
    acc.drain(&w);
    assert_eq!(starts(&acc), 1);
    let call = runtime::of(w.store()).call(VERA).expect("a call");
    assert!(call.pending_ready.is_none());
    // Started with no microphone, as a refusal is — the dialog is still
    // standing and its device would throw — but the words are not a
    // refusal's: nobody has said no, and a yes may still come.
    assert!(!call.microphone && !started(&acc).microphone);
    assert_eq!(call.error.as_deref(), Some("the microphone has not been allowed yet"));
}

/// The bar works for the whole of that wait, and the engine has no call to
/// be told about while it lasts. So *mute* and *camera* land on the row, and
/// the row is what the call is started with — otherwise a person who muted
/// a call that had not begun would be heard for the whole of it.
#[test]
fn a_choice_made_while_a_ready_waits_is_what_the_call_starts_with() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    let rt = runtime::of(w.store());
    fake_capture(&w).microphone_unanswered();

    acc.on_update(&w, &update(ready(), true, true));
    assert_eq!(starts(&acc), 0, "parked on the dialog");

    // What the bar does meanwhile. The panel writes the row as it wishes;
    // the worker writes it again, so a wish from anywhere says the same
    // thing.
    rt.wish_call(VERA, CallWish::Mute(true));
    rt.wish_call(VERA, CallWish::Camera(false));
    acc.drain(&w);
    assert!(
        !acc.engine
            .heard()
            .iter()
            .any(|d| matches!(d, Doing::Mute(..) | Doing::Camera(..))),
        "an engine holding no call is told nothing: it would drop it"
    );
    let call = rt.call(VERA).expect("a call");
    assert!(call.muted && !call.camera, "the row is where the choice is kept");
    assert_eq!(starts(&acc), 0);

    // Nor does the wire repeating itself put the camera back on: what kind
    // of call this is was answered on the first *ready*.
    acc.on_update(&w, &update(ready(), true, true));
    assert!(!rt.call(VERA).expect("a call").camera);

    // And the start carries both.
    fake_capture(&w).answer_microphone(true);
    acc.drain(&w);
    assert_eq!(starts(&acc), 1);
    let ready = started(&acc);
    assert!(ready.muted, "a call muted before it began begins muted");
    assert!(!ready.video, "and with the camera the row says is off");

    // From here the engine is holding the call and hears them itself.
    rt.wish_call(VERA, CallWish::Camera(true));
    rt.wish_call(VERA, CallWish::Mute(false));
    acc.drain(&w);
    let heard = acc.engine.heard();
    assert!(heard.contains(&Doing::Camera(VERA, true)));
    assert!(heard.contains(&Doing::Mute(VERA, false)));
    let call = rt.call(VERA).expect("a call");
    assert!(call.camera && !call.muted, "and the row follows");
}

/// A microphone allowed after the call had already begun without one. The
/// person answers the dialog a moment late, or comes back from the
/// platform's settings; a call that stayed silent to the end because of it
/// would be the worst of both.
#[test]
fn a_microphone_allowed_after_the_call_began_is_turned_on() {
    let w = world();
    let acc = account(FakeTd::new(), None);
    let rt = runtime::of(w.store());
    fake_capture(&w).answer_microphone(false);

    acc.on_update(&w, &update(ready(), true, false));
    assert!(!started(&acc).microphone, "a refusal carries no voice out");
    acc.drain(&w);
    let told = |acc: &Account<FakeTd>| {
        acc.engine.heard().iter().filter(|d| matches!(d, Doing::Microphone(..))).count()
    };
    assert_eq!(told(&acc), 0, "nothing while it is still refused");

    fake_capture(&w).answer_microphone(true);
    acc.drain(&w);
    assert!(acc.engine.heard().contains(&Doing::Microphone(VERA, true)));
    let call = rt.call(VERA).expect("a call");
    assert!(call.microphone, "the row says the call is carrying one now");
    assert_eq!(call.error, None, "and the line the panel was showing goes with it");

    // Once, and not on every pass that follows.
    acc.drain(&w);
    assert_eq!(told(&acc), 1);

    // A call that is over is left alone, however the permission stands.
    let w = world();
    let acc = account(FakeTd::new(), None);
    fake_capture(&w).answer_microphone(false);
    acc.on_update(&w, &update(ready(), true, false));
    acc.on_update(&w, &update(discarded("callDiscardReasonHungUp", false), true, false));
    fake_capture(&w).answer_microphone(true);
    acc.drain(&w);
    assert_eq!(told(&acc), 0, "there is nothing left to turn on");
}

/// A *ready* that arrives for a call already hanging up is dropped. The
/// wire repeats itself and its queue runs behind, and a state taken after
/// *end* would start the engine again and reopen the camera on a call
/// nobody is in — only the wire's own endings may move a row from here.
#[test]
fn a_late_ready_does_not_stand_a_hung_up_call_back_up() {
    let w = world();
    let td = FakeTd::new();
    let acc = account(td.clone(), None);
    let rt = runtime::of(w.store());

    acc.on_update(&w, &update(ready(), true, true));
    assert_eq!(starts(&acc), 1);

    // What *end* on the bar leaves: the discard out, the media down, and
    // the row *hanging up* until the wire says which ending it was.
    rt.change_call(VERA, |c| c.state = CallState::HangingUp);
    acc.send(&w, &requests::discard_call(42, false, 7, true));
    assert!(acc.engine.heard().contains(&Doing::Stop(VERA)));

    // The wire catching up with itself.
    for late in [ready(), pending(true, true), json!({"@type": "callStateExchangingKeys"})] {
        acc.on_update(&w, &update(late, true, true));
        assert_eq!(starts(&acc), 1, "the engine is not started a second time");
        assert_eq!(
            rt.call(VERA).expect("a call").state,
            CallState::HangingUp,
            "and the row does not stand back up"
        );
    }
    acc.drain(&w);
    assert_eq!(starts(&acc), 1, "nor on the pass that follows");

    // The ending itself still lands.
    acc.on_update(&w, &update(discarded("callDiscardReasonHungUp", false), true, true));
    assert_eq!(rt.call(VERA).expect("a call").state, CallState::Ended);
}
