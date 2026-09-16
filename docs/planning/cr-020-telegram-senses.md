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
*updated* time; the expiry is what the wire says is *left* of the share,
counted off the clock, and the message's date plus its period where the
wire says nothing — less the five seconds the phone's client ends a share
**early** by whenever the period is not whole minutes, which is how it
copes with Apple's clients sending 3599 for *an hour* — and the line says
*ended* past it.

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

## Progress — phase 4 (2026-09-14)

Built on `worktree-agent-a7b6dac7bd6926139`, over phases 1 and 2.

**The place panel is a panel over the receiver.** `PlaceKind::open` asks for
it, `Drop` lets it go, and a panel that was refused releases nothing, having
taken nothing — the capability counts its holders and a saturating decrement
would steal the worker's. Until a fix arrives the panel says *finding you…*
and draws no map: a pin at nowhere is a place the person is not. With one, it
is the map at the fix and `47.0472, 8.3164 · ±12 m`. A refusal is its own
line, in the receiver's own words, and nothing is waited for after it. Its
bar is `send` (`s`), `live 1 h` (`v`), `period` (`e`) — which walks *15 min*,
*1 h*, *8 h*, *until stopped*, an hour being where it starts, as in the
clients — and `stop live` (`o`) while a share of this chat runs.

**The shares are the worker's**, in `sync/live.rs` and nowhere on disk. The
account learns one from the echo of its own send (`updateNewMessage`, then
`updateMessageSendSucceeded`, whose id is the one an edit can name — a
pending message's temporary id is refused, and the old id is forgotten), and
every pass it reads `Location` and sends `editMessageLiveLocation` by the
phone's rule: moved more than a metre *and* the last edit at least ten
seconds old, with the heading where the device is moving. A stop is the edit
with the location gone, never a delete; a share past its `until` is dropped
with no request at all. `want` on the first share, `release` on the last —
once, whatever the ask answered, so a refusal is not re-asked three times a
second. While a share runs the pass sleeps at most a second
(`next_pass_sharing`). The runtime publishes the list the panels read
(`live_share`, `set_live_shares`) and carries the stop wishes
(`stop_live`/`take_live_stops`) in a queue of their own, not `Wanted`'s,
which the pass drains only after the chat lists have loaded.

**The chat's status line** says `online · sharing live location · 42 min
left`, beside *loading…* and in the same grammar.

**Received live lines move.** `tg_message` gains one column,
`media_updated`, by a rung (`v19_live_updated`) placed **before** the
column-order repair and inside the range that repair builds its canonical
from — a rung that added a column after it would leave every store a column
wider than the canonical and the repair would refuse them all. `Media.updated`
is the message's `edit_date` when a line arrives whole and the clock when an
edit brings it, and a row reads `live location 55.7512, 37.6184 · 42 min
left · updated 2 min ago`, the *updated* half dropping away once the share
has ended.

**The three ways out open for real.** `maps`/`google maps`/`browser` on the
line's card and on the viewer leave a URL wish on the panel (`take_url`), and
the widget hands it to `platform::browser::open_or_notify` on its next draw —
the rss pattern, a panel having no `Cx`.

**Scenes and suites.** The attach scene's `place` node gained three
neighbours — *finding you*, *refused*, *sharing* — and the first two set the
mount's `FakeLocation` up through a new `catalog::panel_in`, which hands the
node its whole `Session` rather than only its store. `e2e/telegram/places.txt`
walks Vera's chat → `attach` → `place`, sends the place, walks the four
periods, shares live, reads the chat's status line, stops it, and then opens
the hike's place card and presses two of its ways out.

### Deviations from the sketch

- **`getActiveLiveLocationMessages` does not exist** in the installed TDLib.
  What it has is `updateActiveLiveLocationMessages`, which the engine pushes
  on sign-in and whenever the set changes; that update is what restores the
  list, and it is the better seam — a share ended from another device is
  simply not in the next one. `on_ready` drops what this run believed first.
- **`messageLocation` carries only a point** in this TDLib, and a live one is
  its own content, `messageLiveLocation { location: liveLocation, expires_in }`.
  So `updates::content` grew an arm rather than a branch, and the one-off
  send lost the `live_period`/`heading`/`proximity_alert_radius` it had been
  spelling into `inputMessageLocation`, which that content has no room for.
  The old flat reading is still honoured on `messageLocation`, so a fixture
  written against the earlier layer draws the same.
- **The three ways out are held back by `Delivery`, not by the store's
  directory.** The change said a world with no store directory keeps the
  draft toast; a scripted run *is* given a store on disk (`resolve_db` makes
  one under the temp dir), so that test would have opened a browser from
  every suite. `Delivery::Live` — real mode, nobody scripting — is the same
  test a send is weighed by and is the right one.
- **`panel_in`** is the new catalogue helper; `Open`/`Opener` now take a
  `&Session` rather than a `&Store`, and the four existing helpers adapt
  inside, so no other scene changed. It mounts on a fake outside of
  necessity: a `Deny` world has the clock and nothing else, so the place
  scene's nodes (and the `place` node that was there) are `panel_fake`.
- **`model::HERE` is gone**, the trailhead being the fake receiver's answer
  now (`caps::senses::TRAILHEAD`).
- **Two test modules that only compile under `MAKEPAD=headless` were
  broken before this phase** — `shell/touch_tests.rs` lacked phase 1's
  `senses` field — and are fixed here, since the required clippy runs do not
  set `headless` and would not have caught it.

### What could not be verified here

The platform, again: no fix ever came from CoreLocation, so no edit was ever
computed from a device that really moved, and no `editMessageLiveLocation`
reached Telegram. What is proved is the shape of the three requests against
the installed header, and the tick's rule, the expiry, the stop, the restore
from the wire's own list, the want/release counting and the refusal, all over
`FakeLocation` and the fake transport. The first real share is Andrey's to
run — and the one thing to watch for is whether TDLib pushes
`updateActiveLiveLocationMessages` at sign-in when there is nothing running,
since the restore leans on it rather than on a request.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with TDLib); `cargo test
--workspace --locked --no-default-features` — 1390 + 2 + 412 passed, 0
failed; `MAKEPAD=headless cargo build -p superapp --no-default-features` then
`./e2e/run-all.sh` — 115 suites, no failures.

## Progress — phase 5 (2026-09-14)

**Calls on the Mac** are built. TDLib does the signalling and NTgCalls carries
the media; the worker is the joint between them, the runtime holds the call,
and the panel draws it.

- **The engine seam** is `app/src/apps/telegram/calls/`. `CallEngine` is five
  instructions — `start`, `signalling`, `mute`, `camera`, `stop` — and a
  `tick` that hands it the world's clock. It says back one of two things
  (`Told::Signalling`, `Told::Link`) down a channel the worker's pass drains,
  and drops the newest frame of each side into one process-wide slot
  (`calls::frames`) the panel reads on its draw, because a video call makes
  thirty a second and not one of them is worth a pass.
  - `NtgEngine` (macOS, `calls`) owns the library in one task on the kernel's
    *local* executor — its handle may be sent to a thread but not shared
    between two, and a task on the shared pool moves at every await. On
    `callStateReady` it does the five steps every tgcalls client does:
    `create_p2p_call`, `skip_exchange(key, is_outgoing)`,
    `set_stream_sources` for capture and for playback out of the library's own
    `get_media_devices`, and `connect_p2p` with the wire's servers and
    `library_versions`.
  - `FakeEngine` is everywhere else — android, a build without the feature,
    and every test: it connects two of the *world's* seconds after it is
    started, so a suite under a virtual clock walks the states a real call
    walks, and it keeps every instruction for a test to read back.
  - Which one an account gets follows the transport, by a new `Td::REAL`: the
    one live client gets the engine, a `FakeTd` never does. So a test with the
    feature linked still runs on the fake, and there is no `cfg(test)` in the
    choice.
- **Signalling** is `sync/calls.rs`. `updateCall` writes the runtime's call —
  id, unique id, person, direction, video, state in the reference's words,
  first connection, end, emoji, reason, `need_rating` — and drives the engine;
  `on_signaling_data` goes out as `sendCallSignalingData` (base64) and
  `updateNewCallSignalingData` comes back in; the connection's changes move
  the row between *connecting*, *connected* and *reconnecting* (no engine says
  *reconnecting* — the row remembers that it had once connected), and the
  timer runs from the first connection. A connection that dies discards the
  call with `is_disconnected`. `ready` clears them all, as it clears phantom
  sends.
- **The panel** is `call`, one per person, joined to nothing: the name, the
  state line, the four emoji, the other side's picture over mine, and a bar
  that follows the state — `end`; `accept`/`decline`; `mute`/`camera`/`end`;
  `close` and, where the wire asked, `rate`. It is opened by the person's card
  and, on an incoming call, by the worker: the pass leaves the person on the
  runtime and `Telegram::poll` — the one thread that owns the slots — opens it
  in the workspace the person is looking at, or focuses it if it is already up.
- **The sounds** are generated, not bundled: `calls/sounds.rs` writes four
  16-bit WAVs under `<store dir>/sounds/` at first use — the bell (440+480 Hz,
  two seconds on and four off), a ringback (425 Hz, one on and four off, so
  the two are never confused), the busy cadence (480+620 Hz, three times) and
  one short note. The two that ring loop; any verb silences them. A world with
  no directory — every fixture, every scripted run — writes nothing and is
  silent.
- **The line**: `messageCall` projects to a `call` media, and `Media::word`
  and `Media::line` give the reference's five — *outgoing call · 2:31*,
  *incoming video call · 0:08*, *missed call*, *declined call*, *cancelled
  call*, and *line busy* for a refusal at the far end. The chat list's second
  line follows through the same function.

### Deviations from the sketch

- **The two letters on the card are `o` and `v`, and the words are *voice
  call* and *video call***. The sketch asked for `call` (`o`) and `video call`
  (`y`), and the app's own rule — which a test keeps — is that a bar's letter
  must be in the word it underlines. Neither `o` nor `y` is; and of the
  letters in *call*, `c`, `a` and `l` are the chat, the archive and the
  workspace's own. *voice call* carries its `o`, and *video call* carries `v`,
  which is free on a person's card (*leave* is a group's verb and never stands
  beside these).
- **The local picture is the engine's own capture frames**, not a second
  camera session through makepad. NTgCalls reports what it is sending as
  `StreamMode::Capture` frames beside what it receives, so one session serves
  both the call and the preview — which is what the sketch wanted anyway, and
  it means no second `AVCaptureSession` can be refused out from under a call
  in progress. If the library turns out not to report capture frames on macOS,
  the local box simply stays empty and the call is untouched.
- **A frame is drawn as a texture, not through the `Video` widget's
  app-owned frame session.** The frames are converted from I420 to BGRA on
  whichever thread they arrive on and uploaded with `Texture::set_data_u32`
  into an `Image`, which is the idiom the map and the picture cache already
  use. The frame-session path would have meant changing the media kit's
  player, which three other phases are editing.
- **`rate` sends five and no problems**, as this round's brief allows. The
  stars and the problem list are a later surface.
- **NTgCalls is linked as a shared library, not the published static
  archive.** The archive `ntgcalls-sys` fetches does not link with the `ld`
  Xcode 26 ships — it asserts inside its own relocation parser
  (`findRealAtom`, Relocations.cpp) on the ffmpeg objects in it, and neither
  `-ld_classic` (which then fails on compact unwind in libwebrtc's
  `audio_shell_writer`) nor `-no_compact_unwind`, `-dead_strip`, `-S` or a
  newer deployment target gets past it. The shared library of the same release
  links and runs, so `.cargo/config.toml` points the crate at
  `target/ntgcalls/lib` with `NTGCALLS_DYLIB`, and `app/build.rs` fetches
  twelve megabytes into it on the first build of a checkout and adds the
  -rpath. An `NTGCALLS_LIB_DIR` exported in the environment still wins, which
  is how `./android.sh` will point the same crate at its own prefix.
- **Without an engine** — android, or a build without `calls` — a live account
  says *calls are not available on this device yet* to *voice call*, *video
  call* and *accept*; an incoming call still opens the panel, still rings and
  can still be declined. A **demo** world is never refused: it has the fake,
  which carries nothing, and every other verb there is a draft toast the same
  way.

### What could not be verified here

No call was made. There is no account on this machine, so nothing exercised
`createCall`, no `updateCall` ever arrived, no key was ever handed to
NTgCalls, and not one frame or packet crossed the wire. What is proved is the
join: the worker walked through every state the wire can send, against the
fake transport and the fake engine, and the requests it sent were read back
field by field — the key out of its base64, a reflector's peer tag and `tcp`,
a WebRTC server's credentials and `turn`, the library versions, the relay both
ways, the duration and `is_video` on a discard. The engine's own five steps,
the device names it picks and what its callbacks report are compiled and
clippy-clean and are Andrey's to run on a Mac with an account.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib` and `calls`);
`cargo build -p superapp` links and the binary runs; `cargo test --workspace
--locked --no-default-features` 1391 + 412 + 2, no failures; `MAKEPAD=headless
cargo build -p superapp --no-default-features` and `./e2e/run-all.sh` — 115
suites, no failures.

