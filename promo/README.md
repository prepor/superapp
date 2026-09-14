# Superapp promo production

The [revised shooting script](../docs/promo-video.md) describes the 90-second film.
The current video is `.context/promo/superapp-v3.mp4`; open
`.context/promo/review-v3.html` for chapters. Generated captures, demo stores,
WAV stems, and review reports remain under `.context/promo/` and outside Git.
The original first cut remains available separately.

## Revised edit

The third cut reuses the accepted v2 speed, agents, sync, audio and unchanged
app views. New shots add calendar events, a Telegram photo and viewer, launcher
search, filtering, previews, multiple selection and a complete Overview sequence.
`gesture-ms` spreads scripted touch input over real animation frames; the review
checks the tile's travel as well as native capture cadence.

```sh
python3 promo/build_capture.py
python3 promo/shoot_v3.py heroes
python3 promo/shoot_v3.py launch
python3 promo/shoot_v3.py mail
python3 promo/shoot_v3.py files
python3 promo/phone_v3.py
python3 promo/review_v3.py
python3 promo/render_v3.py --stills
python3 promo/render_v3.py
python3 promo/review_v3.py --encoded
```

The desktop comparison is captured with `shoot_v3.take('android-desktop',
[[('workshop_workspaces',[])],[('telegram-chat',['11'])]], window='1100x735',
grid='8x6')`. The generated sample photo and exact prompt live in
`.context/promo/v3/assets/`; all other imagery is the app's own rendering.
The real photo viewer uses an isolated normal instance with Mail and Calendar
disabled, reading only the fixture's cached attachment. Scripted takes use fakes.
Keep the accepted v2 assets in place for `render_v3.py`.

`render_v2.py` contains complete viewports, never guessed panel crops or cover
crops. It renders at 1920×1080/60, with a name for every opening app and a fixed
camera rectangle across the userspace-OS / One-UI transition. The final card is
only **superapp**.

Prepared takes must be archived before recapture: scripts refuse to overwrite
an existing demo database. The revised takes use the prepared fixture baseline
at `.context/promo/v2/base.db`, plus the accepted live-agent result stores.

```sh
python3 promo/build_capture.py
python3 promo/shoot_v2.py stills
MAKEPAD_CAPTURE_BMP=1 python3 promo/shoot_v2.py choreography
MAKEPAD_CAPTURE_BMP=1 python3 promo/shoot_v2.py speed
python3 promo/live_meeting_v2.py
python3 promo/live_language.py --bike
python3 promo/results_v2.py lists
python3 promo/results_v2.py meeting
python3 promo/results_v2.py language
python3 promo/results_v2.py agent
MAKEPAD_CAPTURE_BMP=1 python3 promo/phone_v2.py
MAKEPAD_CAPTURE_BMP=1 python3 promo/shoot_v2.py android-desktop
python3 promo/sync_v2.py
PYTHONPATH=promo python3 -c 'from score import compose; compose(".context/promo/v2/score.wav", revision=2)'
python3 promo/review_v2.py
python3 promo/render_v2.py --stills
python3 promo/render_v2.py
python3 promo/review_v2.py --encoded
```

The native GPU recorder logs monotonic draw timestamps in `frames/frames.csv`.
It writes real frames while animation retains wall-clock timing. The raw
capture build uses a private copy of the pinned Makepad checkout under
`.context/promo/cargo-home`; it leaves the shared Cargo checkout untouched.
Its small [capture patch](makepad-capture.patch) routes file captures to lossless
BMP without PNG compression in Metal’s completion callback. HTTP screenshots,
studio captures, rendering, and animation behavior are unchanged. No optical
flow, virtual clock, or invented intermediate frames are used.

A nominal 60 fps file is insufficient validation. Inspect consecutive source
image content during motion, source timing, action visibility, and the encoded
video. `review-report.json` records the accepted cut’s checks. Rejected takes
are retained with descriptive suffixes.

The meeting and German-lesson results are real model/tool runs over fictional
records. The German question directly translates the phrase in **Berlin by
bike**; its answer capture actually selects the correct choice. Generation
waits are cut. The phone recording is explicitly labelled as a Mac phone-layout
preview because the physical device was unavailable. Sync is actual
bidirectional replication between two local stores, preserving their relative
timing; it does not claim Android or network latency.

## Original first-cut utilities

The commands below document the earlier fixture bootstrap and capture tools.
The revised renderer and shooting script above supersede the first-cut framing.

## Capture and render

