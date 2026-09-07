# CR-012 · Telegram: chats, messages, people — as panels

**Status:** draft, round one. The surface is drawn in the panels library
over a demo world; nothing reaches Telegram yet. Written as the book's
*Telegram* chapter should read once the whole change has landed, with the
phases and the open decisions at the end.

## Why

Telegram is the other half of a day's correspondence, and the one that
lives in a window of its own. The macOS client is a two-pane app: a chat
list on the left, one conversation on the right, search folded into the
list's top, everything else behind buttons. Superapp has no panes and no
buttons that navigate; it has panels, joins, a cursor that previews, and a
bar. This change says what Telegram is in that grammar, and draws it.

The reference is the official macOS client
(`overtake/TelegramSwift`), read for what a row, a message, a title and
the composer carry — not for how they look. What is kept is the
information; the look is the book's.

## The words

- A **chat** is one conversation. It is with a **peer**: a *person*, a
  *group*, or a *channel*. A chat and its peer share one id, as they do on
  the wire.
- A **message** is one line of a chat. It is *mine* or somebody else's;
  it may be a *reply* to another, *forwarded* from someone, *edited*, and
  it may carry **media**: a photo, a video, a *video message* (the round
  one), a sticker, a voice note, an audio track, a file, a location, or a
  *live location* that moves.
- A **service line** is a line of a chat that nobody wrote: *Max joined
  the group*, *Vera pinned a message*.
- A **folder** is a saved filter over the chats — Telegram's tabs above
  the list. Here it is a filter tag.
- **Saved messages** is the chat with oneself.

## The app

The telegram app is a chat list, conversations, search, the people, and a
card for every peer. It registers ten panel kinds, its own schema ladder, a
demo seed, a search source, and — in later phases — a worker per account
and the effects a send and a read need.

### Tags and roots

| Tag | Argument | What it shows |
|---|---|---|
| `chats` | none, or `archive` | every chat, pinned ones first; or the ones put away |
| `telegram-chat` | a chat id, and optionally a message id | one conversation, with the composer at its foot |
| `line` | a chat id and a message id | one line, whole, with the verbs on one line |
| `media` | a chat id and a message id | one line's media as large as the grid allows |
| `attach` | a chat id | what goes with the next message, and the ways to make more of it |
| `place` | a chat id | where the device says I am, to send |
| `messages` | none, or a chat id | the messages a filter finds, everywhere or in one chat |
| `contacts` | none | the people |
| `peer` | a chat id | who or what a chat is with |
| `members` | a group's id | who is in it |

The roots the launcher offers, in this order: **chats**, **contacts**,
**saved messages**. The archive and the folders are not roots (*to
decide*, below).

### The chat list

A `chats` panel lists every chat through the [rich table](./richtable.md):
pinned chats first in their pinned order, then by the time of the last
message. The archive is the same panel over the chats put away —
`chats(archive)`, the way mail's archive is a mailbox — and neither list
shows the other's rows.

A row is two lines. The first is the title, bold while anything in the
chat is unread, and at the right the time of the last message — the hour
today (`11:52`), the weekday inside a week (`mon`), the day past that
(`25.08`). Before the time, a chat whose last message is mine wears its
state: `✓` sent, `✓✓` read, and `✗` in red for one that never left. The
second line is the last message in a word or two: the text, with the
speaker's first name before it in a group (`Max: …`) and `me:` for mine,
and the media in a word after a caption (`new palette · photo`); or the
media alone where there was no text (`photo`, `file report-q3.pdf · 2.1
MB`, `voice 0:42`, `sticker`); or `draft: …` while the composer holds
something unsent; or `typing…` while somebody is. At the right of the second line sits the
**count**: the unread messages as white digits in an ink box, an outlined
box for a muted chat, and `@` in the box when one of them mentions me. A
pinned chat says `pinned`, muted, before its count.

There are no avatars. A name is a name.

The filter tags are `@unread`, `@muted`, `@pinned`, `@kind:` (person,
group, channel), and `@folder:`, whose values are the folders. Free text
matches the title and the last message.

