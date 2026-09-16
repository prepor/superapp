//! What can be proved without a camera, a microphone or a `Cx`.
//!
//! The half of this module that talks to the platform cannot be tested on a
//! build machine: there is no sky over a CI runner and no face in front of
//! it. What *can* be tested is everything between — the wish and the state
//! it leaves, the answers a refusal turns into, which camera is picked out
//! of a device list, and the pixels a frame becomes. So the state machine is
//! written as two halves, [`Senses::land`] and [`Senses::work`], neither of
//! which needs a `Cx`: one takes the platform's events, the other says what
//! the platform is now owed. Both are exercised here with events built by
//! hand.

use super::*;
use makepad_widgets::makepad_platform::permission::PermissionResult;

/// The state, for a test that wants to look inside.
fn state(senses: &Senses) -> std::sync::MutexGuard<'_, State> {
    senses.0.lock().expect("the senses")
}

fn fix_event(lat: f64, lon: f64, speed: Option<f64>) -> Event {
    Event::LocationUpdate(LocationUpdateEvent {
        lat,
        lon,
        accuracy_m: 9.0,
        altitude_m: Some(440.0),
        speed_mps: speed,
        heading_deg: Some(190.0),
        time: 1_770_000_000.0,
    })
}

fn permission(permission: Permission, status: PermissionStatus) -> Event {
    Event::PermissionResult(PermissionResult {
        permission,
        request_id: 1,
        status,
    })
}

/// A camera whose one format runs at `rate` — for the rate alone, every
/// other thing about the formats being equal.
fn camera_at(rates: &[f64]) -> Event {
    Event::VideoInputs(VideoInputsEvent {
        descs: vec![VideoInputDesc {
            input_id: VideoInputId(LiveId(1)),
            name: "Front Camera".to_string(),
            formats: rates
                .iter()
                .enumerate()
                .map(|(n, rate)| VideoFormat {
                    format_id: VideoFormatId(LiveId(n as u64)),
                    width: 640,
                    height: 480,
                    frame_rate: Some(*rate),
                    pixel_format: VideoPixelFormat::NV12,
                })
                .collect(),
            sensor_orientation: 0,
        }],
    })
}

/// Cameras mounted the way a Mac's are: upright, so their frames need no
/// turning at all.
fn camera_list(names: &[&str]) -> Event {
    camera_list_mounted(names, 0)
}

/// The same list with every sensor bolted `mounted` degrees round, which is
/// what says how far a frame of it has to be turned to stand upright.
fn camera_list_mounted(names: &[&str], mounted: u32) -> Event {
    Event::VideoInputs(VideoInputsEvent {
        descs: names
            .iter()
            .enumerate()
            .map(|(n, name)| VideoInputDesc {
                input_id: VideoInputId(LiveId(n as u64 + 1)),
                name: (*name).to_string(),
                sensor_orientation: mounted,
                formats: vec![
                    VideoFormat {
                        format_id: VideoFormatId(LiveId(100 + n as u64)),
                        width: 640,
                        height: 480,
                        frame_rate: Some(30.0),
                        pixel_format: VideoPixelFormat::NV12,
                    },
                    VideoFormat {
                        format_id: VideoFormatId(LiveId(200 + n as u64)),
                        width: 1280,
                        height: 720,
                        frame_rate: Some(30.0),
                        pixel_format: VideoPixelFormat::NV12,
                    },
                    // Past what a video message or a photograph wants, so
                    // it must not be the one picked.
                    VideoFormat {
                        format_id: VideoFormatId(LiveId(300 + n as u64)),
                        width: 3840,
                        height: 2160,
                        frame_rate: Some(30.0),
                        pixel_format: VideoPixelFormat::NV12,
                    },
                ],
            })
            .collect(),
    })
}

fn microphone_list(has_one: bool) -> Event {
    Event::AudioDevices(AudioDevicesEvent {
        descs: has_one
            .then(|| AudioDeviceDesc {
                device_id: AudioDeviceId(LiveId(7)),
                device_type: AudioDeviceType::Input,
                is_default: true,
                has_failed: false,
                channel_count: 1,
                name: "Built-in Microphone".to_string(),
            })
            .into_iter()
            .collect(),
    })
}