## Progress — phase 6 (2026-09-15)

**Calls on the phone** are built — built and linked, which is as far as a
machine with no phone plugged into it can take them. What is here is the
library, the link, the engine on android, the audio route and the words.

- **The library without a JavaVM.** `build-tools/ntgcalls-android.sh` grew a
  fifth fix, under `SUPERAPP_NO_JVM`, which the wrapper toolchain file puts in
  the compiler's flags. Upstream's android is the AAR's: a `JNI_OnLoad`
  registers the VM, Java classes of theirs carry the camera and the hardware
  codecs, and webrtc's own `JNI_OnLoad` is what starts OpenSSL. The C binding
  has none of that, and `webrtc::AttachCurrentThreadIfNeeded` does not answer
  *no VM* — it aborts. So three files say there is none:
  `wrtc/src/utils/java_context.cpp` (`GetJNIEnv` answers `nullptr`),
  `wrtc/src/interfaces/peer_connection/peer_connection_factory.cpp` (the video
  encoder and decoder factories are `webrtc::CreateBuiltinVideo*Factory()`,
  libwebrtc's own software ones, and `webrtc::InitializeSSL()` is called here
  since nothing else will) and
  `ntgcalls/src/media/devices/java_video_capturer_module.cpp`
  (`is_supported` answers false, which is the whole of an empty camera list,
  an empty screen list and a refusal to open either). The rebuild is
  incremental — the script keeps `src/` and `build/` — and the check at the
  end of the script is now two: 75 exported `ntg_*` functions as before, and
  not one undefined symbol naming a JNI, a JVM or a `Java` class. `NEEDED` is
  seven system libraries (`libandroid`, `liblog`, `libOpenSLES`, `libEGL`,
  `libm`, `libdl`, `libc`), none of which is in the NDK sysroot's base
  directory, so cargo-makepad bundles nothing beside it. The library is 20 MB
  stripped rather than 16: the software codecs are libvpx.
