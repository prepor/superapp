use super::*;

fn ready(user: i64) -> Ready {
    Ready {
        user,
        outgoing: true,
        video: false,
        muted: false,
        microphone: true,
        key: vec![1, 2, 3],
        servers: Vec::new(),
        versions: vec!["13.0.0".into()],
        allow_p2p: true,
        custom_parameters: None,
    }
}

#[test]
fn the_fake_engine_connects_two_of_the_worlds_seconds_after_it_is_started() {
    let (out, mut said) = tokio::sync::mpsc::unbounded_channel();
    let engine = FakeEngine::new(out);
    engine.start(ready(7));
    assert_eq!(said.try_recv(), Ok(Told::Link { user: 7, link: Link::Connecting }));
    // The clock this side has is the pass's, and the first pass after the
    // start is what the two seconds are counted from.
    engine.tick(100.0);
    engine.tick(101.9);
    assert!(said.try_recv().is_err(), "not yet");
    engine.tick(102.0);
    assert_eq!(said.try_recv(), Ok(Told::Link { user: 7, link: Link::Connected }));
    // And once only.
    engine.tick(200.0);
    assert!(said.try_recv().is_err());
}

#[test]
fn the_fake_engine_keeps_what_it_was_told_and_forgets_a_stopped_call() {
    let (out, mut said) = tokio::sync::mpsc::unbounded_channel();
    let engine = FakeEngine::new(out);
    engine.start(ready(7));
    engine.signalling(7, vec![9, 9]);
    engine.mute(7, true);
    engine.camera(7, true);
    engine.microphone(7, true);
    engine.stop(7);
    assert_eq!(
        engine.heard(),
        vec![
            Doing::Start(Box::new(ready(7))),
            Doing::Signalling(7, vec![9, 9]),
            Doing::Mute(7, true),
            Doing::Camera(7, true),
            Doing::Microphone(7, true),
            Doing::Stop(7),
        ]
    );
    // Stopped before it connected: the two seconds never come.
    let _ = said.try_recv();
    engine.tick(0.0);
    engine.tick(10.0);
    assert!(said.try_recv().is_err(), "a stopped call does not connect");
}

/// What this build offers Telegram is what the engine behind it can then
/// carry. With the engine linked this is the drift detector: NTgCalls
/// answering anything but the layers and versions written down here fails
/// the run, rather than leaving a call that rings and connects to nothing.
/// Without it, it is the fake being held to the same answer.
#[test]
fn what_this_build_speaks_is_the_layers_the_wire_accepts() {
    let p = protocol();
    assert_eq!((p.min_layer, p.max_layer), (MIN_LAYER, MAX_LAYER));
    assert!(p.min_layer <= p.max_layer, "a range no layer falls in is no offer");
    assert!(p.udp_p2p && p.udp_reflector);
    assert_eq!(
        p.library_versions, LIBRARY_VERSIONS,
        "the signalling versions the engine accepts back are the ones offered"
    );
}

#[test]
fn an_i420_frame_becomes_opaque_bgra_and_a_short_one_becomes_nothing() {
    // Two rows of two: white above, black below. One chroma pair for the
    // lot, which is what half-sized planes mean.
    let mut data = vec![235u8, 235, 16, 16];
    data.extend_from_slice(&[128, 128]);
    let pixels = i420_to_bgra(&data, 2, 2).expect("a frame");
    assert_eq!(pixels, vec![0xffff_ffff, 0xffff_ffff, 0xff00_0000, 0xff00_0000]);
    assert_eq!(i420_to_bgra(&data[..3], 2, 2), None, "too few bytes is no picture");
    assert_eq!(i420_to_bgra(&data, 0, 0), None);
}

/// On a slot of its own, not the process's: a suite runs its tests side by
/// side and the real slot is one for the whole run.
#[test]
fn a_frame_lands_as_the_newest_of_its_side_and_the_end_of_a_call_clears_both() {
    let mut frames = Frames::default();
    frames.put(true, 1, 1, vec![0xffff_ffff]);
    frames.put(false, 1, 1, vec![0xff00_0000]);
    assert_eq!(frames.remote.as_ref().expect("the other side").stamp, 1);
    assert_eq!(frames.local.as_ref().expect("mine").pixels, vec![0xff00_0000]);
    frames.put(true, 1, 1, vec![0xff00_0000]);
    let newest = frames.remote.as_ref().expect("newer");
    assert_eq!((newest.stamp, newest.pixels[0]), (2, 0xff00_0000), "a draw tells them apart by the stamp");
    frames.forget();
    assert!(frames.remote.is_none() && frames.local.is_none());
}
