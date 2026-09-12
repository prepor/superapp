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
rule — runs on sight, loops if it says so, stays muted, and stands outside
the one-at-a-time rule: it is a picture that moves, not a sound.

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

Prepared players are bounded: three across every reading open. A prepared
player is a decoder and its buffers, so past the bound the least recently
used paused clip lets its player go and shows its poster again, and the
next press prepares it afresh. A playing clip is never let go.

Playing and seeking are not actions in the history; `cmd+z` does not
un-play.

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

What the fork does not do yet: report a position without a video frame —
on either platform, since both post the position with a decoded frame —
or, on macOS, the end of a clip. A sound's hairline therefore stands still
while it plays, and on macOS a finished clip's button keeps reading
*pause* until it is pressed. Android reports the end.