- **Linking.** The `ntgcalls` dependency moved to
  `cfg(any(target_os = "macos", target_os = "android"))`, and `build.rs` sets
  one cfg for the pair of conditions the app asks about —
  `calls_engine`, which is the `calls` feature *and* one of those two targets.
  `ntgcalls-sys` adds its own link search path off `NTGCALLS_LIB_DIR`, so
  `app/build.rs` does nothing at all for android; `./android.sh` finds the
  prefix (`NTGCALLS_DIR`, then `~/.cache/superapp-ntgcalls-android`), exports
  `NTGCALLS_DYLIB=1` and `NTGCALLS_LIB_DIR`, and copies `libntgcalls.so` into
  the cargo output directory beside `libtdjson.so`, which is where
  cargo-makepad packages shared libraries from. `--no-calls` builds Telegram
  without the engine; `--no-tdlib` takes the engine with it, there being
  nothing to ring through, and both are now spelled as an explicit feature
  list rather than as `--no-default-features` alone.
- **The engine on android.** `NtgEngine` is compiled for both platforms and
  the *only* difference is the camera. The microphone and the speaker are the
  library's own devices on both — on android `ntg_get_media_devices` answers
  two `default` devices whose metadata says `is_microphone`, and the engine
  already passed that metadata string as the device's `input`, so the code
  that picks them did not change. The camera it has none of, so
  `camera_is_ours()` is true there: the description says
  `NTG_MEDIA_SOURCE_EXTERNAL`, makepad holds the session, and every frame is
  pushed in with `send_external_frame`.
  - The frames reach it through a **tap** on the kernel's `Capture`:
    `watch_frames(Option<FrameTap>)`, a defaulted method that does nothing, so
    the fake and every scripted run are untouched. `platform/senses.rs` calls
    it from the camera callback with the lock let go, beside the copy it
    already keeps for a photograph.
  - Who leaves the tap is the **worker**, not the panel:
    `Account::hold_camera` opens the camera and leaves the tap when
    `callStateReady` says the call has video, takes both back when the call
    ends, and follows the `camera` verb. It asks the engine whether it wants
    frames at all rather than asking the platform, so there is no `cfg` in
    `sync/calls.rs` — on a Mac `frames_wanted()` answers `None` and the whole
    thing is a no-op.
  - Only the newest frame is ever waiting: the command channel carries a
    marker and the frame itself sits in a one-slot mutex the task takes from.
    A phone makes thirty pictures a second and a queue of them is how a phone
    runs out of memory.
  - **The self-preview** on the phone is phase 3's `MediaCamera` through
    `media::show_camera`, pointed at the same open camera the call is being
    sent from, because nothing comes back to draw. The Mac keeps the engine's
    own capture frames, as phase 5 built it.
- **The audio route** is `app/src/platform/audio_route.rs`, beside
  `browser.rs` and in its shape: android over JNI, everywhere else nothing at
  all. `in_call(true)` sets `AudioManager.MODE_IN_COMMUNICATION` as
  `callStateReady` arrives — before the engine is started, because android
  decides where a stream goes when the stream opens — and `in_call(false)`
  puts the mode back to `MODE_NORMAL` and the speakerphone off however the
  call ended. The bar's `speaker` (`p`) is `setSpeakerphoneOn`, on android
  alone; the earpiece is where a call starts, as on the phone's own client.
  The manifest already carried `MODIFY_AUDIO_SETTINGS`, `RECORD_AUDIO` and
  `CAMERA`.
- **The words.** `calls::available()` is `cfg!(calls_engine)`, so a build with
  the feature no longer says *calls are not available on this device yet* on
  the phone. A `--no-calls` build still does, on the person's card and on the
  call panel, and an incoming call there still rings and can still be
  declined.

### Deviations from the sketch

- **`cfg(calls_engine)`, set by `build.rs`**, rather than
  `all(feature = "calls", any(target_os = "macos", target_os = "android"))`
  spelled out in six places. It is the other half of the manifest's target
  gate and the two are kept in step in one file.
- **`InitializeSSL` is part of the patch.** The sketch names the codec
  factories and the camera list; without this third change there would be no
  DTLS at all, since `get_or_create_default` deliberately skips it on android
  and leaves it to webrtc's `JNI_OnLoad`.
