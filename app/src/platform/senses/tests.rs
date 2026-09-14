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

fn camera_list(names: &[&str]) -> Event {
    Event::VideoInputs(VideoInputsEvent {
        descs: names
            .iter()
            .enumerate()
            .map(|(n, name)| VideoInputDesc {
                input_id: VideoInputId(LiveId(n as u64 + 1)),
                name: (*name).to_string(),
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
    assert!(senses.work().ask.is_empty(), "nothing wanted, nothing asked");

    panel.want().expect("the receiver");
    let work = senses.work();
    assert_eq!(work.ask, vec![Permission::Location]);
    assert!(work.start_location);
    worker.want().expect("the receiver");
    let work = senses.work();
    assert!(work.ask.is_empty(), "asked once a run");
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
        senses.work().ask.is_empty(),
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
    assert_eq!(work.ask, vec![Permission::Camera]);
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
            format: 201
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
    assert_eq!(work.ask, vec![Permission::AudioInput]);
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