Moving the cursor previews the chat, by the shell's
[preview](./interaction-grammar.md#preview-the-one-open-that-does-not-go)
rule; `enter` moves focus to it. Rows carry marks like any table.

The bar wears one link always, **new message** (`n`), which opens the
contacts; and while rows are marked, `read n` (`r`), `mute n` (`m`), `pin
n` (`p`), `archive n` (`a`) — `unarchive n` in the archive — `mark all`
(`k`, because `a` and `m` are taken), and `clear`. A chat's own verbs —
mute, pin — are on the chat panel's bar, which the list borrows through
the chord routing while it previews one.

### The chat

A `telegram-chat` panel is one conversation, oldest at the top and the newest at the
bottom, with the composer at its foot. Its header is the chat's title. Under
the header, a muted status line says what the macOS client's title bar says
under the name: `online`, `last seen recently`, `last seen within a week`,
`7 members, 3 online`, `12.4k subscribers`, or `typing…` while it lasts.

The transcript is rows:

- A **day** is an upper-case caption over a hairline: `TODAY`, `YESTERDAY`,
  `30 AUG`.
- **Unread** is a caption over a dark rule, above the first message not read
  when the panel opened. It stays for the panel's life, as it does in the
  client: opening the chat marks it read, but the line says where the
  reading started.
- A **service line** is muted text: `Max joined the group`.
- A **message** is a header line and a body. The header is the writer's
  name in bold — `me` for mine, muted — and at the right, in muted grey:
  `edited` when it was, `sent` or `read` for mine (`failed`, red, for one
  that never left — words, because the bundled faces carry no check mark),
  the views of a channel post (`1.2k views`), and the time. Under it, in
  order and each only when there: `↪ forwarded from Elena Petrova`; `›
  Vera: the quoted line` for a reply, one line, shortened; the media, as
  the next section says; the text, as a selectable run; the reactions,
  `👍 3 · ❤️ 1`; and a channel post's `12 comments`.
- Consecutive messages by one writer within five minutes share one header:
  the second and later draw no name and no time, as the client draws no
  second bubble tail. *(To decide.)*

Nothing is aligned left or right by who wrote it. A transcript in one face
reads by names, as a log does.

### Media, shown

What a line carries is drawn where it can be and spelled where it cannot:

| Kind | Drawn as |
|---|---|
| photo | the picture, at most 320 points wide, over its caption; before the bytes land, `photo 1280×960` |
| video | its poster, then `video 0:14` |
| video message | its poster, square here, then `video message 0:08` |
| sticker | its emoji, drawn large, until stickers are drawn |
| voice | `voice 0:42` |
| audio | `audio Scratchcard Lanyard · 3:41` |
| file | `file report-q3.pdf · 2.1 MB` |
| location | `location 47.0472, 8.3164` |
| live location | `live location 55.7512, 37.6184 · 42 min left`, or `· ended` |

A list — the chat list's second line, a messages row — says the kind in
a word after the caption (`the garden today · photo`) and spells the media
out where there was no text (`voice 0:42`). The store keeps what the wire
says about each: the bytes' reference, a picture's size, a recording's
length, a location's coordinates and, for a live one, until when. This
round the references name two greyscale pictures the app bundles, drawn by
a script; the fourth phase downloads into a cache under the app's
directory and the reference becomes a path.

Playing a voice note, a track, a video or a video message, and opening a
location on a map, are the fourth phase too: `play` (`y`) and `open`
(`o`) on the chat's bar, over the line under the cursor, through the
capabilities below.

### The line's card, and the viewer

`line` is one line whole: the header the transcript draws, what it answers
or was forwarded from, its media at the card's width — the picture, the
poster and its player, the map — and its text as a selectable run. It is
reached by `line` on the chat's bar over the line under the cursor, and it
is joined to the chat, so `reply` on the card lands on the chat's composer:
the card finds the chat it hangs under and tells it.

`media` is the viewer: one line's media as large as the grid allows. A
panel that asks for the whole grid is what *full screen* is in this
grammar — it takes a column as wide as the screen and the camera goes to
it — and it is still a panel: joined to the card or the chat that opened
it, closed with `cmd+w`, undone with `cmd+z`. Its bar walks the chat's media
in place, `previous` (`p`) and `next` (`n`), runs a recording with `play`
(`y`), and hands the bytes to the system with `open` (`o`). A press on a
picture in the transcript or on the card opens it.

### Media, sent

Every kind is sent through the **attach panel**, `attach` on the chat's
bar, joined to the chat (Andrey, 2026-09-05: the chat's bar was
overloaded; browse, voice, video and place go behind one link). Each is an
effect with the same desired/actual split as a text (phase 3 and 4). This
round each is drawn and each ends in a toast.

**One thing at a time**, as the clients have it. In TelegramSwift the
paperclip offers *Photo or Video*, *File*, *Location*, *Poll*, each its own
send; the microphone is the composer's own and records one message; a
picture or a file goes together with the text as its caption, several as
an album. So here: the files on the list go with the next text message and
nothing else does; a voice note, a video message and a place each go on
their own, at once. While a recording runs the list gives way to the strip
— the recording is what is being sent — and comes back when it has gone,
the files still waiting for the text. Sending a place from its panel
leaves the list alone the same way (Andrey, 2026-09-05: files and photos
never go with a location, a voice note or a video circle).

The panel lists what the composer will send with the text, under the same
`CARRIES` caption the composer's own line wears, in the order it will go:
one row a file, the name and under it what it goes as and where it is —
`report-q3.pdf`, `file · ~/Downloads`. The list is the chat panel's, as
the reply line is: the composer sends it, and the attach panel edits it
through the join, as the line's card replies through it. Opened away from
its chat it says so.

- **A photo, a video, a file, an audio track** — from the files app's
  clipboard, exactly as a letter carries a file: `browse` (`b`) is a link
  to the files panel, joined, where `copy` holds what is marked; back on
  the attach panel `add` (`d`) — `add 2` — puts them on the list. The
  link is always there and the button comes and goes beside it with the
  clipboard. The arrows walk the rows; `remove` (`r`)
  puts the cursor's row down; `earlier` (`e`) and `later` (`a`) trade it
  with its neighbours, spelled by the order they will go rather than by
  the screen. What is sent as what is read off the name: a picture as a
  photo, a moving one as a video, a sound as audio, anything else as a
  file. The composer shows the same list on its `CARRIES` line, one link a
  file, and `enter` there sends the text with them; the chat's bar counts
  them, `attach 2`. A picture on the system clipboard pastes into the
  composer with `cmd+v` and goes with the text (phase 4).
- **A voice note** — `voice` (`o`): a recording strip stands at the
  panel's foot — what and how long, `recording voice 0:03`, the keys on
  a muted line under it, and the level under that — and the bar is its
  two ways out, `send` (`s`) and `discard` (`d`); `enter` and `esc` are
  the same two. Through a
  `Microphone` capability, which is the platform's: AVFoundation on macOS,
  `AudioRecord` on android, and a fake that hands back a bundled clip.
- **A video message** — `video` (`v`): the same strip, with the camera's
  picture over it, cropped square as the client does. The makepad this
  build pins carries a `Video` widget with AVFoundation playback and a
  camera preview mode on Apple and its own on android, so the preview and
  the recording ride that rather than a capability of our own.
- **A place** — `place` (`p`): a panel joined to the attach panel with
  where the device says I am, on the map, and the coordinates as a
  selectable run; `send` (`s`) sends it once, `live 1 h` (`v`) shares it
  for an hour, and the worker keeps it moving until then. Through a
  `Location` capability: CoreLocation, android's `LocationManager`, and a
  fake at the trailhead.

Stickers are not sent (Andrey, 2026-09-04): a sticker is a picker.

### The media kit

None of the above is Telegram's. The shell's `widgets/media` is the kit
any panel embeds — a voice note in a chat, an audio part of a letter, an
`.mp3` on a file card are one player:

- **a picture** at a width, the box shown only while there is one;
- **the player**: `play` or `pause`, the progress as a filled hairline, and
  the time, `0:17 / 0:42`. A player's *state* — where it stands, whether it
  runs — is the panel instance's, ticked against the session's clock, so a
  scripted run and a library node advance it with `wait`; the kit only
  draws it. Playing the bytes is a `Playback` capability the kernel owns
  and a platform supplies (AVFoundation, android's `MediaPlayer`), phase 4;
  video frames ride makepad's own `Video` widget;
- **the meter** of a recording under way, as bars;
- **a map**: a place on a snapshot of the map around it, the pin at its
  centre, drawn from Web Mercator tiles. The maths and a fake tile source —
  a drawn street grid, deterministic per tile — are the shell's; the real
  one is the kernel's `Tiles` capability over OpenStreetMap's raster tiles,
  fetched by a worker with the app's user agent and cached under the app's
  directory, phase 4. A place opens in Apple Maps or in the browser at
  OpenStreetMap through the system's opener.

The kit has a scene of its own in the library, `media kit`, drawn from
fixtures and no app.

The transcript has a **cursor**: arrows walk the messages, `space` marks,
`shift+arrow` extends, `esc` clears. The cursor's row takes the table's
wash and marked rows its bar, because the rows are what the bar's verbs act
on: a reply is to the message under the cursor.

The **composer** is a field at the foot, `write a message…  ( enter )`,
multiline: `enter` sends and `shift+enter` breaks a line. While the caret
is in it the field keeps the text chords — `cmd+x`, `c`, `v`, `a` — and
nothing else, so `cmd+h` from the caret is still `attach` and the bar
draws every other letter bold. The key in its
placeholder is the way to it, as the filter's `( / )` is: `enter` on the
transcript's lines puts the caret there, and so does a chat *taking*
focus — `enter` on its row in the list, `cmd+arrows` into it, a press on
it — because a chat is entered to be written in, as on the client. `esc`
hands the keyboard back to the lines, whose cursor the arrows walk, and a
press on a line does the same. Above the field, while replying, a line
says `reply to Vera: …` and `esc` takes it away first. In a channel the
person cannot post in, the field gives way to a muted line, `you can't
post here`; in one they own, its placeholder is `broadcast…  ( enter )`.
What is typed stays with the chat as its draft, which the list shows.

The bar carries what a chat is for and links to the rest. Over the line
under the cursor, and only then: `reply` (`r`), and on a line of mine
`edit` (`e`) and `delete` (`d`). Three links: `attach` (`h`), what goes
with the next message, saying how many files wait (`attach 2`); `line`
(`n`), the cursor's line as a card; `about` (`a`), the peer's card. While
rows are marked the batch has the bar instead of the cursor's verbs:
`forward n` (`f`), `delete n` (`d`) while every marked line is mine, and
`clear`. A reply or an edit puts the caret in the composer, from the bar
or from the card, because both are written.

**Edit and delete are real** on the store this round, the two verbs that
are: `edit` gives the composer the line's text under an `editing: …` line
— the draft waits and comes back — and `enter` writes it, `edited` on the
header; `delete` takes the line away, the cursor stepping to the line
before it. Each is one undoable action: `cmd+z` puts the old text or the
whole line back, exactly as it was, so a deleted line keeps its id and its
place. Nothing reaches Telegram yet; phase 3 puts an effect behind each,
reading the same intents.

Everything else is one link away rather than on the bar. What acts on
**one line** — `edit` (`e`, while it is mine), `forward` (`f`), `delete`
(`d`), `pin` (`p`), `play` (`y`) over a recording, `open` (`o`) to the
viewer, and a place's `maps` (`m`) and `browser` (`b`) — is on the line's
card. What acts on **the chat** — `mute` (`m`), `pin` (`p`), `archive`
(`a`), and `search` (`s`), the messages of this chat — is on the peer's
card, with `chat` and `members` beside them. What **goes with the next
message** — the files, the recordings, the place — is on the attach panel,
the next section. A chord reaches them the way it reaches anything: the
panel is joined to the chat, and the chat's own `cmd+e` while it previews
the card is the card's `edit`.

A chat asks for five columns and the whole height: a conversation is the
one panel that wants to be wide.

### Search

`messages` is a rich table over messages. Bare, it is every chat; with a
chat id it is that chat's, and its title says so (`messages · stelaxis`).
A row is the chat and the writer with the time, then the line itself,
shortened. The filter's free text matches the text; the tags are `@from:`,
`@date`, `@media` (any), and `@chat:` in the bare panel. The cursor
previews the chat opened *at* that message, and `enter` goes there.

The app is also a source for the shell's [search panel](./interaction-grammar.md#search):
it answers with the chats and the people whose names carry every word,
then the messages whose text does, best first — `@app:telegram` keeps its
rows alone.

### People

`contacts` is a rich table of the people in the address book: the name,
and under it the status and the username, muted. `@online` is its one tag;
free text matches the name and the username. The cursor previews the chat
with that person, which is how a new conversation starts: a person who has
never written has an empty chat, and it says *no messages here yet*.

`members` is the same table over one group, with `@admin` beside
`@online`, and its cursor previews the member's card.

### The card

`peer` is a card: the name, bold; one line saying what it is — `@vera ·
online` for a person, `group · 7 members, 3 online`, `channel · 12.4k
subscribers · @rustweekly`; the phone number as a selectable run where
the person is a contact; and the bio or description under a rule.

Its bar: `mute`/`unmute` (`m`), `pin` (`p`), `archive` (`a`), and the
links `chat` (`c`), `search` (`s`), and, for a group, `members` (`e`).

### The bars, together

Every button here acts on what the panel shows and every link goes
somewhere from it, as the [grammar](./interaction-grammar.md#the-bar)
asks. No bar wears `w`, `z`, `u`, `t`, `i`, or `l`, and no bar wears a
letter twice; the app's tests say so for every panel over the demo world.

## The store

Every table is prefixed `tg_`. The ladder is one step in this round.

| Table | What a row is |
|---|---|
| `tg_peer` | a person, a group, or a channel: its kind, name, username, bio, phone, presence, member counts, whether I am its admin, a contact, or it |
| `tg_chat` | one per peer I have a conversation with: pinned order, muted, archived, the unread count, whether one of them mentions me, the draft, who is typing, the last message read |
| `tg_message` | one line: its chat, its sender, the time, the text, mine or not and the state of mine (`sending`, `sent`, `read`, `failed`), edited, what it replies to, whom it was forwarded from, its media and the media's label, views, reactions, and whether it is a service line |
| `tg_member` | a group's membership, with whether the member is an admin |
| `tg_folder`, `tg_folder_chat` | a folder and the chats it holds |

The chat list is a `SqlSource` over `tg_chat` joined to its peer and its
last message; the messages list is one over `tg_message`; the people are
one over `tg_peer`. Everything else a panel reads is a registered query,
so a transcript follows a commit under it.

Opening a chat **claims** the read: the count goes to nought and the last
read message moves, on the same undoable node as the panel appearing, so
one undo closes it and gives the count back. A cursor walk down the list
coalesces into one node as a mail walk does.

### The seed

A fresh store gets a demo world of ten chats around the people mail's
demo already knows — Vera, Elena, Max — and a few more: two pinned chats,
a muted group with an unread mention, a channel with views and comments,
one I own, saved messages, a group with a draft and a failed send, an
archived one; folders *work* and *personal*; and enough messages in the
group to carry a forward, a reply, a file, a photo, a voice note, a
sticker, reactions, and service lines. It is the fixture the library and
the suites draw, and it is what a person sees until an account signs in.

## Phases

1. **The surface, in the library** — this round. The app with its ten
   kinds, the schema and the seed, the widgets, the search source, eleven
   scenes on the canvas, the app's tests, and `e2e/telegram/basic`. Every
   verb that would reach Telegram toasts *draft: nothing leaves — …*; what
   is the panel's own — the cursor, the marks, the reply line, the draft
   text, the read claim — is real. Done when the canvas shows every state
   named above and the battery is green.
2. **An account.** MTProto behind a `Telegram` capability — the wire, the
   session keys in the keychain, sign-in by phone and code as the app's
   own `add_account` kind, a worker per account that pulls dialogs and
   history into the tables and holds the update stream. The demo world is
   what a store without an account shows, as mail's is.
3. **The verbs.** Send, reply, edit, delete, forward, pin, mute, archive,
   mark read — each an effect with a desired/actual split like mail's
   flags, so undo is a plain intent flip and the pass re-converges.
4. **Media.** The downloads into a cache and the references that become
   paths; `Playback` behind the player and makepad's `Video` behind the
   viewer; the kernel's `Tiles` over OpenStreetMap behind the map; and the
   sending of every kind — the attach panel's list, the paste, `voice`,
   `video`, `place` and `live` — behind the `Microphone`, `Location` and
   makepad's camera, each with a fake a suite can run. A photo on the
   attach panel's list shows its picture once the bytes are read through
   the file capability.

## To decide in review

- **The second bubble's time.** A continuation message draws no time. The
  client tucks it into the last line of the bubble, which the DSL cannot
  do without a custom draw. The alternative is a time on every message,
  which doubles the height of a busy chat.
- **Reactions in colour.** `👍 3` is drawn with the emoji font, and an
  emoji is the one colour in the transcript that is not an error. A text
  spelling (`+1 3`) keeps the rule and loses the meaning.
- **Folders as roots.** `App::roots` has no store, so a folder cannot be
  offered by the launcher today. A `chats` argument seeded with `@folder:`
  is one line once the roots API takes a store. The archive could be a
  root now, as mail's is; it is reached from a card's `archive` for the
  moment, and from the chat list's own verb once that is real.
- **The count's shape.** An ink box with white digits is the one filled
  box in the language. A bold number with no box is the quieter option.
- **The transcript's cursor.** Arrows walk messages once `esc` has put
  the keyboard on the lines, so a plain `↓` there is a step and not a
  scroll; scrolling is the wheel's. A chat with hundreds of lines and a
  cursor at its foot is fine; one with a cursor and no marks is a wash
  that means little. The alternative is no cursor: reply and edit by a
  press on the row, and the composer keeps the keyboard always.
- **`about` versus `info`.** The client says *Info*; the bar says `about`,
  because the card is about the peer and `i` is the workspace's.
- **How rare is rare.** `mute`, `pin`, `archive`, `forward`, `delete` and
  `edit` left the chat's bar for the two cards (Andrey, 2026-09-04: hide
  the rare ones); `search` followed to the peer's card and the sending
  verbs to the attach panel (Andrey, 2026-09-05). The chat's bar is
  `reply` and three links.
- **Where the list lives.** What goes with the next message is the chat
  panel's, as the reply line is, so it goes when the panel closes; the
  text of a draft is the chat's row and survives. The client keeps neither
  a picked file nor a recording past the dialog. A `tg_draft_file` table
  would keep the list with the draft, at the price of a schema step.
- **Four words for two moves.** `earlier` and `later` say what the order
  means — first on the list, first sent — where `up` and `down` would say
  the screen. `later` wears `a` and `add` wears `d` because `l` and `t`
  are the workspace's and `e` is `earlier`'s.
- **Delete for me.** The clients delete another's line *for me* in a
  private chat, and an admin deletes anyone's in a group. Here `delete`
  is on my own lines alone (Andrey, 2026-09-05: my own messages); the
  other two are a rule on the peer and a flag on the member, later.
- **The microphone's place.** The clients keep it on the composer; here a
  voice note is two steps away, `attach` then `voice`, because the chat's
  bar was the thing to slim. `voice` and `video` back on the chat's bar
  would be two more buttons there, one step each.
- **The viewer's width.** It asks for twelve columns — the whole grid —
  so a picture is as large as the screen. On the phone's grid that is the
  screen anyway. Six columns, half the screen, would keep the chat beside
  it.

## Not done, on purpose

- **Bots, settings, stickers and emoji pickers, calls, stories, secret
  chats, forum topics, polls, payments.** Not everyday, and each is its own
  surface.
- **Creating groups and channels, editing contacts.** A phase after the
  verbs, if wanted.
- **Sending stickers.** A sticker is a picker; what is received is shown.
- **Reacting.** Reactions are shown, not given: giving one is a picker.
- **Drawing stickers.** Telegram's are WEBP and TGS, and this build
  decodes PNG and JPEG; a sticker is its emoji until a decoder is worth
  carrying.
- **A map.** A location is its coordinates and, later, the system's maps
  through `open`; tiles are not drawn.
- **Avatars.** There are none anywhere in the app, by the language.
- **Notifications.** A mention is a count in a list and a toast is for what
  an action did; a system notification is a shell question, not this
  app's.