- **The route is set at `callStateReady`, not at the first *connected*.**
  Android picks a route when a stream opens, so a mode set once the media has
  connected would route the next call rather than this one.
- **`speaker on` / `speaker off`**, not a plain `speaker`: the bar already
  says `camera on` / `camera off`, where the label is what pressing it does.
  Both carry the `p`, which is what the bar-letter test asks.
- **The hardware video codecs are gone with the JavaVM.** `DefaultVideoEncoderFactory`
  is Java, so the phone encodes and decodes in software — which for this
  libwebrtc build is VP8, VP9 and AV1, and *not* H.264, since their android
  build has `rtc_use_h264` off. A voice call does not care; a video call
  depends on the other client offering one of the three.

### What could not be verified here

**No call was made, and no phone was plugged in.** What is proved is that the
library builds without reaching for a JavaVM, that it exports the C API and
asks for nothing but system libraries, that the app links against it for
`aarch64-linux-android`, and that the APK carries it. Everything the library
*does* at runtime is Andrey's to find out on the Fold:

- whether the native audio really opens — the Oboe path this build links,
  through NTgCalls' own `AudioDeviceModule`. A call that connects and is
  silent both ways is this; `./android.sh logcat` would show `Oboe` or
  `AAudio` lines around the moment the call goes *connecting*. (The fallback
  the sketch named is external frames from makepad's microphone and to its
  output, which is the same tap the camera now uses.)
- whether the **software codecs** negotiate. A voice call should not care. A
  video call where the other side insists on H.264 will connect, carry the
  voice and show a black box; the sign is a `No video encoder`/no common
  codec line rather than a crash.
- whether anything **still reaches for the VM**. If the patch missed a path,
  the app dies at `callStateReady` with a native abort inside
  `libntgcalls.so` and a `Check failed: g_jvm` or `JNI_OnLoad failed to run?`
  line in `logcat` — that is the one failure to look for first, and it would
  happen on an audio call as readily as a video one.
- the **picture's orientation**. A frame is stamped `VideoRotation0` and the
  phone's sensor is usually a quarter turn from the screen, so the other side
  may see the call sideways. Nothing here can tell.
- and the whole of TDLib's half, which no build on this machine has ever run.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib` and `calls`);
`cargo test --workspace --locked --no-default-features` 1421 + 2 + 414 passed,
0 failed; `MAKEPAD=headless cargo build -p superapp --no-default-features` and
`./e2e/run-all.sh` — 117 suites, no failures. `TDLIB_DIR=… ./android.sh build`
produces `target/android/makepad-android-apk/superapp/apk/superapp.apk`, whose
`lib/arm64-v8a/` carries `libntgcalls.so` (20,528,696 bytes, the very file the
prefix holds) beside `libtdjson.so`, and whose `libmakepad.so` names
`libntgcalls.so` in its `NEEDED`; `./android.sh build --no-calls` produces one
that names and carries neither. (Two tests fail now and again under a loaded
parallel run and pass alone — `apps::workshop::snapshots` with an empty
`Git: `, as phase 7 recorded, and `platform::watch` and `apps::mail::selection`
on their wall-clock deadlines.)

## Review — 2026-09-15

