# Media

A clip or a sound is a file on this device or an address on the web, drawn
by the platform's own player through one surface and transported by one
strip. One plays at a time. Three hosts embed it: a `<video>` or `<audio>`
in a [reading](./rss.md), an `.mp4` or `.mp3` on a [file card](./files.md),
and a clip in a [Telegram](./telegram.md) chat.

The same kit draws the rest of what a message can be made of: a place on a
map, what the camera sees while a picture is being taken, and the level of a
recording under way.

## The player

The strip is the control: **play** or **pause**, the progress as a filled
hairline, and the time — `0:17 / 0:42`. Press the button to run or hold;
press or drag on the hairline to seek. The box above it shows the poster
until the player has a picture, then the picture. A sound is the strip
alone.

Pressing play anywhere pauses what played before, in whichever panel it was.
A clip also pauses when its panel closes, when its workspace is switched
away, and when its box scrolls wholly out of view. A clip that a page
publishes as a silent moving picture — `autoplay muted`, the browser's own
rule — runs while its box is on the screen, loops if it says so, stays
muted, and stands outside the one-at-a-time rule: it is a picture that
moves, not a sound. `muted` and `loop` hold on their own as well.

A reading starts its media stream when playback starts; posters can load
beforehand through the picture loader. A paused clip lets its player go,
showing its poster again and remembering where it stood; the next press takes
it on from there. Releasing paused players bounds the decoders and buffers a
reading with many clips can hold.

The platform's player takes an address on the web directly and streams it
itself; nothing of a web clip passes through the app or its caches. What it
can play is what the operating system's own players can: MP4, M4V and MOV
with H.264, H.265, AAC, MP3, WAV and FLAC on Apple; those and WebM and Ogg
on Android. A clip in a reading whose declared type the platform refuses
draws `video ↗` or `audio ↗` in its place, a link to the source that opens
in the browser; a source that declares no type is tried. Where a page
offers several sources, the reading keeps the first in a container every
platform plays — by type, or by the address's extension — so a WebM before
an MP4 yields the MP4.

Playing and seeking are not actions in the history; `cmd+z` does not
un-play.

## Readings and file sources

The shared HTML narrowing in `app/src/reader/html.rs` retains `<video>` and
`<audio>` as media items. It resolves their source and poster against the
reading's base URL and keeps the MIME type, size hints, `autoplay`, `muted` and
`loop`. A tag's own usable `src` takes precedence over nested `<source>`
choices. Only HTTP and HTTPS media sources are accepted; `cid:` and `data:`
media are not playable sources.

Fallback content remains as prose after the player. A video without a usable
source retains its poster as a picture. `<track>` subtitles and `<iframe>`
embeds are not rendered. Sanitizer version 6 rebuilds cached Mail and RSS
readings from their stored source, with saved HTML as the legacy RSS fallback.

`ReaderClip` owns one transport and driver per item. Its video box fits the
column within 360×320, using size hints, poster dimensions or the prepared
video's dimensions; it starts at 16:9 when none are known. Transport lives on
the strip, leaving the box available for text selection. Audio hides the box.
Silent autoplay pauses off-screen and resumes on return, unless the person
paused it explicitly. Unsupported declared types or preparation failures show
a link to the source. Headless builds retain the controls without a decoder.

File cards supply a filesystem path to the shared [viewer](./viewers.md).
Video and audio show **play / pause** (`cmd+y`) in place of fit and zoom;
**open** still hands the file to the operating system. Telegram supplies its
own downloaded file and keeps its message navigation and download feedback.
Mail attachment cards currently supply bytes, so their video/audio parts still
require **open**; exposing the cached file to the player remains unfinished.

## A voice note, heard

A voice note is Ogg Opus. That is what every Telegram client records one
as, and it is the one thing nothing Apple ships will open. So the kit has a
second driver: the file is decoded to samples on a worker and played
through a mixer of the shell's own, out of the machine's default output.

Which of the two drivers a file gets is the kit's to decide, not the host's:
a file named `.ogg`, `.oga` or `.opus`, anywhere but Android, whose own
player takes Opus. Everything around it is unchanged — the same strip, the
same transport, the same scrub, and the same rule that one thing plays at a
time, since both drivers answer the one wish on the one transport.

