# CR-020 · Telegram: places, calls, the camera and the microphone

**Status:** in progress (Andrey, 2026-09-14: "location (current and live)
sharing (and proper rendering! it should also allow to open it in google
maps); calls (audio and video) support; photo from camera sharing; support
of recording audio and video circles; check reference app(s) about how it
was implemented in original apps" — then, on the three questions: calls on
both platforms now; voice notes heard on the Mac, in; the camera shot goes
the way the reference app takes it). Written as the book should read once
the whole change has landed, with the phases and the open decisions at the
end. Built by Opus agents, one phase each, validated before the next.

## Why

[CR-012](./cr-012-telegram.md) drew the four ways a message is *made*
rather than typed — a place, a voice note, a video message, a picture —
and [CR-013](./cr-013-telegram-client.md) put a real account behind
everything but them. Today each is a toast in a live account: *recording
is not available yet*, *location sharing is not available yet; the map
shows a demo location*. The map under a place — sent or received — is a
drawn street grid (`map::FakeTiles`), the same for Lucerne and Moscow.
And a call, in or out, is nothing: `updateCall` is dropped in silence and a
`messageCall` line reads *Call*.

The makepad this build pins has grown what CR-012 waited for: the
platform's location (`Cx::start_location_updates`, CoreLocation and
`LocationManager`), its camera (`use_video_input`, a `Video` widget with a
camera source), its microphone (`audio_input`), the permission dialogs
(`request_permission`), and on macOS an mp4 encoder (`makepad_video`,
AVAssetWriter: H.264 and AAC). So the senses are the platform's; what is
ours is the capability each is reached through, the fakes a suite runs on,
the encodings Telegram wants, and the panels.

## The reference

The official clients — `overtake/TelegramSwift` for the Mac and
`DrKLO/Telegram` for the phone — were read for what each flow *does*, not
how it looks (file pointers in the review notes at the end). What is kept
is the information and the rules; the look is the book's. Where the two
clients differ, the phone's flow is the one followed, because the Mac's
leaves three of the five to the system.

- **A place.** Both clients send the device's fix as a location. Only the
  phone shares it *live*: 15 minutes, an hour, 8 hours, or until turned
  off (`0x7FFFFFFF`). A service ticks every second and edits the message
  with the new fix when it moved more than a metre and the last edit is
  older than ten seconds, with the heading; stopping is an edit with the
  location gone, never a delete; at the period's end nothing is sent. On
  a restart the phone re-registers its shares from a local table. A
  received location is a map snapshot at zoom 15 — 320×120 on the Mac,
  the bubble's width by 195 dp on the phone — with a pin; a live one adds
  who, *updated 2 min ago* and a ring of the time left. Opening it: the
  Mac's client opens **Google Maps on the web**
  (`https://maps.google.com/maps?q=<lat>,<lon>`), the phone a `geo:` intent.
- **A voice note.** Press and hold; a blinking red dot, a timer to the
  hundredth, the level as a blob; slide away to cancel, up to lock. At
  least half a second, no upper bound. Encoded as Opus in Ogg, 48 kHz
  mono, 20 ms frames, ~30 kbps. The *waveform* is 100 five-bit values
  packed into 63 bytes: the recording in 100 slices, each slice's peak
  scaled against 1.8× the mean of the peaks (floored at 2500). Sent as a
  document with the *voice* attribute, duration and waveform — the
  wire's `inputMessageVoiceNote`.
- **A video message.** The same button switched to the camera: a round
  preview from the front camera, at most 60 seconds, a square mp4 —
  H.264 at 1000 kbps, 384 px (the phone; 300 on the Mac), 30 fps, a
  keyframe a second — with AAC mono (48 kHz at 64 kbps on the phone), and
  a 320×320 JPEG thumbnail. The phone flips cameras mid-recording; the
  Mac picks the front one. Sent as a document with *round video* set,
  its side, and the duration — `inputMessageVideoNote`.
- **A photo.** The Mac's client has no camera of its own: *Picture* is
  the OS's `IKPictureTaker` sheet. The phone's attach sheet has a camera
  cell: shutter, flip, flash, zoom, tap to focus; **nothing sends at the
  shutter** — the shot goes to a strip, later shots append to it, the
  strip opens as a preview with a caption field, and *send* sends them
  as one album. The picture goes at most 1280 px on the long side, JPEG
  quality 80, with a 90 px thumbnail.
- **A call.** From the peer's card or the transcript's call line.
  States: *contacting*, *waiting*, *ringing*, *exchanging encryption
  keys*, *connecting*, *reconnecting*, the timer, *line busy*, *call
  ended*, *failed to connect*. The phone's bar is speaker, video, mute,
  end; the Mac's video, screen, mute, end (no speaker route, no camera
  flip). Four emoji from the key's fingerprint, tap to reveal. An incoming
  call rings in a loop; an outgoing plays a ringback; busy, failed and
  ended each have a sound. A rating (1–5 stars, then the problems) is
  asked only when the wire says `need_rating`. An ended call is a line in
  the chat: the phone tells five reasons apart — *outgoing call*,
  *cancelled call*, *incoming call*, *missed call*, *declined call*, with
  *video* in the words when it was one — and the duration follows. The
  protocol advertised is `min_layer 65`, `max_layer 92`, UDP p2p and
  reflectors, and the library versions the linked tgcalls knows. The media
  is tgcalls over WebRTC; TDLib does the signalling (`createCall`,
  `acceptCall`, `updateCall`, `sendCallSignalingData`, `discardCall`) and
  hands the client the key, the servers, the config and the emoji in
  `callStateReady`.

## The words

- The **senses** are what the machine perceives for an app: where it is,
  what its camera sees, what its microphone hears. Each is a kernel
  capability with a fake; the shell installs the real ones on a run that
  is nobody's but this person's, as it installs the voice and the
  clipboard.
- A **fix** is one reading of where the device is: latitude, longitude,
  accuracy in metres, heading when moving, and when it was taken.