The whole of phases 1–5 and 7 read against the reference behaviours above and
against the report they came from. What holds, in short: the waveform's
hundred five-bit bars in sixty-three bytes against 1.8× the mean floored at
2500; Opus at 48 kHz mono in 20 ms frames at 30 kbps with the half-second
floor; the photo at quality 80 capped at 1280; the video message square at
384, a megabit, thirty frames, AAC mono at 48 kHz and 64 kbps, a 320-square
poster, a minute's cap; the album of two to ten with the caption on the
first; the live rule (a metre and ten seconds, the heading while moving, the
stop as an edit, the period's end as a local drop) and the four periods with
`0x7FFFFFFF`; `https://maps.google.com/maps?q=<lat>,<lon>`; the call words in
both places, `min_layer 65` / `max_layer 92`, `need_rating` gating `rate`, the
five engine steps in order and the signalling relayed both ways. No `TODO`,
`todo!` or `unimplemented!` anywhere in the change. What was small and wrong
is fixed in this phase (below); what is larger is here, each with what the
pass that followed did about it (2026-09-15).

**Bars.** No bar in the change wears a reserved letter or the same letter
twice, and every letter is in its own label — but `play` is drawn as `pause`
while it runs, and there is no `y` in *pause*
(`app/src/apps/telegram/panels/line.rs:202`, `panels/media.rs:294`,
`app/src/shell/widgets/viewer/control.rs:54`). It predates this change and
the letter is in the book (`viewers.md`), which is why it is listed rather
than changed. Neither guard can see it: `bar::check`
(`app/src/shell/bar.rs:232`) tests only *reserved* and *twice*, and the app's
`check_bar` (`app/src/apps/telegram/tests.rs:1181`) only ever asks a panel at
rest. Proposed: `a`, which is in both words and free on all three bars, and
the letter-in-label test moved into `bar::check` so a debug draw catches
every bar in every state.

**Places.**

- `expires_in` is dropped on a *fetched* live location.
  `updates::live_location_media` (`app/src/apps/telegram/updates.rs:457`)
  computes `date + live_period` and never reads it, though this TDLib carries
  it on `messageLiveLocation`; `updates::live_expiry` (`updates.rs:471`)
  exists and is wired only to the edit path (`sync.rs:1355`). Proposed: hand
  `updates::message` the clock the projection already has and prefer
  `now + expires_in` where the wire gives one. **Fixed:** `updates::message`
  and `updates::content` take the projection's clock, and a live location's
  end is `now + expires_in` where the wire gives one and `date + period`
  where it does not — both readings through `model::live_end`. Tested in
  `updates.rs` against a page fetched half an hour into a share.
- The phone's five-second grace is absent. Android expires a share at
  `period - 5` when `period % 60 != 0` — how it copes with Apple's 3599 for
  an hour — and never for `0x7FFFFFFF`. (The sketch above has the direction
  backwards: the phone ends it *earlier*, not later.) Proposed: keep the
  period beside the expiry so the rule can live next to `live_left`, or drop
  the requirement — it decides one second in when a row says *ended*.
  **Fixed:** `model::live_end(end, period)`, beside `live_left`, is the one
  place the rule lives, and every expiry there is — a fetched line's, an
  edit's, the worker's own share — goes through it. *Places, live* above says
  which way the five seconds go now.
- The first edit of a share is redundant. `worth_sending`
  (`app/src/apps/telegram/sync/live.rs:236`) answers yes for the first pass
  after a share is learned, so an `editMessageLiveLocation` goes out carrying
  the fix the `sendMessage` a moment earlier already carried. The clients
  seed the baseline from the message they sent. Proposed: record the fix and
  the time when the share is learned — the echo carries the location.
  **Fixed:** `updates::live_pin` reads the pin and the date it was put down
  off the message itself, and that is a share's baseline from the moment it
  is learned — so the first pass adds nothing, and a share restored on
  sign-in, whose pin is old, moves on the first reading that differs.
- `dyn Tiles` in the capability bag is written and never read
  (`kernel/src/caps/mod.rs:1102`, `app/src/shell/boot.rs:552`): every reader
  goes through the process-wide handle, because a map is composed on a worker
  with no world. Proposed: drop it from the bag and keep the one handle.
  **Fixed:** no world carries a `Tiles` now — neither the kernel's fake nor
  the shell's OpenStreetMap — and `caps/tiles.rs` says why it is the one
  capability that is in no bag. The gate is `boot::tiles_for`, with a test
  that a scripted run is given none.

**Captures.**

- An album is capped rather than split. `requests::parcels`
  (`app/src/apps/telegram/requests.rs:311`) takes the first ten pictures as
  one album and sends the rest one message each
  (`app/src/apps/telegram/tests.rs:4181`); the clients split into albums of
  ten. Proposed: chunk. **Fixed:** `parcels` cuts the pictures into albums of
  ten in the order they were carried; an odd one at the end has no album to
  be in and goes as a picture.
- The camera's frame rate is assumed. `choices`
  (`app/src/platform/senses.rs:760`) ranks formats by pixel format and area
  and ignores `frame_rate`, while the file declares thirty frames a second,
  stamps them by a counter (`app/src/platform/senses/circle.rs:89`) and puts
  `frames / 30` on the wire as the duration (`senses.rs:748`). A camera that
  answers 24 or 60 gives a picture that runs against its own sound, a wrong
  duration, and a cap that is not a minute. Proposed: prefer a format near
  thirty, or stamp each frame with the time it arrived and take the length
  off the clock. **Fixed:** both. `choices` ranks a format by how near its
  `frame_rate` is to thirty before it ranks it by size; every frame carries
  the moment the camera handed it over, both encoders take that as their
  timestamp (`push_frame(nv12, at)`, on the Mac and on the phone), and the
  minute's cap and the length are read off the same clock (`circle_secs`).
- A keyframe a second is set on the phone and not on the Mac — named in
  phase 1 already; makepad's `VideoFileEncoderOptions` carries `keyframe_only`
  and nothing between it and the default. Proposed: add the field to the
  fork, or leave VideoToolbox's own GOP, which plays everywhere.
- A photo added from the files app is sent byte for byte
  (`requests.rs:274`); only a camera shot goes through the 1280/q80 rule.
  The clients re-encode both. Proposed: nothing, until a phone's own gallery
  is a source.

**Calls.**

- CI never compiles the real binding. Every job passes
  `--no-default-features`, so `app/src/apps/telegram/calls/ntg.rs` is checked
  only by a default-feature build on somebody's Mac. Proposed: one
  `cargo check -p superapp --features tdlib,calls` step on the macOS runner.
  **Fixed:** that step, named *check the engines*. A check links nothing, so
  the runner needs no `libtdjson`; `app/build.rs` fetches NTgCalls' shared
  library off the project's own release the first time a checkout is built,
  which is a dozen megabytes and the one step in this job that reaches the
  network. From a cleared `target/ntgcalls` here the step takes nine seconds,
  fetch included, so it is not the fragility it looked like.
- `updateMessageContent` carrying a `messageCall` would read as *incoming*:
  `sync.rs:1351` goes through `updates::content` directly and misses the
  prefix `updates::message` adds (`updates.rs:36`). Latent — TDLib does not
  edit a call's content. **Fixed:** `updates::call_way` is the one place the
  direction is written in front of the words, and `on_message_content` puts
  an edited call through it with the direction the row it is editing holds.
- `unique_id` is decoded and stored and read by nothing
  (`app/src/apps/telegram/runtime.rs:264`, `updates.rs:622`); `Reason::Empty`
  and `Reason::UpgradeToGroupCall` are decoded and then indistinguishable
  from a hang-up in both the line and the panel. **Fixed:** `unique_id` is
  gone — the installed TDLib's `InputCall` is `inputCallDiscarded`, which
  names a call by its `id`, or `inputCallFromMessage`, so nothing can ever
  want it — and the two reasons have words of their own in both places:
  *call* alone where the wire says nothing about how it ended, *moved to a
  group call* where the two were carried into one.
- The fifth bar verb, `speaker`, and the phone's audio route belong to the
  phase that carries a call on the phone, and are not here.

**Fakes, and what a script may touch.** The engine, the receiver, the camera,
the microphone, the tile server and the browser are all behind the same
gate — a world that is nobody's gets the fake — and three holes in that gate
were found and closed in this phase (below). Two remain:

- The camera preview is pointed at the platform whatever the run is:
  `telegram/widgets/attach.rs:184` hands any camera id to `set_source_camera`,
  and under a script the id is `FakeCapture`'s sentinel
  (`kernel/src/caps/senses.rs:378`), which no device answers. Headless drops
  the op; a windowed scripted run would raise the camera permission dialog
  before failing to find it. Proposed: the fake answers no camera id at all
  and the panel draws an empty box, which is what a suite sees anyway.
  **Fixed:** the fake answers no `CameraId` at all, and what a recording
  waits for and the attach panel's line reads is the new
  `Capture::camera_open` — open is not the same as having a camera to point a
  widget at. The call panel's `camera()` (phase 6) reaches the same
  capability and is covered by the same answer.
- A link clicked in a message opens the browser with no test of the run
  (`app/src/apps/telegram/widgets/text.rs:55`), unlike the map's ways out,
  which go through `map_wish` and refuse a world that delivers nothing. The
  same shape is in mail, rss, calendar, workshop, the agent and the shell's
  viewer, so it is not this change's alone; the suites avoid it by dragging
  rather than clicking. Proposed: one opener in the shell that weighs
  `Delivery`.

**Tests that would have caught something and do not exist.** The half-second
floor is enforced only in code that needs a microphone
(`app/src/platform/senses.rs:596`) and is asserted nowhere; JPEG quality 80
is asserted nowhere; the call panel's `[close, rate]` bar and the `failed to
connect · …` line are not walked; nothing asserts the `© OpenStreetMap`
credit or the `!scripted` gate on the real tile source. **Fixed:** the floor
is `opus_ogg::long_enough`, which both microphones — the platform's and the
fake — now answer to, with the words and the number asserted beside it; the
quality and the two caps are asserted against the codec's own constants;
the call panel's bar is walked at rest *and* over `[close, rate]` and the
error's `failed to connect · …`; the credit is proved by building a
`MediaMap` out of the live design and reading the label under the picture,
since the two are one template and neither can be drawn without the other;
and `boot::tiles_for` says in a test that a scripted run gets no tiles of
its own. The format choice and a recording's length, which changed above,
have tests of their own in `platform/senses/tests.rs`.

## Progress — phase 8 (2026-09-15)

**Reviewed**: phases 1–5 and 7, the code rather than the notes —
`kernel/src/caps/{senses,tiles}.rs`, `kernel/src/codec/`,
`app/src/platform/senses{.rs,/}`, `app/src/shell/{tiles,sound}.rs`,
`app/src/shell/widgets/{map,media}.rs` and `media/opus.rs`, the telegram
`{panels,widgets}/{attach,place,call,line,media,chat}.rs`, `requests.rs`,
`updates.rs`, `model.rs`, `sync/{live,calls}.rs`, `calls/`, `scenes.rs`, the
three suites, and the bars of every panel this change touches — against the
reference behaviours and the report they came from. What holds and what does
not is the *Review* above; this is what was done about it.

**Fixed, with a test where one fitted:**

- `live_left` (`model.rs`) had no case for the fourth period, so *until
  stopped* read `596523 h left` on the place panel, the chat's status line
  and every received line. It says *until stopped* now, a day being where a
  countdown ends and the forever period begins.
- `end` pressed before the wire had named a call sent
  `discardCall{call_id: 0}`, which TDLib refuses; the call went on ringing
  the other side and the next `updateCall` stood the row back up at
  *contacting…*. The panel now sends nothing while the id is nought, and
  `on_call` (`sync/calls.rs`) discards the call the moment the wire names it.
- `on_ready` cleared the call rows without telling the engine, which would
  have carried a call no row could end. It stops each one first.
- `--library` given with `--e2e` left a stage unscripted (`boot.rs`), so a
  suite that put the canvas away would have come up on the machine's
  keychain, its receiver and camera, the real clipboard and OpenStreetMap's
  servers. A script is the *run's* now, not the stage's.
- The sound out was served on every event of every run (`stage.rs`); a
  scripted run must not be heard by whoever ran it. It is served only where
  there is no script, and a deviceless run moves the position by the clock
  as it always did.
- A call's sounds were gated on the store having a directory — which a
  scripted run *has*, under the system's temp. They are weighed by
  `Delivery` now, the same test a send and a map's way out are weighed by,
  and the two comments that said otherwise (`calls/sounds.rs`,
  `e2e/telegram/calls.txt`) say what is true.
- `waveform.rs`: `PACKED` is the arithmetic that reaches sixty-three rather
  than the number; the unreachable guard in `pack` is gone and its comment
  with it; the comment over the binning claimed something the code does not
  do. A new test pins the hundred and the sixty-three to the wire's own
  numbers and walks the last bar across the two bytes it lands in.
- `opus_ogg::Head::parse` reported a channel mapping family as a channel
  count; `Capture::stop_voice`'s doc promised a floor the fake does not
  keep; `Media::line` re-bound a word it already had. Doc comments were
  added where their neighbours all had one (`FakeLocation::new`,
  `FakeCapture::new`, `write`, `Senses::new`, both `Encoder`s,
  `Attach::in_topic`/`cursor`/`set_cursor`, `Place::in_topic`).
- The comment beside the person's card said neither `o` nor `c` is in
  *call*; `c` is, and is the chat's.
- New tests: the call requests' JSON, field by field, against the names
  TDLib knows (`createCall`, `acceptCall`, `sendCallRating`'s
  `inputCallDiscarded` wrapper) — nothing read one before; the tile address
  and the user agent, which the canned fetcher used to throw away.

**Written**: `telegram.md` gained *What goes with a message* (the carried
list, the album, and the three captures), *Places* (the panel, the four
periods, the worker's rule, the status line, a received line, the three ways
out) and *Calls* (the card's verbs, the panel's states and bar, the emoji,
the sounds, the lines an ended call leaves, and NTgCalls behind the `calls`
feature); the paragraph saying recording and location sharing are
unavailable and the map is a demo is gone. `media.md` gained *A voice note,
heard* (the second driver and the mixer), *The map* (the tiles, the cache,
the incomplete snapshot, the credit) and *Captures* (the three encodings,
where the files live, the sweep, the camera box and the meter), and no
longer says a voice note is what this build cannot decode.
`architecture.md` gained *The senses* — the two senses and the tiles among
the capabilities, their fakes, and the wish the stage serves — and the
module tables name `codec/`, `sound.rs` and `tiles.rs`. `dev-x.md` gained
*Calls* (the feature, the fetched dylib, `--no-default-features`) and *What
a run writes beside its store*, and the android section says why libopus
needs a CMake toolchain file of ours. `vocabulary.md` gained *sense*, *fix*,
*capture* and *live share*, and says that Telegram's *call* is not the
agent's. `open-questions.md` gained the tile server and how much a call
should say out loud. The capability lists in `apps.md` and
`data-substrate.md` count ten now, not seven.

The android recipe for calls is phase 6's and is not written here.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib` and `calls`);
`cargo test --workspace --locked --no-default-features` — 1424 + 415 + 2
passed, 0 failed; `MAKEPAD=headless cargo build -p superapp
--no-default-features` then `./e2e/run-all.sh` — 117 suites, no failures;
`mdbook build docs/book` clean.