/// The receiver is asked for once, held while anybody wants it, and let go
/// by the last of them — a panel and a worker both hold it, and neither
/// knows about the other.
#[test]
fn the_receiver_is_held_until_the_last_caller_lets_go() {
    let senses = Senses::new();
    let (mut panel, _) = senses.capabilities();
    let (mut worker, _) = senses.capabilities();
    assert!(senses.work().ask.is_none(), "nothing wanted, nothing asked");

    panel.want().expect("the receiver");
    let work = senses.work();
    assert_eq!(work.ask, Some(Permission::Location));
    assert!(work.start_location);
    worker.want().expect("the receiver");
    let work = senses.work();
    assert!(work.ask.is_none(), "asked once a run");
    assert!(!work.start_location, "and started once");

    panel.release();
    assert!(!senses.work().stop_location, "the worker still wants it");
    worker.release();
    assert!(senses.work().stop_location);
    // A release too many is not a panic, and not a second stop.
    worker.release();
    assert!(!senses.work().stop_location);
}

/// A fix lands where the capability reads it, and the heading only comes
/// with movement.
#[test]
fn a_fix_lands_where_the_capability_reads_it() {
    let senses = Senses::new();
    let (location, _) = senses.capabilities();
    assert_eq!(location.fix(), None, "nothing until one arrives");

    assert!(senses.land(&fix_event(47.0472, 8.3164, Some(1.4))));
    let fix = location.fix().expect("a fix");
    assert!((fix.lat - 47.0472).abs() < 1e-9);
    assert_eq!(fix.accuracy_m, 9.0);
    assert_eq!(fix.heading_deg, Some(190.0), "moving, so it has a course");

    // Standing still: the course is the last direction walked, which a
    // live share must not draw an arrow from.
    senses.land(&fix_event(47.0472, 8.3164, Some(0.0)));
    assert_eq!(location.fix().expect("a fix").heading_deg, None);
    senses.land(&fix_event(47.0472, 8.3164, None));
    assert_eq!(location.fix().expect("a fix").heading_deg, None);

    // An event nothing here is about changes nothing.
    assert!(!senses.land(&Event::Signal));
}

/// A refusal is the capability's answer, in words with a way out in them,
/// and it is never asked for again.
#[test]
fn a_refusal_is_what_the_next_wish_answers() {
    let senses = Senses::new();
    let (mut location, mut capture) = senses.capabilities();

    senses.land(&Event::LocationError(LocationErrorEvent::PermissionDenied));
    let said = location.want().expect_err("refused");
    assert!(said.contains("your location is not allowed"), "{said}");
    assert!(said.contains("Settings"), "and where to go: {said}");
    assert!(
        senses.work().ask.is_none(),
        "a refusal is not retried on its own"
    );

    senses.land(&permission(Permission::Camera, PermissionStatus::DeniedPermanent));
    let said = capture.open_camera().expect_err("refused");
    assert!(said.contains("the camera is not allowed"), "{said}");
    senses.land(&permission(
        Permission::AudioInput,
        PermissionStatus::DeniedCanRetry,
    ));
    let dir = std::path::PathBuf::from("/tmp/superapp-senses-test");
    let said = capture.start_voice(&dir).expect_err("refused");
    assert!(said.contains("the microphone is not allowed"), "{said}");

    // Granted afterwards — in System Settings, say — clears it.
    senses.land(&permission(Permission::Camera, PermissionStatus::Granted));
    assert!(capture.open_camera().is_ok());

    // A platform with no device at all says so too.
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    senses.land(&camera_list(&[]));
    assert_eq!(
        capture.open_camera(),
        Err("this device has no camera".to_string())
    );
    senses.land(&microphone_list(false));
    assert_eq!(
        capture.start_voice(&dir),
        Err("this device has no microphone".to_string())
    );
}