- A **capture** is a file the camera or the microphone made under the
  app's own directory: a *photo* (JPEG), a *voice note* (Ogg Opus), a
  *video message* (square mp4). It lives until it is sent or discarded.
- A **live share** is a location message the worker keeps moving until its
  period ends or the person stops it.
- A **call** is one conversation over the wire with one person, audio or
  video, in one of the states the wire names.

## The senses

Three kernel capabilities in `kernel/src/caps/senses.rs`, beside the voice
and the disk. Each is a trait, a fake, and a shared handle — a worker's
world and the window's must read one location, one camera.

```rust
/// Where the device is. `want` turns the receiver on, `release` lets it
/// go; `fix` is the last reading, if any. A denied permission is an error
/// a panel says out loud.
pub trait Location {
    fn want(&mut self) -> Result<(), String>;
    fn release(&mut self);
    fn fix(&self) -> Option<Fix>;
}

/// The camera and the microphone, as captures. Each `start_*` opens the
/// device and each `stop_*` closes it and answers the file. `level` is
/// what the meter draws while a recording runs; `open_camera` names the
/// camera the `Video` widget shows while one is open.
pub trait Capture {
    fn open_camera(&mut self) -> Result<CameraId, String>;
    fn close_camera(&mut self);
    fn take_photo(&mut self, dir: &Path) -> Result<Photo, String>;
    fn start_voice(&mut self, dir: &Path) -> Result<(), String>;
    fn stop_voice(&mut self) -> Result<VoiceNote, String>;
    fn start_circle(&mut self, dir: &Path) -> Result<(), String>;
    fn stop_circle(&mut self) -> Result<VideoNote, String>;
    fn discard(&mut self);
    fn level(&self) -> f32;
}
```

`Photo { path, width, height }`, `VoiceNote { path, secs, waveform:
Vec<u8> }`, `VideoNote { path, secs, side, thumbnail: PathBuf }`.

- **The fakes** (`FakeLocation`, `FakeCapture`) are what every scripted
  run, test and library mount gets. The fake location answers the
  trailhead (`model::HERE`) and a test can move it. The fake capture
  writes real files: a photo is the demo garden JPEG; a voice note is a
  tone the same Opus encoder writes (the encoder is pure code, so the file
  is real and the waveform is computed the same way); a video message is
  the demo tree's `clip.mp4`, copied. Its level is `media::fake_level`.
  So a suite sends what a person would, and a fixture line drawn from a
  fake capture is a line drawn from a real one.
- **The real ones** live in `app/src/platform/senses.rs`. Makepad's
  senses are reached through `Cx` and answer as events, and a
  capability is called from a verb or a worker thread with neither; so
  the real capability is a *wish* the shell serves. `RealLocation::want`
  sets a flag; the stage, on its next event, calls
  `cx.request_permission(Permission::Location)` then
  `cx.start_location_updates()`; every `Event::LocationUpdate` lands in
  the shared slot `fix` reads; `Event::LocationError` and a denied
  `PermissionResult` land as the error the next `want` answers.
  `RealCapture` is the same shape over `use_video_input` /
  `video_input` (the frames), `use_audio_inputs` / `audio_input` (the
  samples), and the encoders below. The stage services the senses in
  `handle_event` before the hosted panels see the event, and asks for the
  permission once per kind per run.
- **Permissions.** The platform's own dialogs, through makepad's
  `request_permission`; a denial is the capability's error, said in the
  panel (*the camera is not allowed — System Settings › Privacy*) and
  never retried on its own.

### The encodings

- **A photo** is the newest camera frame (I420 or NV12, converted to RGB)
  written as JPEG at quality 80 by `jpeg-encoder` (pure Rust), at the
  camera's size capped at 1280 on the long side — what the client sends.
- **A voice note** is Opus in Ogg: the microphone's samples resampled to
  48 kHz mono, encoded in 20 ms frames at 30 kbps in the VoIP application
  by libopus through the `opus` crate (C, built from source, as SQLite
  is), paged by the `ogg` crate with the `OpusHead`/`OpusTags` headers.
  The waveform is the clients' rule above: 100 slices, each slice's peak
  against 1.8× the mean of the peaks floored at 2500, `min(31, peak·31 /
  scale)`, packed five bits each into 63 bytes. Shorter than half a second
  is discarded with a note; there is no cap.
- **A video message** is a square mp4, H.264 at 1000 kbps with AAC mono
  at 48 kHz and 64 kbps, 384×384, 30 fps, a keyframe every second: each
  frame cropped to its centre square and scaled. On macOS the encoder is
  makepad's `VideoFileEncoder` (AVAssetWriter, VideoToolbox), pushed
  NV12 frames and 16-bit PCM. On android the same file is written through
  `MediaCodec` and `MediaMuxer` over JNI, in the app's
  `platform/android/recorder.rs`, in the synchronous mode that needs no
  Java class — the fork carries no file encoder for android. Sixty
  seconds is the cap: the recording stops there and the strip stays with
  its two verbs. The thumbnail is the first frame as a 320×320 JPEG.