The mixer holds one recording, which is that rule written where the samples
are: taking the slot stops whatever had it, and a panel closing stops the
sound, which is the one thing a wish on a transport cannot do without a
draw. It resamples 48 kHz to whatever rate the device asked for and writes
the recording into every channel. **With no device out** — a headless
build, a machine with no speakers, the moment before the platform has named
one — the position still moves, by the clock the strip is already ticked
against, so a scripted run behaves exactly as a real one does minus the
sound.

A minute of speech is a few million samples, so the decode runs on the
blocking pool; until it lands the strip reads *pause* at `0:00`, as it does
while the platform prepares a clip. A recording that could not be read
reports *not playing*, which puts the button back rather than leaving the
host pressing at it. Nothing opens a speaker until something has a
recording loaded: registering the callback is also what wakes the
platform's enumeration, and a run that never plays a note should never open
one.

## The map

A place is a snapshot of the map around it, at zoom 15, with the pin at its
centre, drawn from OpenStreetMap's raster tiles. Under it stands one muted
line, **© OpenStreetMap**, wherever a map is shown, because the tiles'
licence asks for it.

A snapshot is composed from the tiles that are here. One that is not
answers *pending* and is fetched; the ground colour stands where it will
go, the snapshot says it is incomplete, and it is composed again when the
tile lands — the way a picture appears after its download. A pin that moves
redraws the same way.

What has been fetched is kept: the PNG in the blob cache under
`tile:<z>/<x>/<y>`, so a restart draws the same map without asking for it
again, and sixty-four decoded tiles in memory beside it, so composing a
picture is copying. The tile server's policy is what the code keeps: the
program names itself, its version and its repository in the user agent, at
most two fetches are on the wire at a time, and a tile the server refused is
left alone for a minute. This is one person's map on one person's machine;
a product would want a key of its own.

A scripted run, a test and a panels-library mount get a different source
altogether: a drawn street grid, the same for Lucerne and for Moscow. No
suite reaches the network, and every scene draws the picture it has always
drawn.

## Captures

A capture is a file the camera or the microphone made for the app: a
**photo**, a **voice note**, or a **video message**. Each is written in the
shape Telegram's own clients write it, because every other client has to
play it back.

- A **photo** is the newest camera frame as JPEG at quality 80, at the
  camera's size with its long side capped at 1280 pixels.
- A **voice note** is Opus in Ogg: the microphone resampled to 48 kHz mono
  and encoded in 20 ms frames at 30 kbps in the VoIP profile. Beside it
  goes the **waveform** the clients draw — the recording in 100 slices,
  each slice's peak measured against 1.8 times the mean of the peaks, five
  bits each packed into 63 bytes. The scale has a floor under it, or a quiet
  room's own hiss would be drawn as a shout. Shorter than half a second is
  refused; there is no upper bound.
- A **video message** is a square mp4 — 384 pixels, H.264 at a megabit,
  thirty frames a second, with a mono AAC track at 48 kHz and 64 kbps — and
  a 320-pixel-square JPEG poster beside it. Each frame is cropped to its
  centre square and scaled on the way in. A minute is the cap: the
  recording stops itself there.

Captures are written under `captures/` beside the store, named by the
clock. A discarded one is taken away at once. A sent one is left where it
is, because the engine reads the file while it uploads it; the account
worker sweeps what is older than a day when it starts. A world with no
store to sit beside — a panels-library mount, a test — writes into a
numbered directory of its own under the system's temp, which nobody sweeps
and the system empties itself.

While one is being made the kit draws it: the camera's picture in a square
box, cropped to fill rather than letterboxed, and the microphone's level as
a row of bars.

## Implementation

The kit is `app/src/shell/widgets/media.rs` and the modules beside it.

- **`Source`**: `File(path)` or `Web(url)`, what a host says it is playing.
  A file becomes the platform's filesystem source, an address its network
  source; the Apple and Android backends take both.
- **The surface**, `MediaClip`: the poster, the frames and a note over them,
  in one rectangle fitted to the column at the clip's proportions. The host
  gives it the shape (`surface_aspect`) before the poster is filled, so a
  thumbnail's pixels never decide the box; a first frame never moves what
  is under it. `fill_clip` lends the host's player to the box while there
  is a picture and writes the note.
- **The strip**, `MediaPlayer`, with `fill_player`, `play_rect` and
  `SeekBar`.