/// The camera: a wish, then the enumeration it wakes, then the session —
/// and the front camera is the one a video message is recorded with.
#[test]
fn the_camera_opens_on_the_front_one_once_the_platform_has_listed_them() {
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    capture.open_camera().expect("the wish");
    assert_eq!(capture.camera(), None, "not until the platform answers");

    let work = senses.work();
    assert!(work.watch, "the frame callback wakes the enumeration");
    assert_eq!(work.ask, Some(Permission::Camera));
    assert!(work.open_camera.is_none(), "nothing to open yet");

    senses.land(&camera_list(&["Back Camera", "Front Camera"]));
    let work = senses.work();
    let (input, format) = work.open_camera.expect("a session");
    assert_eq!(input, VideoInputId(LiveId(2)), "the front one");
    assert_eq!(format, VideoFormatId(LiveId(201)), "720p, not the 4K mode");
    assert_eq!(
        capture.camera(),
        Some(CameraId {
            input: 2,
            format: 201,
            turns: 0,
            front: true
        })
    );
    assert!(!senses.work().watch, "registered once a run");

    capture.close_camera();
    let work = senses.work();
    assert!(work.close_camera);
    assert_eq!(capture.camera(), None);

    // With no front camera the first one is what there is.
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    capture.open_camera().expect("the wish");
    senses.land(&camera_list(&["USB Webcam"]));
    assert_eq!(
        senses.work().open_camera.map(|(i, _)| i),
        Some(VideoInputId(LiveId(1)))
    );
}

/// The microphone is opened by a recording and closed when the last one
/// stops — the meter is off in between.
#[test]
fn the_microphone_follows_the_recording() {
    let dir = std::env::temp_dir().join(format!("superapp-senses-{}", std::process::id()));
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    senses.land(&microphone_list(true));
    assert_eq!(capture.level(), 0.0);

    capture.start_voice(&dir).expect("a recording");
    let work = senses.work();
    assert!(work.listen);
    assert_eq!(work.ask, Some(Permission::AudioInput));
    assert_eq!(work.open_microphone, vec![AudioDeviceId(LiveId(7))]);
    assert!(
        capture.start_voice(&dir).is_err(),
        "one recording at a time"
    );

    // Thrown away: the microphone goes with it, and so does the file.
    capture.discard();
    assert!(state(&senses).voice.is_none());
    assert!(senses.work().close_microphone);
    assert_eq!(capture.level(), 0.0);
    let left: Vec<_> = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "a discarded recording leaves nothing: {left:?}");
    let _ = std::fs::remove_dir_all(&dir);

    // And a stop with nothing running is an answer, not a panic.
    assert!(capture.stop_voice().is_err());
    assert!(capture.stop_circle().is_err());
}

/// A device opened while the platform's dialog was still up hands over
/// nothing, and it does not start handing anything over when the person
/// finally says yes. So the grant closes what is open and opens it again —
/// which is the whole of why a first recording used to be silent.
#[test]
fn a_permission_granted_after_the_device_opened_reopens_it() {
    let dir = std::env::temp_dir().join(format!("superapp-grant-{}", std::process::id()));
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    senses.land(&microphone_list(true));

    // The input opens in the very pass that asks for the permission: the
    // devices are known, and the answer is a person's and comes later.
    capture.start_voice(&dir).expect("a recording");
    let work = senses.work();
    assert_eq!(work.ask, Some(Permission::AudioInput));
    assert_eq!(work.open_microphone, vec![AudioDeviceId(LiveId(7))]);
    assert!(senses.work().open_microphone.is_empty(), "opened once");

    senses.land(&permission(Permission::AudioInput, PermissionStatus::Granted));
    let work = senses.work();
    assert!(work.close_microphone, "the session that heard nothing goes");
    assert_eq!(
        work.open_microphone,
        vec![AudioDeviceId(LiveId(7))],
        "and a fresh one opens behind it, in that order"
    );
    assert!(!senses.work().close_microphone, "once, not on every pass");
    capture.discard();
    let _ = std::fs::remove_dir_all(&dir);

    // The camera is the same story, on the same camera.
    capture.open_camera().expect("the wish");
    senses.land(&camera_list(&["Front Camera"]));
    let opened = senses.work().open_camera.expect("a session");
    senses.land(&permission(Permission::Camera, PermissionStatus::Granted));
    let work = senses.work();
    assert!(work.close_camera);
    assert_eq!(work.open_camera, Some(opened), "the same camera, opened again");
    assert!(!senses.work().close_camera);
}