- **A capture's file** sits under `<store dir>/captures/`, named by the
  clock, and is removed once sent (TDLib has read it by then; the
  guard from CR-013 copies rather than moves a file outside the
  engine's own directory) or discarded.

## The map

`map::TileSource` gets its real implementation: `Tiles`, a kernel
capability over OpenStreetMap's raster tiles
(`https://tile.openstreetmap.org/{z}/{x}/{y}.png`), fetched by a worker
with the app's user agent, decoded, and kept in the blob cache under
`tile:<z>/<x>/<y>` — one budget over everything cached, as CR-013 wanted.
A snapshot is composed from the tiles it has; a missing tile is asked for
and the snapshot is drawn again when it lands, the way a photo appears
after its download. The fake stays the drawn grid, so every scene and
suite draws what it drew.

The snapshot is 320×160 at zoom 15 in a transcript row, the pin at its
centre, and the line's card draws it at the card's width; a live
location's pin moves as the row is rewritten. Under the map the line says
what it said, with what the reference adds: `location 47.0472, 8.3164`,
`live location … · 42 min left · updated 2 min ago`. Attribution is one
muted line in the media kit's `MediaMap`, *© OpenStreetMap*, because the
tiles' licence asks for it.

**Opening a place.** The line's card and the viewer wear `google maps`
(`g`) — `https://maps.google.com/maps?q=<lat>,<lon>`, the Mac client's own
link, which android hands to the Maps app and the Mac to the browser —
beside `maps` (`m`, Apple Maps, macOS only) and `browser` (`b`,
OpenStreetMap).

## Places, live

The place panel (`place`, joined to the attach panel) shows the device's
fix on the map as it arrives — *finding you…* until it does, then the
coordinates and the accuracy, `47.0472, 8.3164 · ±12 m` — and wears:

- `send` (`s`): the fix, once, `inputMessageLocation`.
- `live 1 h` (`v`): `inputMessageLiveLocation` with the period; `period`
  (`e`) cycles the label through *15 min*, *1 h*, *8 h* and *until
  stopped* (`0x7FFFFFFF`), the phone's four.
- `stop live` (`o`) while a share of this chat runs.

A live share is the worker's: `Account` keeps `live_shares` — chat,
message id, until — and on every pass reads the `Location` capability
and sends `editMessageLiveLocation` by the phone's rule: when the fix
moved more than a metre and the last edit is ten seconds old, with the
heading when the device is moving. On *stop* it sends the edit with
`location` null; at the period's end it drops the share and sends
nothing, as the phone does; with no share left it releases the receiver.
On sign-in `getActiveLiveLocationMessages` restores the list, so a share
survives a restart. The chat's status line says `sharing live location ·
42 min left` while one runs.

Received, a live location's `updateMessageContent` moves the row's
coordinates (the projection already writes the media whole) and its
*updated* time; the expiry is the message's date plus its period — the
wire's `expires_in` where a fetched message carries it, since Apple's
clients send 3599 for *an hour* and the phone's client forgives five
seconds for that — and the line says *ended* past it.

## The attach panel

What CR-012 drew, made real, in the phone client's shape:

- `voice` (`o`): the strip — *recording voice 0:03*, the meter reading
  the capability's level, `send` (`s`) and `discard` (`d`), `enter` and
  `esc` the same. `send` stops the capture and sends `inputMessageVoiceNote`
  with the duration and the waveform, on its own, and the list comes back.
- `video` (`v`): the camera's picture over the strip, cropped square by
  the widget, from makepad's `Video` widget on the camera source (the
  front camera where there is one); the recording runs from the same
  frames. `send` sends `inputMessageVideoNote` with the length and the
  thumbnail. `flip` (`f`) trades cameras on a phone, mid-recording too.
- `camera` (`c`): the picture alone with `shoot` (`s`) and `done`
  (`n`). A shot lands on the chat's `CARRIES` list as a photo — the
  phone's strip is this list — and the camera stays up for the next
  shot; `done` puts the list back, where each shot shows its picture and
  can be removed or reordered like a picked file. The composer is the
  caption field: `enter` sends. Photos and videos on the list go **as
  one album** (`sendMessageAlbum`, ten at most a message), the caption
  on it, as the phone sends them; documents still go one each.

While a capture runs the attach panel says so in its title (`attach ·
Vera · recording`), the chat keeps the keyboard, and closing the panel
discards the capture — a recording is the panel's, as the reply line is
the chat's. Slide-to-cancel and the lock are gestures, and stay the
phone's; here the two verbs are the way out.

## Calls

A call is a panel: `call`, one per person, joined to nothing, opened by
`call` (`o`) or `video call` (`y`) on the peer's card, and by the worker
on an incoming call — in the workspace the person is looking at, with the
ring. The panel is the person's name, a state line in the reference's
words, the four emoji once the keys are exchanged, the remote picture
over the local one in a video call, and a bar that follows the state:

| State | Line | Bar |
|---|---|---|
| pending, outgoing | *contacting…*, *waiting*, then *ringing* once received | `end` (`e`) |
| pending, incoming | *incoming call* / *incoming video call* | `accept` (`a`), `decline` (`d`) |
| exchanging keys | *exchanging encryption keys* | `end` |
| ready, connecting | *connecting*, *reconnecting* | `mute`/`unmute` (`m`), `camera` (`c`), `speaker` (`p`, the phone), `end` |
| connected | the timer, `0:42` | the same |
| hanging up, discarded | *call ended · 2:31*, *line busy*, *declined*, *missed* | `close` (`c`), `rate` (`r`) when the wire asks |
| error | *failed to connect*, and the wire's words | `close` |

The sounds are the Mac client's set, bundled and played through the
media kit's player: an incoming loop, the ringback while an outgoing one
waits, and one note each for busy, failed and ended; any verb stops the
loop. The platform's own ringers are not reachable without a
notification, which the shell does not have.

**The engine.** [NTgCalls](https://github.com/pytgcalls/ntgcalls) — a
C library over libwebrtc that speaks tgcalls' protocol — behind the
`calls` cargo feature, on both platforms:

- On macOS the `ntgcalls-sys` crate fetches the published static
  archive for arm64 at build (3.0.0-rc03, whose C API this change binds
  through the `ntgcalls` crate) and links it with the frameworks it names.
- On android nobody publishes the C API — the AAR on Maven is the Java
  binding alone — so `libntgcalls.so` is built once from source with the
  library's own CMake (`-DBINDING=c -DSTATIC_BUILD=OFF`) and a complete
  NDK, its prebuilt libwebrtc and chromium clang fetched by that build,
  and kept in a prefix beside the TDLib one (`NTGCALLS_DIR`,
  `~/.cache/superapp-ntgcalls-android`). The recipe is
  `build-tools/ntgcalls-android.sh` (done, 2026-09-14: 75 `ntg_*`
  exports, 16 MB stripped, no `libc++_shared`, native Oboe audio in;
  four fixes to their CMake and templates applied by the script). What it
  found: the library as built still reaches a JavaVM for its video codec
  factories and camera list even in an audio call, so the script patches
  `wrtc`'s peer connection factory to use libwebrtc's built-in software
  codecs and an empty camera list when no VM is registered — the camera
  is external frames anyway. `./android.sh` stages the `.so` into the APK
  the way it stages `libtdjson.so`, `--no-calls` builds without it, and
  the crate is pointed at the prefix with `NTGCALLS_DYLIB=1
  NTGCALLS_LIB_DIR=$NTGCALLS_DIR/lib`. The audio route (`speaker`) is the
  app's, through `AudioManager.setMode(MODE_IN_COMMUNICATION)` and
  `setSpeakerphoneOn` over JNI, since the C API has none.

TDLib does what it does in every client, and the worker joins the two:

1. `createCall{user_id, protocol: ntg_get_protocol(), is_video}` on
   *call*; `acceptCall{call_id, protocol}` on *accept*; `discardCall` on
   *end* and *decline* with the duration and `is_disconnected`.
2. `updateCall` projects to `tg_call` (id, user, outgoing, video, state,
   started at, reason, emoji) — one row per call this run has seen, which
   the panel reads — and drives the engine: on `callStateReady` the worker
   calls `create_p2p`, `skip_exchange(encryption_key, is_outgoing)`,
   `set_stream_sources(capture: the microphone and, for video, the
   camera)`, `connect_p2p(servers, library_versions, allow_p2p)`.
3. Signalling both ways: `on_signaling_data` → `sendCallSignalingData`;
   `updateNewCallSignalingData` → `send_signaling_data`.
4. `on_connection_change` moves the row between *connecting*,
   *connected* and *reconnecting*; the timer runs from the first
   *connected*. `on_frames` hands the remote video's I420 frames to the
   panel, which draws them through the `Video` widget's app-owned frame
   session; the local picture is the camera source, as the attach panel's
   preview is.
5. The microphone and the speaker are NTgCalls' own devices (WebRTC's
   audio module with its echo cancellation; on the phone its native
   Oboe path, and if that proves silent, external frames from makepad's
   microphone and to its output). The camera is makepad's on both,
   pushed as external frames, so the preview and the call share one
   session.

**The line.** `messageCall` projects to a media kind `call`, in the
phone's five words: `outgoing call · 2:31`, `incoming video call · 0:08`,
`missed call`, `declined call`, `cancelled call`; the chat list's second
line says the same in a word.

## Voice notes, heard

A received voice note is Ogg Opus, which AVPlayer will not play, so on
the Mac it has run the fake timeline (CR-017, *not done*). With libopus
linked for the recorder, the media kit gains a second driver beside the
platform's player: `OpusClip`, which decodes the file to PCM on a worker
and plays it through makepad's `audio_output`, reporting position and
end the way the platform player does, so the strip, the transport, the
one-at-a-time rule and the scrub are unchanged. Chosen by `Source::File`
where `can_play_type("audio/ogg")` is *no*; the phone's ExoPlayer keeps
playing them itself.

## The store

Two rungs on the ladder:

| Rung | What |
|---|---|
| `tg_call` | a call this run has seen: id, unique id, user, outgoing, video, state, reason, started, ended, emoji — device-local, cleared on sign-in like phantom sends |
| `tg_live_share` | a share the worker keeps moving: chat, message id, until — device-local, re-read on sign-in and reconciled with the wire's answer |

`tg_message.media` gains the `call` kind and a live location's *updated*
time; nothing else changes shape. A capture is never in the store.

## The library and the suites

- The `media kit` scene gains the map with attribution and the meter at
  a real level; the `attach` scene gains *camera* and *video message*
  nodes drawn from the fake capture; a `call` scene draws the states
  over the demo world with a fake engine that connects after two virtual
  seconds.
- `e2e/telegram/senses`: opens Vera's attach panel, records a voice note
  for two seconds, sends it, sees the line; shoots a photo, sees it on
  `CARRIES`; opens the place, sends it, shares live, sees the status line,
  stops; the telegram suite stays green. Headless has no camera, no
  microphone, no location and no engine: the fakes are what run, which is
  the point of them.
- The app's tests: every bar still wears no reserved letter and none
  twice; the waveform of a known tone against the clients' rule; a voice
  note's Ogg pages parse back; the live share's tick sends an edit only by
  the phone's rule; the call row walks the states in the order the wire
  sends them; `messageCall` words; an album request's shape.

What no build here can prove is the platform: a fix from CoreLocation, a
frame from the FaceTime camera, a call connecting. Each is compiled,
clippy-clean with and without its feature, and is Andrey's to run — on the
Mac and on the Fold through `./android.sh`. The android build is at least
*built* here, since the SDK and the TDLib prefix are on this machine.

## Phases

1. **The senses.** `Location` and `Capture` with their fakes in the
   kernel; the real ones in `platform/senses.rs` over the fork's location,
   camera, microphone and permissions, serviced by the stage; installed at
   boot beside the voice. The Opus/Ogg writer and the waveform, proven on a
   tone; the JPEG shot; the mp4 circle on macOS through `makepad_video`
   and on android through `MediaCodec`/`MediaMuxer` over JNI (type-checked
   against the android target, built with `./android.sh build`).
2. **The map.** `Tiles` in the kernel over the blob cache, the fetch, the
   snapshot that redraws as tiles land, attribution; every map in the app
   on it; `google maps` on the card and the viewer. Independent of 1.
3. **The captures, sent.** The attach panel's *voice*, *video* and
   *camera* over `Capture`; `inputMessageVoiceNote`, `inputMessageVideoNote`,
   the photo onto `CARRIES` with its picture, the album send; the files
   under `captures/`; the scenes and the suite. After 1.
4. **Places, live.** The place panel over `Location`, the periods, the
   worker's `live_shares`, `editMessageLiveLocation`, the restore, the
   status line, *stop live*; received live lines moving. After 1 and 2.
5. **Calls on the Mac.** The `calls` feature, the NTgCalls binding,
   `tg_call` and the worker's join of TDLib and the engine, the call panel
   and its states, the sounds, `messageCall` lines, the fake engine and
   the scene. After 1 (the camera frames).
6. **Calls on the phone.** `libntgcalls.so` for arm64 from source, the
   prefix, `android.sh --no-calls`, the audio route (`speaker`), the
   Oboe-or-external decision, an APK that builds. After 5.
7. **Voice notes, heard.** `OpusClip` in the media kit on the Mac. After 1.
8. **The book and the review.** `telegram.md`, `media.md`
   (maps, captures, the Opus driver), `architecture.md` (the senses among
   the capabilities), `dev-x.md` (the `calls` feature, the two prefixes,
   the android recorder); a review pass over the whole against the
   reference behaviours above.

## To decide in review

- **The round mask.** The clients draw a video message as a circle; CR-012
  drew it square, by the language. Proposed: square, the poster being a
  picture like any other.
- **The album.** Photos and videos on `CARRIES` going as one album is
  the phone's behaviour and a change to CR-013's *each a message of its
  own*. Proposed: the album, since a strip of shots is what the camera
  makes.
- **One `period` that cycles** (proposed) or four verbs on the place
  panel's bar. Four labels is what the phone's sheet shows; four letters
  is what the bar cannot spare.
- **The tile server.** OpenStreetMap's own, with the app's user agent,
  is fine for one person and against the policy for a product. A key for
  another provider is a config line later.
- **The sounds.** The Mac client's five, or the incoming loop alone. A
  workspace that switches to a panel might be enough on a desk.
- **The phone's audio route.** `speaker` toggles `setSpeakerphoneOn`
  through JNI; the earpiece is the default, as on the phone. Or always
  the speaker, since a Fold is held open.

## Not done, on purpose

- **Group calls, conference calls, screen sharing, call history as a
  panel.** Each its own surface; `messageCall` lines are the history.
- **Venues, proximity alerts, the heading arrow on a live pin, the
  clients' venue search through a bot.** The wire carries them; the row
  says the coordinates.
- **Editing a shot** (crop, draw, tune, the HD toggle), **flash, zoom and
  tap-to-focus**, and **trimming a recording** before the send. A capture
  is sent or discarded.
- **Slide-to-cancel and lock gestures**, the phone's; the bar's two verbs
  are the grammar's.
- **Speech recognition of a voice note** (Premium) — the wire's
  `speech_recognition_result` is shown if it comes, never asked for.
- **Self-destructing media.** The wire's `self_destruct_type` is passed
  null.

## Progress — phase 1 (2026-09-14)

**The senses** are `kernel/src/caps/senses.rs`: `Location` and `Capture` as
the sketch draws them, `Fix`, `CameraId`, `Photo`, `VoiceNote`, `VideoNote`,
and the two fakes. `SenseSource` sits on `Env` beside the blob cache — one
receiver and one camera for every world of a run, since the device has one
of each — and `caps::install` puts both capabilities in every world, plus
the concrete fakes under their own types where they are the fakes, as the
voice is. A world the shell gave nothing gets the fakes, which is every
test, every scripted run and every library mount.

`FakeCapture` writes real files: a real JPEG of a generated shot, a real Ogg
Opus of a 440 Hz tone through the very encoder a recording uses, and a real
mp4 copied out of the demo tree. The demo tree had no clip to copy — the
`talk.mp4` in the listing carries no bytes — so one was made and added as
`kernel/resources/clip.mp4`: a second and a half of 384-pixel H.264 with a
mono AAC track, twenty kilobytes, the shape a video message is recorded in.
It is `demo::CLIP_MP4` and is deliberately *not* in the tree's listing, so
no files panel or suite sees a new row.

**The encodings** are `kernel/src/codec/`: `opus_ogg` (libopus through the
`opus` crate, 48 kHz mono, 20 ms frames, 30 kbps VoIP, paged by `ogg` with
the `OpusHead`/`OpusTags` headers), `waveform` (the clients' rule, and the
five-bit packing proved by hand), `jpeg` (quality 80, the 1280 cap, a box
downscale) and `pcm` (the linear resampler both the voice note and a video
message's sound track go through). A note written here was checked outside
the build: `ffprobe` reads it as 48 kHz mono Opus at 31 kb/s of the right
length, and its decoded samples are a clean 440 Hz tone at the amplitude
that went in.

**The real ones** are `app/src/platform/senses.rs`. A capability records a
wish; `Senses::service` — called from `Stage::handle_with` before any hosted
panel sees the event — turns wishes into `cx` calls and lands
`LocationUpdate`, `LocationError`, `PermissionResult`, `VideoInputs` and
`AudioDevices` into the one state behind them. The camera callback keeps the
newest frame as owned I420 for a photograph and, while a circle records,
crops it to its centre square, scales it to 384 and hands it to the
recorder; the microphone callback smooths a level and feeds whichever
recorder is running. Each recorder is a thread with a short channel in front
of it, so neither callback ever waits on an encoder. Files go under the
directory the panel names, by the clock; a note under half a second is
discarded with a note; a circle stops itself at the minute.

- **macOS** writes the circle with `makepad_video::VideoFileEncoder`
  (AVAssetWriter, VideoToolbox): NV12 frames and 16-bit PCM, H.264 at a
  megabit, 384 square, 30 fps, mono AAC at 48 kHz and 64 kbps.
- **android** writes it in `app/src/platform/android/recorder.rs` through
  `MediaCodec` and `MediaMuxer` in the synchronous mode, framework classes
  only: `video/avc` with `COLOR_FormatYUV420Flexible` written plane by plane
  against the strides `getInputImage` reports, `audio/mp4a-latm` fed 16-bit
  PCM, and a muxer whose first samples are *held* rather than dropped until
  both tracks have a format — the first video sample is the keyframe.
- **elsewhere** there is no encoder and the capability says so.

**Boot** installs the platform's senses only on a real run that nobody is
scripting, beside the real voice, and hands the stage the handle it services.

### Deviations from the sketch

- **`open_camera` is two calls**, `open_camera() -> Result<(), String>` and
  `camera() -> Option<CameraId>`. It cannot be one: the device list arrives
  as a platform event, so which camera a `Video` widget is pointed at is not
  known in the frame the wish was made in. The panel asks for the picture and
  shows it when it comes, as it asks for a fix and draws it when it comes.
- **`Senses::service` answers a `bool`** — whether anything a panel reads
  has changed — and the stage redraws on it. Without it the place panel
  would say *finding you…* until something else happened to ask for a frame.
- **`media::fake_level` moved into the kernel** (`caps::fake_level`) and the
  shell's delegates to it, because the fake capture answers it as its level
  and a meter drawn from either has to be the one wave.
- **The macOS keyframe interval is AVAssetWriter's own.** Makepad's encoder
  options carry `keyframe_only` and nothing between it and the default, so
  the *keyframe a second* the reference names is set on android
  (`i-frame-interval`) and left to the platform on the Mac.
- **The android build needed a CMake toolchain file of ours**,
  `build-tools/android-cmake.toolchain`, named by `android.sh`. libopus
  builds through CMake, and neither of CMake's two ways of cross-compiling
  for Android works here: the built-in module wants a `platforms/android-*`
  tree no NDK has carried since r23, and the NDK's own toolchain file — which
  only the full NDK ships — reads `ANDROID_PLATFORM`, which cargo-makepad
  already exports as `android-33-ext4`, which is not an API level and which
  clang refuses. The file says only that this is a cross build and which
  compiler to use, read from the variables cargo-makepad already exports for
  the `cc` crate, so the NDK's path is written down in one place and it is
  not there.

### What could not be verified here

The platform, as the change says. No fix came from CoreLocation, no frame
from a camera, no mp4 out of AVAssetWriter or `MediaCodec`, and no
permission dialog was answered — a build machine has no sky over it and no
face in front of it. What is proved: the codecs against a decoder, the fakes
against their files, and the wish-and-answer state machine against events
built by hand (`app/src/platform/senses/tests.rs`), which is why it is
written as two halves that need no `Cx`.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with TDLib); `cargo test
--workspace --locked --no-default-features` 1366 + 410 + 2 passed, 0 failed;
`./e2e/run-all.sh` 114 suites, no failures; `./android.sh build` produces
the APK, libopus and the JNI recorder cross-compiled for arm64.

## Progress — phase 2 (2026-09-14)

Built, on `worktree-agent-a399eda5ba7fe0159`:

- **`Tiles`, a kernel capability** (`kernel/src/caps/tiles.rs`): a map asks a
  source for tile `(z, x, y)` and is answered `Ready(pixels)`, `Pending` —
  asked for, on its way — or `Missing`. The fake is the drawn street grid
  that used to live in the shell's `FakeTiles`, moved over unchanged, so a
  scene and a suite draw exactly the map they drew before. It is installed
  into every world beside the other fakes, and there is one process-wide
  handle (`tiles::install`, `tiles::source`) besides, because a map is
  composed on a picture worker that holds no world and has no `Cx`.
  `TILE` and the ground colour moved with it; the maths, the pin and the
  ways out stayed in the shell's `widgets/map.rs`.
- **OpenStreetMap, for real** (`app/src/shell/tiles.rs`). It is the shell's
  and not `platform/`'s because none of it is what *this machine* answers
  for — there is no macOS way and android way to fetch a PNG — and it sits
  beside the maths it serves. Tiles come from
  `https://tile.openstreetmap.org/{z}/{x}/{y}.png` on the async runtime,
  with a user agent naming the program, its version and its repository, a
  ten-second patience, at most two fetches on the wire, the PNG kept in the
  blob cache under `tile:<z>/<x>/<y>` so a restart never refetches, and the
  decoded BGRA in a sixty-four-tile LRU. A tile that is not held answers
  `Pending`; when it lands, a `Landed` action wakes a redraw the way a
  decoded picture does. A failed fetch answers `Missing` and is left alone
  for a minute. `boot` installs it only on a run nobody is scripting, so a
  suite reaches no server at all.
- **A snapshot says what it has**: `Snapshot::complete` is false wherever a
  tile was not there, ground stands in for it, and the transcript's picture
  cache treats such a map as missing — its once-a-second retry is what asks
  again, so the minute's cool-off after a failure eventually turns back into
  a fetch. `PlacePanel` lost its once-only `mapped` flag: it re-snapshots
  when the device's coordinates change and when a tile lands.
- **The credit**: `MediaMap` draws one muted `© OpenStreetMap` under the
  picture, in the kit's own style, wherever a map is shown — the grid in a
  scene included, because the line belongs to the kit.
- **A third way out**: `google maps` (`g`) beside `maps` (`m`) and `browser`
  (`b`), on the line's card and on the viewer, from `map::google_url`. Apple
  Maps is offered only where there is one to open; a phone opens
  `maps.apple.com` at nothing.
- **Tests**: the tile key and the LRU; `Pending` → `Ready` through a canned
  fetcher, with the blob cache proving a restart draws without asking again;
  a refused tile answering `Missing` once and not being asked again; a
  snapshot with a pending tile being incomplete and whole once the tile is
  there; `google_url`; and the bar-letter suite over the place card and the
  place viewer.

Decisions worth naming:

- The three ways out still *report* their URL as a draft toast, exactly as
  `maps` and `browser` did before: this phase adds the third way, not an
  opener.
- Apple Maps is gated with `cfg!(target_os = "macos")` rather than the
  attribute, because clippy refuses a `Vec::new()` followed by a
  conditionally compiled push.
- A `Missing` tile, not only a `Pending` one, leaves a snapshot incomplete.
  Otherwise nothing would ever ask again after the cool-off.

What is **unverified here**: no real tile was ever fetched. Nothing in the
tests may reach the network, and a suite never installs the real source, so
`Web`'s reqwest path — the user agent as OpenStreetMap sees it, a real 256²
PNG through `decode_image_from_data`, the blob written and read back on a
second boot, `Landed` waking a window — is proven only through a canned
fetcher handing over the app's own 256×256 icon as a stand-in tile. The
first run on a machine with a network is what will say whether the map
really draws.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings`; `cargo clippy -p superapp
--all-targets --locked -- -D warnings`; `cargo test --workspace --locked
--no-default-features` (1363 + 392 + 2, no failures); `MAKEPAD=headless
cargo build -p superapp --no-default-features` and `./e2e/run-all.sh` — 114
suites, no failures.

## Progress — phase 3 (2026-09-14)

**The attach panel** now makes what CR-012 drew, over the kernel's `Capture`
capability (`app/src/apps/telegram/panels/attach.rs`). The panel holds the
world it was opened with, because a verb has no session to reach a
capability through and a draw has no `&mut Session` either; everything the
camera and the microphone are asked is one `with_cap` away.

- **`voice` (`o`)** starts `start_voice` under `captures/`, the strip says
  *recording voice 0:03*, and the meter draws `Capture::level` — the real
  microphone's smoothed level, the fake's wave. `send` (`s`, `enter`) stops
  it and sends `inputMessageVoiceNote` with the duration and the waveform,
  on its own and with no caption; `discard` (`d`, `esc`) throws it away.
- **`video` (`v`)** asks for the camera and starts the circle the frame the
  camera answers — `open_camera` is a wish, so the panel waits for
  `camera()` and gives up with a word after five seconds if nothing comes.
  The picture stands over the strip in a new `MediaCamera` (the media kit's
  square box, cropped to fill rather than letterboxed), pointed at the open
  camera with `set_source_camera` in `Texture` preview mode and primed with
  an empty quad a frame the way a clip's player is, because android hands a
  player its texture only on a draw. At [`CIRCLE_MAX`] the capture stops
  itself: the panel takes the file, the clock stops at the minute, the meter
  falls to nothing and the strip stands with its two verbs. `send` sends
  `inputMessageVideoNote` with the side, the duration and the 320-square
  poster.
- **`camera` (`c`)** is the picture alone, with `shoot` (`s`) and `done`
  (`n`). Each shot is a JPEG under `captures/` put on the chat's carried
  list through the join, and the camera stays up for the next one. A carried
  photograph's row draws its own picture, through the transcript's picture
  cache (`pictures::local`), so a row drawn sixty times a second decodes
  once.
- Closing the panel discards, in `Drop`: a window shut on a running
  recording must not leave a device listening to an empty room.

**The album.** `requests::parcels` says how a carried list leaves: the
photos and the videos together where there are two to ten of them, the group
standing where its first picture stood, everything else a message of its
own. One picture is still a `sendMessage`; two are a `sendMessageAlbum` with
the caption on the first content and the reply on the message.
`sendMessageAlbum` was taught to the three places that know what a send is —
`history::describe` (*send 2 pictures*, undo deletes the album),
`history::targets`, `operations::sending`/`label`, and the reply
correlation's *expected* count, which until now read `message_ids` alone and
would have called an album's two-message answer an incomplete send.

**Live accounts.** The *recording is not available yet* refusal is gone: a
capture goes through `told` → `history::command` like every other send, and
a build with no worker keeps the draft toast, which now says what would have
left — *draft: nothing leaves — voice 0:02*.

**The files.** A capture is written under `<store dir>/captures/`, or a
numbered directory of its own under the system's temp where there is no
store — a library mount, a test. A discarded one is removed at once
(including one the minute had already written); a sent one is left where it
is, because TDLib reads the file while it uploads it, and
`sync::sweep_captures` collects what is older than a day at the account
worker's start.

**The library and the suites.** The `attach` scene gained *camera* and
*shots* nodes and its *video message* node now really records on the fake;
`e2e/telegram/senses.txt` records a voice note for two seconds and sends it,
shoots two photographs and finds them on `CARRIES`, and records a video
message and throws it away. The app's tests cover the panel's state machine
over `FakeCapture` (what is started, what is written, what a send says, what
a discard takes back, the minute's stop, a refusal said in the panel's own
words, and the drop), the four request shapes, the parcel rule, the sweep,
and the bar letters over the camera's bar as well as the recording's.

### Deviations from the sketch

- **`flip` is not there.** The capability has no way to trade cameras — it
  opens *the* camera and answers which one it turned out to be — so the
  verb would have nothing to call. It belongs with the phone's own camera
  work, not here.
- **A capture answers nothing.** `inputMessageVoiceNote`'s caption is the
  empty `formattedText` and the reply is left off: the sketch says a voice
  note goes *on its own*, and the chat's reply line is the chat's. A forum's
  topic *is* carried, through `requests::in_topic`.
- **`Recording` grew a `stopped`**, so the minute can stop the clock while
  the strip stands; the line then reads *video message 1:00 · recorded*.
- **The panel says one thing when the camera never comes.** `open_camera`
  answering *yes* is not the camera arriving, and a device with no camera
  only says so once its (empty) device list has landed; five seconds of
  waiting is a refusal.

### What could not be verified here

The devices, again: no frame from a camera, no sample from a microphone, no
preview drawn from a live `Video` session, and no upload of a capture by
TDLib. What ran is the fake, which writes real files — the suite really
encodes an Ogg Opus through libopus and really writes a JPEG — and the
request builders against their shapes. The macOS camera preview
(`set_source_camera` in texture mode inside a hosted panel) and the android
one (the primed quad) are Andrey's to look at.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with TDLib); `cargo test
--workspace --locked --no-default-features` 1374 + 412 + 2 passed, 0 failed;
`MAKEPAD=headless cargo build -p superapp --no-default-features` and
`./e2e/run-all.sh` — 115 suites, no failures.

## Progress — phase 7 (2026-09-14)

**Voice notes are heard.** A received note is Ogg Opus, which AVFoundation
will not open, so on the Mac one has run the clock timeline since CR-017 —
the seconds counting over silence. It now decodes and plays.

- **The decoder** is `kernel::codec::opus_ogg::decode(path) -> Pcm`, beside
  the writer phase 1 added and for the same reason: it is arithmetic over
  bytes. Pages by the `ogg` crate, `OpusHead` parsed for the channels, the
  pre-skip and the output gain, packets through libopus to 48 kHz float, a
  stereo file folded to one channel, the pre-skip dropped and the last
  page's granule position trimming the silence a writer padded its final
  frame with. `Pcm` — samples and a rate — sits in `codec::pcm`, which is
  where the sound already is. A test round-trips a 440 Hz tone through the
  encoder and back: the length, a Goertzel bin at 440 against four others,
  and the level it went in at.
- **The sound out** is `app/src/shell/sound.rs`: one `Mixer` over a slot
  holding at most one recording, its position and whether it runs. A
  `Voice` is a claim on the slot — taking it stops whatever had it, and
  dropping it (a panel closing) stops the sound, which is the one thing a
  wish on a transport cannot do without a draw. The callback is a `Send`
  closure on the platform's audio thread that locks the slot, resamples 48
  kHz to the device's rate (linear, between the two samples the position
  falls between), writes the one recording into every channel and returns;
  it allocates nothing after the first buffer. **With no device out** — a
  headless build, a machine with no speaker, the moment before one is open
  — the position moves by the clock the transport already ticks against,
  so a scripted run behaves exactly as a real one does minus the sound.
  The callback latches a flag the first time it runs; after that the
  position is its, however long it is between draws.
- **The driver** is `OpusClip` (`shell/widgets/media/opus.rs`): pointed at
  a thing by the host's key, given a source and a wish, answering a
  `ClipDrawn` of the same shape `Clip` does, with a word for the trace
  (`silent`, `reading`, `ready`, `playing`, `paused`, `ended`, `refused`).
  The decode runs on `kernel::runtime::spawn_blocking`; until it lands the
  strip reads *pause* at `0:00`, as it does while the platform prepares a
  clip. A recording that could not be read reports *not playing*, so a
  refusal puts the button back rather than leaving the host drawing at it.
- **The kit picks**, not the host. `Clip::drive` asks `media::played_by_kit`
  — a `Source::File` whose name ends in `.ogg`, `.oga` or `.opus`, anywhere
  but android, whose own player takes Opus — and where it says yes, hands
  the box's native player back and drives the `OpusClip` instead. The
  strip, the transport, the one-at-a-time rule and the scrub are untouched:
  what pauses one pauses the other, since both are the same wish on the
  same transport. `Clip::drive` gained a `now`, which is the clock a
  deviceless run moves a recording by; its four callers pass the session's.
  `Clip::hush` stops a recording without a draw, beside the `pause_video`
  the native player already had.
- **The stage** serves the output the way it serves the senses.
  `Senses::land` writes down `default_output()` off the `AudioDevices` event
  it already lands `default_input()` from, and `Stage::handle_with` hands
  that list to `sound::service`, which installs the callback once and opens
  the default device. It does nothing at all until something has a
  recording loaded — which matters, because registering the callback is
  also what wakes makepad's enumeration, and a run that never plays a note
  should never open a speaker.
- **Telegram** asks for a note's bytes on *play* and never before, by the
  `rid` the row already keeps: `Playback::ask_for_sound` wants the file,
  the worker turns it into `getRemoteFile` and a download, and the bytes
  land in the blob cache under the key the row names — the road a picture
  the cache has let go already travels. `plays_sound` is a `voice` line of
  the wire's; a demo note names no `tg:` file and keeps the fake timeline
  it has always had. It plays on the transcript row, the line's card and
  the viewer, all three through the one driver.

### Deviations from the sketch

- **The choice is by the file's name, not by `can_play_type`.** Makepad's
  player has no such question to ask, and the blob cache's playable link
  already carries the extension its bytes say it is (`playable_path`), read
  by the very sniffing that made the link. So `played_by_kit` reads the
  name.
- **Everywhere but android, not macOS alone.** The sketch says the Mac;
  written as *not android* it means a headless or Linux build takes the
  same path this machine tests, rather than a different one nothing here
  runs.
- **The mixer is the process's, not a store's.** The transport's registry
  is per store, deliberately, so two sessions in one test process never
  pause each other; a device cannot be. `sound::alone()` is the lock a test
  that plays through the process's mixer holds.
- **The sound out is served from the stage, not from `Senses::service`.**
  `platform/` names nothing of the shell anywhere else, and the mixer is
  the shell's. What is shared is the landing of the device list, which is
  one line in `Senses::land` and a reader beside it.
- **A file card's `.ogg` now plays too**, since the choice is the kit's and
  the file viewer drives the same `Clip`. Nothing in the tree offers one,
  so nothing here exercises it.

### What could not be verified here

**No sound was ever made.** There is no speaker on a build machine and no
headless backend for one — `use_audio_outputs` and `audio_output` are
no-ops under `MAKEPAD=headless` — so the callback, the resampler against a
real device rate, `default_output`'s choice, and whether a note is audible
at all are Andrey's to hear on the Mac. What is proved: the decoder against
the encoder on a real tone; the mixer's position, seek and end arithmetic
without a device, and its callback against a hand-made `AudioBuffer` at
half the rate; the driver's words over a real Ogg Opus file; the kit's
choice; and the whole transcript path — a note asked for on play, decoded,
played, moving by the clock and running out — with the platform's player
never once prepared (`inline_video.rs`). No e2e suite plays one, because a
scripted run has no engine to receive a `tg:` note from, and a demo note
deliberately stays on the fake timeline.

`docs/book/src/media.md` still says a voice note is what this build cannot
decode; the book is phase 8's, and this is what it should say instead.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean; `cargo test --workspace
--locked --no-default-features` 1379 + 414 + 2 passed, 0 failed;
`MAKEPAD=headless cargo build -p superapp --no-default-features` and
`./e2e/run-all.sh` — 114 suites, no failures. (`apps::workshop::snapshots`
fails now and again under a loaded parallel run with an empty `Git: `
error, on this branch and beside it; it passes alone and on a second pass.)