- **`Transport`**: the wish, run or hold; what the platform last said of the
  position and the length; and the store's registry of who plays now, held
  weakly so a closed panel never keeps playing. A verb has no `Cx` to reach
  a player through, so the host sets the wish here and the draw makes it
  so. A host with no player at all — a demo line standing for a recording
  no device could fetch — runs a `Timeline` instead, ticked against the
  session's clock, which the same strip draws. The registry is per store,
  which is what keeps two sessions in one test process from pausing each
  other.
- **`Clip`**: the driver. It leases the native player for the life of its
  host (`VideoPlayback`; Makepad's `Video` does not stop its player on
  drop, so a lease retires it to `cleanup_videos` at the app root), hands
  the source over exactly once, keeps the poster up until the player has
  delivered a first frame, and reads the platform's word back. It also
  asks `played_by_kit` whether this file is the platform's at all, and
  where it is not, hands the native player back and drives an `OpusClip`.
- **`OpusClip`**: the other driver. Pointed at a thing by the host's key,
  given a source and a wish, answering the same `ClipDrawn` the native one
  does, over `kernel::codec::opus_ogg::decode` and the shell's mixer.
- **The mixer**, `app/src/shell/sound.rs`: one slot, one callback on the
  platform's audio thread, the linear resample to the device's rate, and
  `Voice`, the claim on the slot that stops the sound when it is dropped.
  The stage serves the audio output the way it serves the senses: it lands
  the device list and installs the callback once something has samples.
- **`Scrub`**: a press on the hairline, the drag that follows, the release,
  with the bar the press landed on kept through the whole drag.
- **`prime_video`**: draws a hidden box's player at no size every frame,
  for Android, which gives a player its texture only on a draw and will
  not prepare a clip before then.
- **`MediaCamera`**, **`MediaMeter`**, **`MediaMap`**: the square camera
  box pointed at whichever camera the capture capability opened, the
  level as bars, and the map snapshot with its credit. The snapshot itself
  is `widgets/map.rs` — the projection, the pin and the ways out — over the
  kernel's `Tiles` capability, whose real implementation is
  `app/src/shell/tiles.rs`.

A host embeds the surface and the strip in its own template, keeps a
`Transport` and a `Clip` per thing it can play, keeps the player in a
hidden holder of its own, registers the button and the hairline in its hit
table, and says where the source is. The reading's item (`reader/clips.rs`)
is its own host; the file viewer hosts one for a card; Telegram's panels
host theirs over a download.

The kit also supplies `MediaPicture`, the recording-level bars in `MediaMeter`,
the camera's square box in `MediaCamera`, and `MediaMap`.
`shell/widgets/map.rs` composes Web Mercator tiles and a pin through the
kernel's `Tiles` capability — OpenStreetMap's own on a real run, the
deterministic street grid under a script; see *The map* below. The
recording controls read the capture capability; see *Captures*.

## Current limits

What the fork does not do yet: report a position without a video frame —
on either platform, since both post the position with a decoded frame —
or, on macOS, the end of a clip. A sound's hairline therefore stands still
while it plays, and on macOS a finished clip's button keeps reading
*pause* until it is pressed. Android reports the end. (The fork's macOS
backend used to give up on a native player that yielded no frame for
sixty polls — still loading, paused or buffering — and hand it to a
software decoder this build does not carry, an error that stopped every
web clip inside a second; since `92125497` only a playing player's
frameless polls count, and without a decoder plugin there is no
fallback.) None of it touches a voice note, which the kit plays itself and
therefore reports on itself.


RSS/Atom enclosures and JSON Feed attachments are not appended to readings;
only media embedded in the selected HTML content appears. Whether to add
enclosures and how to retain paused frames are [open questions](./open-questions.md).
Volume, mute and speed controls, fullscreen, picture-in-picture and subtitles
are not exposed by this kit. Telegram audio tracks still use their simulated
timeline; a voice note plays through the kit's own Opus driver, above, and
the shared file viewer can play supported audio files by path.

HTML unit tests and `e2e/rss/media.txt`, `e2e/files/media.txt` and the Telegram
inline-video/seek suites cover parsing, controls, sizing and playback wishes.
Headless checks cannot verify native decoding, sound, position updates or
completion events; those require a windowed run on the target platform.