/// A call asks for the microphone and opens nothing: the call library holds
/// the device itself, where this capability cannot see it, so without this
/// the permission would never be asked for at all.
#[test]
fn a_call_asks_for_the_microphone_without_opening_it() {
    let dir = std::env::temp_dir().join(format!("superapp-ask-{}", std::process::id()));
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    senses.land(&microphone_list(true));

    capture.ask_microphone();
    let work = senses.work();
    assert_eq!(work.ask, Some(Permission::AudioInput));
    assert!(work.open_microphone.is_empty(), "no input is opened");
    assert!(!work.listen, "and no callback is registered for one");
    capture.ask_microphone();
    assert!(senses.work().ask.is_none(), "asked once a run");

    // A recording afterwards opens the input and asks nothing: the platform
    // has already answered.
    capture.start_voice(&dir).expect("a recording");
    let work = senses.work();
    assert!(work.ask.is_none());
    assert_eq!(work.open_microphone, vec![AudioDeviceId(LiveId(7))]);
    capture.discard();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The camera is held rather than flagged: two callers want the picture at
/// once — an attach panel photographing and a video call sending — and the
/// one that lets go is not the one that closes it.
#[test]
fn the_camera_stays_open_while_anybody_is_holding_it() {
    let senses = Senses::new();
    let (_, mut panel) = senses.capabilities();
    let (_, mut call) = senses.capabilities();
    panel.open_camera().expect("the wish");
    call.open_camera().expect("the same camera");
    senses.land(&camera_list(&["Front Camera"]));
    assert!(senses.work().open_camera.is_some());

    call.close_camera();
    assert!(!senses.work().close_camera, "the panel is still looking through it");
    assert!(panel.camera_open());
    panel.close_camera();
    assert!(senses.work().close_camera, "the last one out closes it");
    assert!(!panel.camera_open());
}

/// A video message needs the camera first: the recording is refused until
/// there is a picture to record.
#[test]
fn a_video_message_waits_for_the_camera() {
    let dir = std::env::temp_dir().join(format!("superapp-circle-{}", std::process::id()));
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    senses.land(&microphone_list(true));
    assert!(capture.start_circle(&dir).is_err(), "no camera yet");

    capture.open_camera().expect("the wish");
    senses.land(&camera_list(&["Front Camera"]));
    senses.work();
    assert!(capture.camera().is_some());
    // Started for real: the encoder is the platform's, and what it makes of
    // it is Andrey's to see on the glass. What is proved here is that the
    // recording is *allowed* to start, and that a photograph is refused
    // until a frame has actually arrived.
    assert!(
        capture.take_photo(&dir).is_err(),
        "no frame has arrived yet"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A frame cropped to its centre square and scaled: the square is the
/// short side, and the picture in it is the picture that was in the middle.
#[test]
fn a_frame_is_cropped_to_its_centre_square() {
    // A 16-wide, 8-tall frame whose middle eight columns are white and
    // whose edges are black, so a centre crop is all white.
    let (w, h) = (16usize, 8usize);
    let mut frame = CameraFrameOwned {
        width: w,
        height: h,
        layout: CameraFrameLayout::I420,
        plane_count: 3,
        ..CameraFrameOwned::default()
    };
    frame.planes[0].bytes = (0..w * h)
        .map(|i| if (4..12).contains(&(i % w)) { 235 } else { 16 })
        .collect();
    frame.planes[1].bytes = vec![128; (w / 2) * (h / 2)];
    frame.planes[2].bytes = vec![128; (w / 2) * (h / 2)];

    let square = square_nv12(&frame, 4);
    assert_eq!(square.len(), 4 * 4 * 3 / 2);
    assert!(
        square[..16].iter().all(|y| *y == 235),
        "the middle is what was kept: {:?}",
        &square[..16]
    );
    assert!(square[16..].iter().all(|uv| *uv == 128), "grey chroma");

    // A frame in a layout nothing here understands is an even grey rather
    // than a picture a decoder chokes on.
    let empty = CameraFrameOwned::default();
    let square = square_nv12(&empty, 4);
    assert!(square.iter().all(|b| *b == 128));
}

/// The colour conversion, at the three points that matter.
#[test]
fn the_pixels_come_out_where_they_went_in() {
    assert_eq!(yuv_to_rgb(16, 128, 128), (0, 0, 0), "black");
    let (r, g, b) = yuv_to_rgb(235, 128, 128);
    assert!(r > 250 && g > 250 && b > 250, "white: {r} {g} {b}");
    let (r, g, b) = yuv_to_rgb(82, 90, 240);
    assert!(r > 200 && g < 60 && b < 60, "red: {r} {g} {b}");

    // And a whole frame, through the path a photograph takes.
    let frame = Frame {
        width: 2,
        height: 2,
        turns: 0,
        mirror: false,
        y: vec![235, 235, 235, 235],
        u: vec![128],
        v: vec![128],
    };
    let rgb = rgb_of(&frame);
    assert_eq!(rgb.len(), 12);
    assert!(rgb.iter().all(|c| *c > 250));

    let nv12 = {
        let mut f = vec![235u8; 4];
        f.extend_from_slice(&[128, 128]);
        f
    };
    let rgb = rgb_of_nv12(&nv12, 2);
    assert_eq!(rgb.len(), 12);
    assert!(rgb.iter().all(|c| *c > 250));
}

/// The format a camera is opened on is the one nearest thirty frames a
/// second — the rate a video message is written at. A camera that answers
/// sixty or twenty-four gives a picture that runs against its own sound
/// where the file's rate is simply assumed.
#[test]
fn the_camera_is_opened_at_the_rate_a_video_message_is_written_at() {
    // Thirty exactly, out of a list that has it.
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    capture.open_camera().expect("the wish");
    senses.land(&camera_at(&[60.0, 30.0, 15.0]));
    assert_eq!(
        senses.work().open_camera.map(|(_, f)| f),
        Some(VideoFormatId(LiveId(1)))
    );

    // And the nearest where it does not: twenty-four is six frames away,
    // sixty is thirty.
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    capture.open_camera().expect("the wish");
    senses.land(&camera_at(&[60.0, 24.0]));
    assert_eq!(
        senses.work().open_camera.map(|(_, f)| f),
        Some(VideoFormatId(LiveId(1)))
    );

    // The size is what decides between two formats at the same rate, which
    // is what the platform's own list usually is.
    assert_eq!(off_wanted(Some(30.0)), 0);
    assert_eq!(off_wanted(None), 0, "a format that does not say is taken as thirty");
    assert!(off_wanted(Some(29.97)) < off_wanted(Some(25.0)));
}

/// How long a video message runs is measured off the clock the frames
/// arrived on, not off a count divided by the rate the file declares.
#[test]
fn a_video_message_is_as_long_as_the_clock_says_it_ran() {
    // Two seconds at thirty frames: sixty-one moments, the last of which
    // gets its own thirtieth of a second on the screen.
    let secs = circle_secs(2.0, 61);
    assert!((secs - (2.0 + 1.0 / 30.0)).abs() < 1e-9, "{secs}");

    // The same two seconds from a camera that answered twenty-four: the
    // length is the two seconds it ran, not 49/30 of a second.
    let slow = circle_secs(2.0, 49);
    assert!((slow - (2.0 + 2.0 / 48.0)).abs() < 1e-9, "{slow}");
    assert!((slow - secs).abs() < 0.03, "the same recording is the same length");

    // One frame is one frame's worth, and no frames at all is nothing.
    assert!((circle_secs(0.0, 1) - 1.0 / 30.0).abs() < 1e-9);
    assert!((circle_secs(0.0, 0) - 1.0 / 30.0).abs() < 1e-9);
}

/// Two permissions wanted in the one pass go out one after the other.
/// Android cancels the second dialog of a pair while the first is still
/// standing and answers the cancelled one with nothing at all — so a first
/// video call, which wants the microphone and the camera together, would
/// leave one of the two unanswered for the rest of the run.
#[test]
fn two_permissions_are_asked_for_one_after_the_other() {
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();

    // What a video call wants the moment it appears: the microphone, for
    // the engine's own capture, and the camera for the picture.
    capture.ask_microphone();
    capture.open_camera().expect("the wish");
    assert_eq!(senses.work().ask, Some(Permission::AudioInput), "one dialog");
    assert!(
        senses.work().ask.is_none(),
        "and nothing else while it is up: a second would be cancelled"
    );

    // The first one's answer is what lets the next one out — and it is the
    // answer that does it, not another pass.
    senses.land(&permission(Permission::AudioInput, PermissionStatus::Granted));
    assert_eq!(senses.work().ask, Some(Permission::Camera));
    assert!(senses.work().ask.is_none(), "nothing is waiting behind it");
    senses.land(&permission(Permission::Camera, PermissionStatus::Granted));
    assert!(senses.work().ask.is_none(), "and both were asked once");
}

/// What a call reads before it starts its engine: the microphone's
/// permission, unanswered while the dialog is still up. The engine captures
/// through its own library, and a capture opened under an open dialog hands
/// over silence for the whole of the call.
#[test]
fn the_microphones_permission_is_unanswered_until_the_platform_answers() {
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    assert_eq!(
        capture.microphone_allowed(),
        Some(true),
        "a platform this build never asks is a platform that allows it"
    );

    capture.ask_microphone();
    assert_eq!(capture.microphone_allowed(), None, "the wish alone answers nothing");
    assert_eq!(senses.work().ask, Some(Permission::AudioInput));
    assert_eq!(capture.microphone_allowed(), None, "and the dialog is open");

    senses.land(&permission(Permission::AudioInput, PermissionStatus::Granted));
    assert_eq!(capture.microphone_allowed(), Some(true));

    // Refused, in either of the platforms' two words.
    for status in [PermissionStatus::DeniedCanRetry, PermissionStatus::DeniedPermanent] {
        let senses = Senses::new();
        let (_, mut capture) = senses.capabilities();
        capture.ask_microphone();
        senses.land(&permission(Permission::AudioInput, status));
        assert_eq!(capture.microphone_allowed(), Some(false));
    }
}

/// A refusal remembered for the rest of the run is every later call
/// carrying no voice out. The person may go to the platform's own settings
/// and allow it, and neither platform says a word when they do — so a wish
/// that meets a remembered refusal *looks* instead: makepad's
/// `check_permission`, which reads what the platform already knows, answers
/// with the same result event and raises no dialog on either side.
#[test]
fn a_refused_microphone_is_looked_at_again_without_a_dialog() {
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();

    // The one real request of the run, and android's answer to a first
    // refusal: denied, and ask me again.
    capture.ask_microphone();
    assert_eq!(senses.work().ask, Some(Permission::AudioInput));
    senses.land(&permission(
        Permission::AudioInput,
        PermissionStatus::DeniedCanRetry,
    ));
    assert_eq!(capture.microphone_allowed(), Some(false));

    // A call already carrying without a voice only looks: a dialog over a
    // running call is not a thing a poll may raise.
    capture.recheck_microphone();
    let work = senses.work();
    assert!(work.ask.is_none(), "no dialog while a call is running");
    assert_eq!(work.check, Some(Permission::AudioInput), "a look instead");

    // Which is what android answers for a permission it will not ask about
    // again — and that is no answer at all. The refusal stands.
    senses.land(&permission(
        Permission::AudioInput,
        PermissionStatus::NotDetermined,
    ));
    assert_eq!(capture.microphone_allowed(), Some(false), "still refused");

    // A call appearing gets the second dialog the platform is still
    // willing to raise, and gets it once.
    capture.ask_microphone();
    assert_eq!(
        senses.work().ask,
        Some(Permission::AudioInput),
        "the second dialog, which android has one of"
    );
    senses.land(&permission(
        Permission::AudioInput,
        PermissionStatus::DeniedPermanent,
    ));
    capture.ask_microphone();
    let work = senses.work();
    assert!(work.ask.is_none(), "and no third");
    assert_eq!(work.check, Some(Permission::AudioInput));

    // Allowed in the settings panel while nothing was listening: the check
    // is what finds it, and the next call carries a voice.
    senses.land(&permission(Permission::AudioInput, PermissionStatus::Granted));
    assert_eq!(capture.microphone_allowed(), Some(true));
    let dir = std::env::temp_dir().join(format!("superapp-recheck-{}", std::process::id()));
    capture.start_voice(&dir).expect("a recording");
    capture.discard();
    let _ = std::fs::remove_dir_all(&dir);

    // And one look at a time, like one dialog at a time: a poll that ran
    // while the answer was still coming would stack up behind it.
    capture.recheck_microphone();
    assert!(senses.work().check.is_none(), "a grant is not looked at again");
}

/// The camera's remembered refusal is looked at the same way, by the wish
/// that meets it. The wish itself still fails — a refusal is this moment's
/// answer — and the check is what makes the next one succeed.
#[test]
fn a_refused_camera_is_looked_at_again_by_the_next_wish() {
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();

    capture.open_camera().expect("the wish");
    assert_eq!(senses.work().ask, Some(Permission::Camera));
    senses.land(&permission(Permission::Camera, PermissionStatus::DeniedPermanent));
    let said = capture.open_camera().expect_err("refused");
    assert!(said.contains("the camera is not allowed"), "{said}");
    let work = senses.work();
    assert!(work.ask.is_none(), "the dialog is not raised twice");
    assert_eq!(work.check, Some(Permission::Camera), "it is looked at instead");

    senses.land(&permission(Permission::Camera, PermissionStatus::Granted));
    assert!(capture.open_camera().is_ok(), "and the next wish is the picture");
    assert!(senses.work().check.is_none(), "nothing left to look at");
}

/// What is wrong with the receiver is answered on every draw, not once at
/// the panel's opening: the platform's dialog is answered *after* the wish
/// was made, so a refusal arrives with the panel already up — and a place
/// panel that never heard it would leave *finding you…* standing for good.
#[test]
fn what_is_wrong_with_the_receiver_is_answered_after_the_wish() {
    let senses = Senses::new();
    let (mut location, _) = senses.capabilities();
    assert!(location.trouble().is_none(), "nothing is wrong yet");

    location.want().expect("the receiver");
    senses.land(&Event::LocationError(LocationErrorEvent::PermissionDenied));
    let said = location.trouble().expect("a refusal");
    assert!(said.contains("your location is not allowed"), "{said}");
    assert!(said.contains("Settings"), "and where to go: {said}");

    // The platform's own answer arrives the same way, and a fix landing
    // afterwards is the receiver working again.
    let senses = Senses::new();
    let (mut location, _) = senses.capabilities();
    location.want().expect("the receiver");
    senses.land(&permission(Permission::Location, PermissionStatus::DeniedPermanent));
    let said = location.trouble().expect("a refusal");
    assert!(said.contains("your location is not allowed"), "{said}");
    senses.land(&fix_event(47.0472, 8.3164, None));
    assert!(location.trouble().is_none(), "a reading is nothing being wrong");
}

/// Which way up a frame is, worked out rather than assumed: the sensor's
/// own mounting, which way the lens faces, and how the screen is turned at
/// that moment. The rule is CameraX's, and a phone whose picture arrived
/// sideways is a phone where one of the three was left out.
#[test]
fn a_frames_turn_is_the_sensors_mounting_against_the_screens_own() {
    // The phone held the way phones are held: the sensor's mounting is the
    // whole of it.
    assert_eq!(upright_turns(90, false, 0), 1, "a back camera bolted a quarter round");
    assert_eq!(upright_turns(270, true, 0), 3, "and a front one three quarters");

    // Turned once anticlockwise — the phone on its side. The screen's turn
    // counts *against* a back camera's and *with* a front one's, because a
    // front camera's picture is already reversed left for right.
    assert_eq!(upright_turns(90, false, 1), 0);
    assert_eq!(upright_turns(270, true, 1), 0);
    assert_eq!(upright_turns(270, true, 3), 2, "and three quarters round the other way");

    // A sensor mounted upright — a Mac's, whose window never turns — is no
    // turn at all, whichever way the lens faces.
    assert_eq!(upright_turns(0, false, 0), 0);
    assert_eq!(upright_turns(0, true, 0), 0);
}

/// A plane turned a quarter at a time, and four quarters back where it
/// started. An odd turn stands it on its side, so the sides swap; a chroma
/// plane of two-byte samples moves the pair rather than the bytes.
#[test]
fn a_plane_turns_a_quarter_at_a_time_and_comes_back_in_four() {
    // Three wide, two tall, every byte its own:
    //   1 2 3
    //   4 5 6
    let (w, h) = (3usize, 2usize);
    let plane = vec![1u8, 2, 3, 4, 5, 6];

    // A quarter clockwise: the first row goes down the last column, and
    // what comes back is two wide and three tall.
    assert_eq!(turn_plane(&plane, w, h, 1, 1), vec![4, 1, 5, 2, 6, 3]);
    // A half: end for end, the same shape it went in.
    assert_eq!(turn_plane(&plane, w, h, 1, 2), vec![6, 5, 4, 3, 2, 1]);
    // And three quarters, which is a quarter the other way.
    assert_eq!(turn_plane(&plane, w, h, 1, 3), vec![3, 6, 2, 5, 1, 4]);
    // Four is where it started.
    assert_eq!(turn_plane(&plane, w, h, 1, 4), plane);
    assert_eq!(turn_plane(&plane, w, h, 1, 0), plane);

    // The same plane read as two-byte samples — an NV12 chroma plane, U and
    // V side by side. Three wide, one tall, turned a quarter: one wide,
    // three tall, and each pair still a pair.
    assert_eq!(turn_plane(&plane, 3, 1, 2, 1), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(turn_plane(&plane, 3, 1, 2, 2), vec![5, 6, 3, 4, 1, 2]);
}

/// Left for right, which is what makes a front camera's picture the one the
/// person was looking at.
#[test]
fn a_plane_mirrored_is_left_for_right() {
    let mut plane = vec![1u8, 2, 3];
    mirror_plane(&mut plane, 3, 1, 1);
    assert_eq!(plane, vec![3, 2, 1]);

    // And by the sample, not by the byte: three two-byte samples turn round
    // with their pairs intact.
    let mut pairs = vec![1u8, 2, 3, 4, 5, 6];
    mirror_plane(&mut pairs, 3, 1, 2);
    assert_eq!(pairs, vec![5, 6, 3, 4, 1, 2]);
}

/// A photograph is written the way the person saw it, not the way the
/// sensor lay: a frame kept on its side comes out of `take_photo` with its
/// sides swapped, and the JPEG is a real one.
#[test]
fn a_photograph_of_a_frame_on_its_side_comes_out_upright() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("superapp-upright-{stamp}"));
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();

    // Four wide and two tall as the sensor made it, a quarter turn from
    // upright — which is every phone's front camera held in the hand.
    state(&senses).frame = Some(Frame {
        width: 4,
        height: 2,
        turns: 1,
        mirror: false,
        y: vec![16, 60, 120, 180, 200, 235, 90, 40],
        u: vec![128, 128],
        v: vec![128, 128],
    });

    let photo = capture.take_photo(&dir).expect("a shot");
    assert_eq!(
        (photo.width, photo.height),
        (2, 4),
        "the sides swapped: what was lying down is standing up"
    );
    let bytes = std::fs::read(&photo.path).expect("the file");
    assert_eq!(&bytes[..2], b"\xff\xd8", "a JPEG");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The turn follows the screen. The sensor is bolted where it is bolted,
/// but a phone turned in the hand is a preview that has to be turned with
/// it — so the camera's own turn is worked out again rather than kept from
/// the moment the session opened.
#[test]
fn the_cameras_turn_follows_the_screen() {
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    capture.open_camera().expect("the wish");
    senses.land(&camera_list_mounted(&["Back Camera", "Front Camera"], 270));
    assert!(senses.work().open_camera.is_some(), "a session");

    let camera = capture.camera().expect("a camera");
    assert!(camera.front, "the front one, as a video message wants");
    assert_eq!(camera.turns, 3, "three quarters, the phone held upright");

    // Turned once anticlockwise in the hand: a front camera's turn goes
    // *with* the screen's, and three quarters and one more is none at all.
    {
        let mut s = state(&senses);
        s.screen_turns = 1;
        assert!(s.stand(), "the turn moved, so the preview is redrawn");
    }
    assert_eq!(capture.camera().expect("a camera").turns, 0);
    assert!(!state(&senses).stand(), "and standing it again moves nothing");
}

/// A phone turned end over end changes no geometry and raises no
/// configuration change, so the screen is asked on a clock while a camera
/// is open. The clock is the phone's alone — a Mac's window never turns —
/// and it is taken back by the very event it fires, so that `work` sets the
/// next one going only while the camera is still open.
#[test]
fn the_screen_is_asked_on_a_clock_that_fires_once_and_is_set_again() {
    let senses = Senses::new();
    let (_, mut capture) = senses.capabilities();
    capture.open_camera().expect("the wish");
    senses.land(&camera_list(&["Front Camera"]));
    assert!(senses.work().open_camera.is_some(), "a session");
    assert_eq!(
        senses.work().poll,
        cfg!(target_os = "android"),
        "the clock is the phone's alone"
    );

    // The clock going off, and only this clock: another timer's event is
    // somebody else's.
    state(&senses).poll = Some(Timer(7));
    let other = Event::Timer(TimerEvent { time: None, timer_id: 8 });
    assert!(!senses.polled(&other), "not this clock");
    assert!(state(&senses).poll.is_some(), "and it is still set");
    let fired = Event::Timer(TimerEvent { time: None, timer_id: 7 });
    assert!(senses.polled(&fired));
    assert!(state(&senses).poll.is_none(), "fired once, and taken back");
    assert!(!senses.polled(&fired), "a clock taken back fires no more");
}