## Review fixes — 2026-09-15

Seven findings from a second reading of phases 5 and 6, each with a test
where the code can be tested without a device — which is the fakes and the
pure `land` / `work` halves of the senses.

- **The remote picture.** `playback()` had `camera: None`, and NTgCalls hands
  a received video track to `on_frames` only while the *playback*
  description carries a camera, so the far side never arrived and the panel
  drew an empty box. It takes the flag now — `MediaSource::External`, which
  is the only source that side allows — and a `camera on` mid-call opens the
  sink where the call began without one. It is not taken away again by a
  `camera off`: whether *they* are sending a picture is theirs to decide.
- **Hanging up.** The engine was stopped only by TDLib's terminal
  `updateCall`, so a person who had hung up went on being heard for as long
  as the wire took to agree. `Account::discarding` tears the media down as
  the `discardCall` goes out — the worker's one outbound boundary, so *end*,
  *decline* and the worker's own discard all reach it — and the row still
  says *hanging up* until the wire says which ending it was. `over` is
  called twice for one call now, so the camera is given back by whoever
  holds it and an engine already stopped is stopped again harmlessly.
- **The phone's microphone.** NTgCalls captures through its own library, so
  `Permission::AudioInput` was never asked for and a first call on the phone
  would have carried silence. `Capture::ask_microphone` asks and opens
  nothing, and the worker asks the moment a call appears — *contacting…* or
  *incoming* — together with the camera for a video call, so both are
  answered before `callStateReady`.
