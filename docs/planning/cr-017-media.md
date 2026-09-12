# CR-017 · Clips and sounds: video and audio in the reader, on the card, and in the chat

Status: **in progress** (Andrey, 2026-09-12: "keep video and audio, add
support them (code should be shared with telegram and our file browser)" —
after grumpy.website's posts opened in rss with their clip gone; then "go,
implement"). Phases 1, 3 and 4 have landed on `prepor/rss-grumpy-video-articles`
and the book says what they do (`media.md`, `rss.md`, `mail.md`,
`files.md`, `viewers.md`, `telegram.md`). Left: phase 2 (the fork's position
beat and end-of-item on macOS), phase 5 (enclosures), and a letter's video
part played from the blob cache's file rather than read as bytes. One
decision went the other way from what is proposed below, and the book has
it: the registry stays per store rather than on `Cx`, because a verb takes
the slot without a `Cx` and two sessions in one test process must not
pause each other.

## Why

A grumpy.website post is a clip and a caption. Its feed says so:

```html
<p><video autoplay loop muted controls>
  <source type="video/mp4" src="https://grumpy.website/media/2026/1804.mp4"/>
</video></p>
<p><strong>nikitonsky:</strong> Okay here's the thing that worries me …</p>
```

The reader drops `video`, `audio`, `source` and `track` whole at parse
(`reader/html.rs`, `DROP`: "media the widget cannot draw"), the paragraph
around the clip is then empty and vanishes, and the stored reading is the
caption alone. Nothing says a clip was there; **open original** is the only
way to it. The same narrowing reads every letter, so a `<video>` in a
newsletter goes the same way.

The app can draw a moving picture. The shell's media kit
(`shell/widgets/media.rs`) plays a clip through the platform's own player —
AVPlayer, ExoPlayer — and Telegram plays videos inline with it: a poster
until the first frame, a *play* button and a hairline beneath, seeking by
drag, one thing playing at a time. But the pieces that make that a player
rather than a widget — the wish and the native state, the lease over the
native player, the poster handoff, the scrub — are Telegram's
(`telegram/panels/playback.rs`, `telegram/widgets/inline_video.rs`), written
against its messages and its download queue. A reading has no message, and
a card on `~/Downloads/talk.mp4` today reads *no preview — open shows it*.

This change moves the player into the shell and gives it three hosts: the
reader, the file card, and the chat it came from.

## The model

> **A clip or a sound is a file on this device or an address on the web,
> drawn by the platform's player through one surface, transported by one
> strip. One plays at a time.**

Five rules.

### The kit has a player, and a host supplies the source

The media kit grows from templates and fill functions into a player, in
five parts, all in `shell/widgets/media/`:

- **The surface** (`MediaClip`): the poster, the frames and a note over
  them, in one box that never changes size once it has one — Telegram's
  `TelegramInlineVideo`, moved. A picture, a clip's poster and a clip's
  frames occupy the same rectangle, so playing never moves what is under
  it.
- **The strip** (`MediaPlayer`): *play* or *pause*, the hairline, the time.
  Already in the kit; unchanged.
- **The transport** (`Transport`): the wish (run or hold), the position the
  platform last reported, the seek waiting to be applied. Telegram's
  `Playback` without TDLib. A verb or a click sets the wish; the draw is
  where it is made so, because a player is reachable only with a `Cx`.
- **The driver** (`Clip`): the lease over the native player, the source
  handed over exactly once, the first-frame gate that keeps the poster up
  until the player really has a picture, the word for a trace
  (`unprepared` … `completed`). Telegram's `InlineVideo`, moved.
- **The scrub** (`Scrub`): press on the hairline, drag, release — the
  small state machine that `line.rs`, `chat.rs` and `media.rs` each carry
  today, once.

A host embeds the surface and the strip in its own template, keeps a
`Transport` and a `Clip` per thing it can play, registers the button and the
hairline in its own hit table, and answers one question: **where is the
source?**

```rust
pub enum Source {
    /// A file on this device: a card's path, a blob-cache entry, a
    /// Telegram download.
    File(PathBuf),
    /// An address the platform's player streams itself. Nothing of the
    /// clip passes through the app.
    Web(String),
}
```

`File` becomes `VideoDataSource::Filesystem`; `Web` becomes
`VideoDataSource::Network`, which the Apple and Android backends both take.
A reading supplies `Web`; a card supplies `File`; Telegram supplies `File`
once its download has landed, and until then nothing — with its note
(*downloading · 12 MB / 48 MB*) over the surface, as today. Fetching stays
the host's: the shell never downloads a clip.

### A sound is a clip without a picture

The same widget, the same driver, the same strip; the surface is not shown.
The platform player prepares an audio-only item as such — the fork's Apple
backend reports `0×0` with an audio track — so an `.mp3` on a card and an
`<audio>` in a reading are the video path with the box hidden. Codec
coverage is the platform's, which is also the OS's own: MP4/M4V/MOV with
H.264/H.265, AAC, MP3, WAV, FLAC on Apple; those and WebM/Ogg on Android.

One thing the fork does not do yet: a position arrives only with a video
frame, on both platforms (`macos.rs` polls `poll_frame` and posts
`VideoTextureUpdated`; Android posts the same on `updateTexImage`), and on
macOS nothing posts `VideoPlaybackCompleted` at all. A sound has no frames,
so its hairline would stand still; a finished clip's button would keep
reading *pause* on macOS. Both are one patch to `prepor/makepad`: a
position beat on every poll of a playing item that produced no frame, on
both backends, and an end-of-item event on macOS when `currentTime`
reaches `duration` — the one Android already sends. The phases below put
the patch before the sounds.

### One plays at a time

Pressing *play* anywhere pauses what played before — in another panel,
another app, another workspace. The registry is `Cx`'s: whoever drives a
clip in a draw takes the slot, and the previous holder's wish falls, so it
pauses on its next draw. Telegram keeps this rule today per store
(`ActivePlayback` in `store.local()`); the screen is where one thing plays,
so the store-scoped one goes.

A clip also pauses when it leaves: its panel closes (the lease retires the
native player to `cleanup_videos`, as now), its workspace is switched away,
or its box scrolls wholly out of its panel's viewport.

### The reader keeps `<video>` and `<audio>`

`video`, `audio`, `source` and `track` leave `DROP`. The narrowing writes a
clip the way it writes a picture — one self-closing tag with what the item
needs:

```html
<video src="https://grumpy.website/media/2026/1804.mp4" type="video/mp4"
       autoplay loop muted/>
<audio src="https://…/episode.mp3" type="audio/mpeg"/>
```

- **The source** is the tag's own `src`, else the first `<source>` with a
  web address in a container every platform plays — by its `type`, or by
  the address's extension where there is none — so `<source webm><source
  mp4>` yields the mp4, and only a lone WebM is a WebM; resolved against
  the reading's base like `src` and `href`, and so is `poster`. `type`
  travels along. `cid:` and `data:` are not sources for a clip.
- **The size** is the `width`/`height` hint, as for a picture (a `style`
  `max-width` is not one); the box then follows the same rules as
  `ReaderImage` — the hint, never wider than the column, capped at 360×320,
  proportions kept. Without a hint, the poster's own pixels decide; without
  either, 16:9 until the player reports the clip's, and the box takes them
  once — the reflow a picture has when its header arrives, not Telegram's
  fixed rectangle, which a transcript needs and a reading does not.
- **The fallback** — the element's children that are not `source` or
  `track` — is what a browser shows when it cannot play, and stays as
  prose after the clip.
- **Nothing to play** — no web source at all — leaves the poster as a
  picture and the fallback as prose.
- **`plain`** counts a clip as it counts a picture (`IMG_LINES`).

`html::VERSION` becomes 6, and cached readings rebuild on the next open —
RSS and mail from their stored raw, legacy RSS from its saved markup — which
is the mechanism this version exists for.

The item is `ReaderClip`, minted by the `Html` widget from `video :=` and
`audio :=` templates the way `img :=` mints `ReaderImage`. It owns its
transport, its driver and its native player: a reading's items are stable
for as long as the reading is (the widget keys them by node), which is what
Telegram's virtual rows are not — that is why a Telegram panel owns one
player and its rows borrow it, and why the reader need not. The players
prepared at a time are the ones playing: the one clip with a sound, and
the silent loops on the screen.

Before playing, the item asks the platform whether it can — `can_play_type`,
the browser's own question — and where the answer is *no* it draws a link
in the clip's place, `video ↗` or `audio ↗` to the source, which opens in
the browser: an Ogg Opus episode on macOS is still reachable, and honest.
A source without a `type` is not refused, and neither is anything on a
platform with no player at all (headless): the surface and the strip
stand, and nothing runs.

**Autoplay.** A `<video>` that says `autoplay muted` runs while its box is
on the screen, looping if it says `loop`, and stays muted — the browser's
rule, and what a silent screen recording is published as. It is a moving
picture, not a sound, so it stands outside the one-at-a-time rule and never
pauses anything; scrolled out of view it holds, and plays again when it is
back. Anything else waits for *play*. `muted` and `loop` are honoured on
their own too: a player is told both when it is made, so the item keeps
one per combination and drives the one the tag asked for.

**Before play, and after pause.** Nothing is fetched until *play*: a clip
with a poster shows it, one without shows the dark box. A paused clip lets
its player go and remembers where it stood, and the next press prepares
it again there. Two reasons, one of them the platform's: a prepared player
is a decoder and its buffers, and a reading with a clip in every paragraph
must not hold one per clip; and the fork's Apple backend gives up on a
native player that yields no frame for sixty polls — a paused one does not
— and hands it to a software decoder this build does not carry, which is
an error. Until the fork stops counting a paused player's polls (phase 2),
a paused frame cannot be kept anywhere on macOS; the reader chooses to let
go on purpose rather than at random.

### The card plays a file

`FileKind` gains `Video` and `Audio`: by extension (`mp4`, `m4v`, `mov`,
`webm`, `mkv`; `mp3`, `m4a`, `aac`, `wav`, `flac`, `ogg`, `oga`, `opus`) and
by MIME (`video/*`, `audio/*`) for attachments without one. The words are
*video* and *audio*; the filter tags are `@kind:video` and `@kind:audio`.

The shared viewer gains a media branch beside text, image and PDF: a
`Preview::Path` or `Preview::Disk` of those kinds shows the surface and the
strip, and the card's panel — files, a letter's attachment, Telegram's
document — wears **play** / **pause** (`y`, as Telegram's viewer does; the
card's own letters `o p m r d c` stay). A player wants a file, not bytes: a
letter's part, which the blob cache already keeps on disk, reaches the
viewer as its cache path rather than as `Preview::Bytes`; and a file whose
name says nothing — a Telegram cache file — reaches it under the playable
link, the blob under a name that says its container, which is what the
platform's player goes by. A video card asks
for the clip's proportions the way an image card does; an audio card is
compact. The viewer's fit and zoom verbs do not apply to a clip. **open**
still hands the file to the system, which is the sure way to see what the
in-app player cannot.

## The surface

**A reading.** The clip is a box in the column with the strip beneath it —
`play`, the hairline, `0:00 / 0:12` — the way a picture is a box. Its
poster (or first frame) is in it until the player has a picture. *play* on
the strip runs it; a drag on the hairline seeks; the box itself is inert, so
a selection can start on it as on a picture. The pointer wears a hand over
the button and the hairline, registered through the reading's control
rectangles as a linked picture is today (`pictures::link_rects` becomes
labelled controls). A sound is the strip alone. The article bar is
unchanged.

**A card.** Under the rule, where the preview goes: the box at the clip's
proportions, the strip, and the bar's `play`. Mail attachments and browsed
files alike.

**A chat.** Nothing changes for the person: the transcript row, the line
card and the media viewer draw and transport as they do today, on the
shell's pieces instead of their own. Telegram keeps what is Telegram's —
asking for a clip through TDLib, the download note, the rule that a demo
line runs the fake timeline — in `telegram/panels/playback.rs` as a thin
host over `Transport`.

**The library.** The rss `reading` scene shows a clip (the demo feed gains
an entry with one); a `media` scene under the shell draws the surface and
the strip in their phases; the files scenes gain a card on the demo tree's
`clip.mp4` and `track.mp3`.

## Phases

1. **The kit's player.** `Source`, `Transport` and the `Cx` registry,
   `Clip`, `MediaClip`, `Scrub`, in `shell/widgets/media/`. Telegram moved
   onto them with no change in behaviour; its tests
   (`inline_media_keeps_its_rectangle_through_playback`,
   `native_players_pause_when_another_panel_takes_playback`, the seek
   tests) move with the code they test. `pictures::link_rects` becomes
   labelled controls. The `media` scene.
2. **The fork.** macOS, in `prepor/makepad` (`~/code/makepad-superapp`,
   branch `superapp-pin`), three things, then the pin moves:
   - `apple_video_player.rs`, `poll_frame`: a native player that yields no
     frame for sixty polls is switched to the software decoder — which
     this build does not carry, so the switch is an error. An audio-only
     item never yields a frame, and a paused one does not either, so
     neither may be switched: `check_prepared` already knows the tracks,
     and `should_play` says whether a frame was even due. Until then the
     reader lets a paused player go rather than keep it (see *Before play,
     and after pause*), and a paused Telegram clip is on borrowed time.
   - `macos.rs`, the `Paint` poll, and `android.rs` after
     `get_video_updates`: a position beat for a playing item that produced
     no frame this poll (an audio-only one), so the widget's
     `current_position_ms` moves and the strip with it. `VideoTextureUpdated`
     with the current position is what the widget already reads;
     a `VideoPositionUpdated` of its own would be cleaner.
   - `apple_video_playback.rs`, beside `check_looping`: an end-of-item
     check — `currentTime` at `duration`, not looping, still asked to
     play — that `macos.rs` posts as `VideoPlaybackCompleted` once, the
     event Android already sends; `VideoPlayback::drive` then releases the
     clip and the button reads *play* again.
   Verified in a windowed run with a clip that ends and an `.mp3`; the
   headless harness has no player, so this phase is Andrey's to run.
3. **The reader.** The narrowing (`video`/`audio` kept, the emit, `poster`
   resolved, fallback as prose, `VERSION` 6, the tests in `html/tests.rs`
   with grumpy's entry among them); `ReaderClip`; `can_play_type` and the
   link; autoplay and prepare-on-sight; pausing on leave; the hits; the
   demo feed's clip entry; an e2e script that opens it, sees the strip,
   clicks `play` and sees `pause` (headless has no player; the wish is what
   the button reads). Book: `rss.md`, `mail.md`'s reader section, and a new
   shell chapter `media.md` — pictures, clips, sounds, places — which is
   where the kit finally gets written down.
4. **The card.** `FileKind::Video`/`Audio` with words and tags; the viewer's
   media branch and its measure; `play` on the card bar for files, mail
   attachments and Telegram's document viewer; the demo tree's two files;
   the files scenes. Book: `files.md`, `viewers.md`, `mail.md`'s
   attachments.
5. **Enclosures** (optional). A feed that carries its media beside the
   text — RSS `<enclosure>`, Atom `rel="enclosure"`, JSON Feed
   `attachments` — gets it as a clip or a sound at the end of the reading
   when the reading does not already point at that address. This is what a
   podcast is; grumpy's enclosure duplicates its `<video>` and adds nothing.
   `parse.rs` only.

## To decide in review

- **Autoplay.** Honour `autoplay muted` as an animation outside the
  one-at-a-time rule (proposed), or never autoplay and let every clip wait
  for *play*. The first is what grumpy's clips were published for; the
  second is one rule fewer.
- **Before play.** Prepare on sight for a clip without a poster (proposed:
  a partial fetch, a real first frame), or fetch nothing until *play* and
  show a dark box. Telegram's rule is the second, because TDLib downloads
  whole files; a streaming player does not.
- **The box.** Inert (proposed), or a tap on it plays and pauses too. The
  kit's grammar puts transport on the strip so a clip and a sound are
  transported the same way; every other player in the world answers a tap.
- **The reader's bar.** No verb (proposed); or `play` (`y`) on the article
  bar driving the reading's first clip, so the keyboard reaches it.
- **Players per reading.** One per item (proposed) or one per panel with
  items borrowing it, Telegram's rule. Per-item is simpler; per-panel bounds
  native resources by construction rather than by the one-at-a-time rule.
- **Phase 2's place.** Before the reader (proposed) so a finished clip is
  right from the first build; or after, accepting a *pause* that never
  returns to *play* on macOS for a while.
- **Enclosures.** In or out of this change.

## Considered and not chosen

- **A link in the clip's place.** `video ↗` to the source, opening the
  browser: an hour's change to the narrowing and no player. It is the
  fallback this change keeps for what the platform cannot play, not the
  behaviour.
- **Downloading web clips into the blob cache first**, Telegram's road.
  The platform player streams and seeks over HTTP itself; a cache would
  double the bytes, delay the first frame, and keep clips nobody asked to
  keep. Telegram downloads because TDLib is the only road to its files.
- **A second engine for sound** — a decoder crate over makepad's audio
  output. One lifecycle for clips and sounds is the point; the platform
  already plays both. The price is codec coverage on Apple: no Ogg, no
  Opus, no WebM.
- **`<iframe>` embeds.** A YouTube frame is a page, not a clip; nothing
  here can play it. It stays dropped. A link to the frame's page is a
  separate, smaller change.

## Not done on purpose

- **Telegram voice notes.** Ogg Opus; AVPlayer will not, so on macOS they
  keep the fake timeline they have today. Android's ExoPlayer can, and a
  later change may route them through the same player there.
- **Telegram's viewer onto the shared viewer's media branch.** It embeds
  the same surface and strip already; what it adds — download feedback,
  the walk between messages — is its own, and stays.
- **Volume, mute, speed, fullscreen, picture-in-picture, subtitles
  (`<track>`), remembering a position, HLS/DASH.** A muted clip stays
  muted; everything else plays with the platform's sound.
- **A history node for playing.** Play and seek are not actions; `cmd+z`
  does not un-play.
- **Sync.** Nothing here replicates.
