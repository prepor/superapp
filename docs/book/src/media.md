# Media

A clip or a sound is a file on this device or an address on the web, drawn
by the platform's own player through one surface and transported by one
strip. One plays at a time. Three hosts embed it: a `<video>` or `<audio>`
in a [reading](./rss.md), an `.mp4` or `.mp3` on a [file card](./files.md),
and a clip in a [Telegram](./telegram.md) chat.

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
  so. A host with no player at all — a demo line, a voice note this build
  cannot decode — runs a `Timeline` instead, ticked against the session's
  clock, which the same strip draws. The registry is per store, which is
  what keeps two sessions in one test process from pausing each other.
- **`Clip`**: the driver. It leases the native player for the life of its
  host (`VideoPlayback`; Makepad's `Video` does not stop its player on
  drop, so a lease retires it to `cleanup_videos` at the app root), hands
  the source over exactly once, keeps the poster up until the player has
  delivered a first frame, and reads the platform's word back.
- **`Scrub`**: a press on the hairline, the drag that follows, the release,
  with the bar the press landed on kept through the whole drag.
- **`prime_video`**: draws a hidden box's player at no size every frame,
  for Android, which gives a player its texture only on a draw and will
  not prepare a clip before then.

A host embeds the surface and the strip in its own template, keeps a
`Transport` and a `Clip` per thing it can play, keeps the player in a
hidden holder of its own, registers the button and the hairline in its hit
table, and says where the source is. The reading's item (`reader/clips.rs`)
is its own host; the file viewer hosts one for a card; Telegram's panels
host theirs over a download.

The kit also supplies `MediaPicture`, the recording-level bars in `MediaMeter`,
and `MediaMap`. `shell/widgets/map.rs` composes Web Mercator tiles and a pin
through `TileSource`; the current source is the deterministic `FakeTiles`
street grid. It is not a live map service. Recording controls are fixtures
until a host supplies actual capture; Telegram reports capture and location
sharing unavailable on live accounts.

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
fallback.)

RSS/Atom enclosures and JSON Feed attachments are not appended to readings;
only media embedded in the selected HTML content appears. Whether to add
enclosures and how to retain paused frames are [open questions](./open-questions.md).
Volume, mute and speed controls, fullscreen, picture-in-picture and subtitles
are not exposed by this kit. Telegram voice notes and audio tracks still use
their simulated timeline; the shared file viewer can play supported audio
files by path.

HTML unit tests and `e2e/rss/media.txt`, `e2e/files/media.txt` and the Telegram
inline-video/seek suites cover parsing, controls, sizing and playback wishes.
Headless checks cannot verify native decoding, sound, position updates or
completion events; those require a windowed run on the target platform.