- **The first recording.** The input opened as soon as the devices were
  known, which is before the permission dialog is answered, and a later
  grant never reopened it. A `Granted` now makes what is open *stale* and
  the next `work()` closes it and opens it again — `perform` runs the closes
  before the opens, so one pass is both.
- **One camera, two panels.** `close_camera` was a flag, so any panel's
  clean-up dropped a camera another panel had opened. It is a count in both
  capabilities now — the wish stands while any hold remains — and the attach
  panel remembers whether it is one of the holders, so a refused recording
  gives back its own hold and never another's, and a voice note's refusal
  touches the camera not at all.
- **A live share in a topic.** `send_live` went bare while `send_place` went
  through `requests::in_topic`, so a share opened from a forum topic landed
  in the group's general history and went on moving there. It is wrapped the
  same way.
- **A refusal that came later.** The place panel read the receiver's refusal
  once, at its opening, so a denial arriving afterwards left *finding you…*
  standing for good. `Location::trouble` answers what is wrong now, and
  `refusal()` and `where_line` read it on every draw.

The playback description's **audio** field was checked against NTgCalls'
source and left exactly as it was, on the reading that
`StreamManager::set_stream_sources` configures `desc.speaker` as the Speaker
device in every mode. **That reading was wrong**, and the round below fixes
it. `set_stream_sources` does take both fields in both modes, but a P2P call
adds six tracks and its playback three are `Microphone`, `Camera` and
`Screen` (`p2p_call.cpp:222`) — there is no playback *speaker* track in one
at all — and `StreamManager::optimize_sources` turns the incoming audio on
only where a writer sits under `Microphone`
(`enable_audio_incoming(writers_.contains(Microphone) ||
external_writers_.contains(Microphone))`, `stream_manager.cpp:87`). So a
description that named the speaker registered a writer nothing was ever
routed to, and every call was silent. The speaker's own *device* is still
what the description carries — a playback audio description is built with
`MediaSourceFactory::from_audio_output`, which reads the `input` field as
the output to write to — it is the field that had to be `microphone`.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib` and `calls`);
`cargo test --workspace --locked --no-default-features` — 1437 + 419 + 2
passed, 0 failed; `MAKEPAD=headless cargo build -p superapp
--no-default-features` then `./e2e/run-all.sh` — 117 suites, no failures.

## Review fixes — 2026-09-15, second round

Six more findings from a reading of phases 5 and 6 against NTgCalls' own
source and the two platforms' permission dialogs. Each has a test where the
code can be tested without a device, which is the fakes, the fake engine and
the pure `land` / `work` halves of the senses.

- **A call carried no sound.** The playback description put its audio in
  `speaker`, and a P2P call has no playback speaker track to put it in: the
  incoming voice arrives on `Microphone`, and the library enables it only
  while a writer is registered there. The field is `microphone` now — 48 kHz,
  two channels, the *speaker* device's metadata as its `input`, which is
  what `from_audio_output` reads — and `speaker` is `None`. The paragraph
  above that said the field was right says what the wiring is instead.
- **A far side that turned its camera on mid-call was never seen.** The
  external camera sink was in the playback description only while *our*
  camera was on, so an audio call had nowhere for their picture to arrive
  and the `camera` verb had to open the sink as a side effect. Every call's
  playback description carries the camera now, and `Cmd::Camera` sets the
  capture sources and nothing else. Both are read back in a test in
  `calls/ntg.rs`, which the full-feature clippy step compiles.
- **A call started before the microphone was granted.** `callStateReady`
  arriving while the dialog was still open started the engine, whose capture
  opens the device itself and then hands over silence for the whole call.
  `Capture::microphone_allowed` answers `None` while unanswered, and a
  *ready* with no answer waits on the row (`Call::pending_ready`) until the
  person answers or twenty seconds pass — the wire rings for longer. A
  refusal starts it anyway: the call carries, and the panel's line says *the
  microphone is not allowed* beside the timer.
- **Two permission dialogs at once.** A first video call pushed `Camera` and
  `AudioInput` into one pass, and android cancels the second request while
  the first is up and never answers it. The senses keep a queue: one dialog
  is outstanding at a time, and a landed `PermissionResult` — for any
  permission — is what lets the next one out.
- **A real location's refusal was invisible.** `RealLocation` never
  overrode `Location::trouble`, so the one capability the place panel reads
  on every draw answered `None` on the platform and only the fake ever said
  anything. It answers what the receiver's own error and the permission
  result landed.
- **A queued *ready* restarted a hung-up call.** The wire repeats itself,
  and any non-terminal state arriving for a row already *hanging up*
  overwrote it, started the engine again and reopened the camera. From
  `HangingUp` on, only the wire's own endings — `Discarded` and `Error` —
  may move a row.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib` and `calls`);
`cargo test --workspace --locked --no-default-features` — 1466 + 420 + 2
passed, 0 failed; `MAKEPAD=headless cargo build -p superapp
--no-default-features` then `./e2e/run-all.sh` — 118 suites, no failures.
Nothing here was run against a call: what is proved is the join and the
descriptions' fields, and the library's own behaviour is still Andrey's to
find out on a Mac and on the Fold.

## Review fixes — 2026-09-15, third round

Two findings from a reading of the call against its own waiting, both about
what a call is *started* with — which was the wire's word for it and is the
row's now.