The scripts use macOS, the project's mise toolchain, FFmpeg, Python with Pillow
and NumPy, and Swift for finding the app's own window. No video-generation or
stock-music service is required. The Android device needs USB debugging and an
unlocked screen with Superapp available.

From the repository root:

```sh
mkdir -p .context/promo/bin
MAKEPAD=headless mise exec -- cargo build -p superapp --no-default-features
cp target/debug/superapp .context/promo/bin/superapp-headless
python3 promo/capture.py --validate
python3 promo/capture.py

mise exec -- cargo build -p superapp --no-default-features
swiftc promo/window_id.swift -o .context/promo/bin/window-id
python3 promo/native_capture.py speed
python3 promo/native_capture.py choreography

python3 promo/score.py
python3 promo/render.py --stills
python3 promo/render.py --url github.com/prepor/superapp
```

The public URL is an explicit renderer argument. Its default is empty until
the launch URL is confirmed. `--width 1280 --fps 30` makes a smaller review
export; the default is 1920×1080 at 60 fps. Rendered previews are in
`film-contact-sheet.jpg` and `preview-*.jpg`.

`capture.py --only mail calendar` captures a subset. Each capture directory
contains its replay script, log, screenshots, individual rendered frames, and
a provenance manifest. The demo stores and file capability use fixtures. A
headless capture renders genuine app widgets and shaders but does not measure
runtime performance.

`native_capture.py` starts a separate demo window and records that window only.
Its speed scene opens 32 additional populated mail panels and moves a note
through the workspace. It preserves real playback speed. Earlier takes are
retained when a new take is recorded. The choreography take logs actual shortcut
times for its on-screen key labels.

All native capture utilities keep the window behind other Mac windows, inject
input through Makepad's process-local bridge, and set
`MAKEPAD_PRESENT_WHEN_OCCLUDED=1` so a fully covered window continues presenting
frames. `remote_session.py` also checks that its process never becomes the
foreground app after a control command. No global keyboard events are used.

## Live agent rehearsal

`live_agent.py` is a rehearsal utility for the meeting scenario. It expects a
fresh fixture database at `.context/promo/live-agent/demo.db` and uses the
normal app's configured gateway. It edits only that dedicated demo database.
Never point it at the everyday store. A run can incur the normal model cost.

Create the rehearsal database before its first run:

```sh
mkdir -p .context/promo/live-agent
printf 'wait 200\nquit\n' > .context/promo/live-agent/seed.txt
mise exec -- .context/promo/bin/superapp-headless \
  --e2e .context/promo/live-agent/seed.txt \
  --db .context/promo/live-agent/demo.db --no-draw --draws 300
python3 promo/live_agent.py
python3 promo/capture_result.py note
python3 promo/live_language.py
python3 promo/capture_result.py lesson
python3 promo/native_sync.py
```

Inspect `result.json`, the screenshots, and the actual tool-call records before
using the outcome in the film. A run reporting `done` is insufficient by itself:
it must have produced the intended note from the intended sources.

The meeting take reads calendar, cached mail, and Telegram messages, then creates
the note. Its original run also attempted to read a fixture attachment that had
no credential; the source recording and note retain that limitation. The result
shot opens the saved note explicitly. The language take reads an RSS article,
checks the learner's due items, and authors five exercises plus two vocabulary
cards through the real gateway. Its result capture answers the first exercise.
The edit cuts model processing waits; it makes no model-speed claim.

`native_sync.py` records two native Mac instances with separate stores, paired
through the app over loopback. One uses phone dimensions. It verifies the same
note UID and two-device rosters in both stores, and edits crossing both ways.
The two video streams run at their original speed. This is a local sync demo;
it makes no claim about physical Android rendering or network latency. Archive
the prior `sync-a` and `sync-b` folders before another take. The language utility
likewise requires archiving its previous take before rerunning.

The phone's source clip belongs at
`.context/promo/native/android/recording.mp4`. Capture only the prepared demo
workspace. The physical Mac-to-phone sync shot needs two simultaneous recordings
preserving their relative timing. Until those are available, the first cut
labels the phone layout preview and the local sync take inside the video.

## Music

`score.py` generates an original 120 BPM arrangement with percussion, bass,
plucked synths, pads, movement sounds, and a two-note ending. It writes the
combined score plus separate music and effects stems as 48 kHz stereo WAVs.
The renderer normalizes the mix to −16 LUFS with a −1.5 dBTP ceiling and encodes
AAC audio. The rendered video uses H.264 and fast-start MP4 metadata.