- **A choice made while the *ready* waited was lost.** *mute* and *camera*
  are on the bar for the whole of the wait on the microphone's permission,
  and a wish went straight to an engine that has no such call — NTgCalls
  drops it, there is nothing to apply it to — while the start read the
  wire's flags, so the call went on to carry the voice and the picture the
  row said were off. The row is the truth now: a wish lands on it, the
  engine is told only while it is holding the call (`Call::carrying`), and
  `begin` hands it the row's `muted` and `camera`. A *ready* the wire
  repeats no longer puts the camera back on either. In the engine the mute
  is asked for after `connect_p2p`, because the library's mute is a state on
  the outgoing tracks (`StreamManager::update_mute`) and those are made with
  the connection (`P2PCall::connect`) — asked for earlier it would sit on
  nothing.
- **A refused or unanswered microphone threw the call away.** The capture
  named the device whatever the platform had said, and the library throws
  where a device it was handed cannot be opened, so `start` erred and the
  call was discarded — the one outcome worse than a call with no voice going
  out. `Ready::microphone` is false for a refusal and for a wait that ran
  out, `capture()` names no microphone then, and the call connects and says
  which it is: *the microphone is not allowed*, or *the microphone has not
  been allowed yet* where the dialog is still standing. A permission granted
  while the call runs is picked up by a pass of its own
  (`Account::grant_microphone` → `CallEngine::microphone`, which sets the
  capture description again with this end's mute back over it), and the line
  goes away with it.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib` and `calls`,
which is the step that compiles `calls/ntg.rs`);
`cargo test --workspace --locked --no-default-features` — 1487 + 420 + 2
passed, 0 failed; `MAKEPAD=headless cargo build -p superapp
--no-default-features` then `./e2e/run-all.sh` — 118 suites, no failures.
Still nothing run against a real call: what is proved is the join, the row
and the descriptions' fields.

## Review fixes — 2026-09-15, fourth round

Two findings about waiting, both from a reading of the round above against
the clocks the two sides actually keep.

- **The wait outlived the call it was waiting for.** `READY_WAIT` held a
  *ready* for twenty seconds while NTgCalls and the peer's tgcalls give a
  connection about ten before they throw it away, so a call parked on the
  microphone's dialog was a call the other side had given up on by the time
  it started. Nothing is parked now: the wire's *ready* starts the engine in
  the same breath, with the microphone where the permission is already
  granted and with none named where it is refused or unanswered — the fourth
  round's micless start, which the row says in its own words (*the
  microphone is not allowed*, *…has not been allowed yet*) and which the
  late grant still turns on mid-call. `Call::pending_ready` and
  `start_waiting` are gone with the constant, `carrying()` is the row's
  state alone, and only the *first* `callStateReady` starts anything: a
  repeat would have started a second engine, put the camera back on and
  rewound a connected row to *connecting*.
- **A grant made in the platform's settings was invisible.** A refusal was
  cached in `microphone_answer` and nothing ever looked again, so every
  later call in the run carried silence — and neither platform says a word
  when a person goes to the settings panel and allows it. The senses now
  *look*: makepad's `check_permission`, which raises no dialog on either
  platform and answers with the same `PermissionResult`, goes through the
  same one-at-a-time queue (`Ask { permission, dialog }`, and `work` puts it
  in `check` rather than `ask`). A wish that meets a cached refusal looks —
  the call appearing, the call already carrying and hearing nothing (polled
  from `grant_microphone`, no oftener than three seconds), the camera's next
  `open_camera` — and android's *denied, ask me again* buys one more real
  dialog the next time a call appears, and only one. A `NotDetermined`
  result, which only a check can produce, is no answer at all now and leaves
  a refusal standing.

Verified: `cargo clippy --workspace --all-targets --locked
--no-default-features -- -D warnings` clean; `cargo clippy -p superapp
--all-targets --locked -- -D warnings` clean (with `tdlib` and `calls`);
`cargo test --workspace --locked --no-default-features` — 1489 + 420 + 2
passed, 0 failed; `MAKEPAD=headless cargo build -p superapp
--no-default-features` then `./e2e/run-all.sh` — 118 suites, no failures.
Still nothing run against a real call, and no permission dialog raised on
either platform here: what is proved is the row, the queue and the words.

## Follow-up — 2026-09-15: the picture's orientation

**The finding.** Three paths carried a picture and not one of them turned
it: the photo written from the camera's newest frame, the video message's
frames, and the call's, in both directions. A phone's sensor is mounted a
quarter turn from its screen, so all three were sideways on the phone and
sideways at the far end of a call. The comment saying nothing here could
tell which way up a frame was lying was literally true — makepad's fork
never surfaced the sensor's mounting at all — and the fork's own preview
made it worse, turning frames by a rule of its own while knowing neither the
screen's rotation nor which way the lens faced. The call's remote box was a
fourth thing: `height: Fit` over `ImageFit.Horizontal` is the right box for
a landscape picture and three screens tall for the 9:16 a phone held upright
sends.

**The fix.** In the fork: `VideoInputDesc::sensor_orientation` reports
Android's `SENSOR_ORIENTATION` — nought wherever frames already arrive
upright — `VideoRef::set_uniform` lets a caller turn and mirror a preview in
its own shader, and the fork's preview no longer turns anything by itself.
In the app: `CameraId` carries `turns` and `front`, worked out by CameraX's
own rule over the sensor's mounting, the lens facing and
`Display.getRotation()`, from what the device reports and never assumed.
The screen is read when a session opens, on every geometry change, and —
on the phone, while a camera is open — every half second on a clock,
because a phone turned end over end changes no geometry and raises no
configuration change: the platform's own guidance is a `DisplayListener`,
which is Java this app has none of, so it asks instead.
Photos and video messages are turned upright before they are written and,
from the front camera, mirrored, so that what is sent is the picture the
person was looking at. A call sends its frames as the sensor made them with
the turn stamped beside them (`FrameData.rotation`) — what every phone
client sends, and what libwebrtc itself spends when the far side negotiated
no orientation extension; a frame arriving is turned by the quarters the
wire sent with it; the library's own capture, the Mac's camera looking at
the person, is mirrored, a self-view being a mirror; and the remote box
takes the panel's remaining height and fits the picture inside it whatever
its shape.

**What is not proved.** No phone was attached for any of this. The turns are
proved by unit tests on synthetic pictures — a three-by-two of six distinct
words turned each of the four ways, and the mirror, row by row — and by the
provenance of the rule that produces them. That each picture is turned the
right way *on the glass* is proved nowhere here; the first phone build is
where the signs will be read.
