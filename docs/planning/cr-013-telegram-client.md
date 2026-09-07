# CR-013 · Telegram, the client: the engine, the local view, the cache

The [round-one surface](./cr-012-telegram.md) draws the demo world. This is
the plan for the real one: an account signs in, its dialogs and history come
down, and the panels that were fixtures are drawn over live tables. The
grammar does not change; what changes is where the rows come from.

## The engine

The wire is [TDLib](https://core.telegram.org/tdlib), Telegram's own client
library, reached through its C interface: `td_json_client_send`,
`_receive`, `_execute`. It owns MTProto, the type language, the auth flow —
phone, code, two-factor — updates, and layer upgrades, so none of that is
ours to keep correct. We bind five C functions, not a protocol.

TDLib is a worker's engine, one per account, on its own thread: a loop that
pumps `td_receive`, turns each update into a change on the store, and takes
requests from the app as `td_send`. The store is the app's own; TDLib does
not write our tables. Its own message and chat databases are **off**
(`use_message_database`, `use_chat_info_database`, `use_file_database` all
false): we project what we want into `tg_*` ourselves, so there is one
durable view of a chat, not two. What TDLib must keep is its session — the
auth keys and the update cursors — in an encrypted binlog under a local,
**un-synced** directory beside the store, never the store itself, because a
session is a secret and the store replicates.

macOS first. The native build is real work — CMake, a C++ toolchain,
OpenSSL — and Android is a second cross-compile the tree has no setup for,
so the Fold waits. Prebuilt `libtdjson` is vendored per platform rather
than built in-tree.

## The local view

A chat is read from the store the overwhelming part of the time and the
wire is the fallback, so scrolling and search are instant and offline.

- **History.** The worker keeps up to **10 000 messages per chat** in
  `tg_message`, the newest, trimmed as new ones arrive. Scrolling within
  that window is the store's. Scrolling past it asks TDLib for older
  messages, which are drawn but not persisted past the window — deep
  scrollback is paged, not hoarded.
- **Search.** A word in a chat is found locally, over an FTS index of the
  window (SQLite's FTS5, which the bundled build carries). What the window
  does not hold — older messages, a global search across chats — falls
  through to TDLib's `searchChatMessages` / `searchMessages`, and the
  results draw the same way a local hit does.
- **The dialog list, peers, members** are projected whole: they are small
  and wanted always.

"A lot of text, cheap" is the rule: the store may hold millions of
messages across chats and it is still the fast path. Media is the opposite.

## The cache

Media never goes in the store. A photo, a video, a voice note, a sticker
is a file, and files live in a **blob cache** — a generic local store, not
Telegram's and not the shell's, that any app draws from.

- **Generic.** Keyed by an opaque string an app owns — `tg:<remote unique
  id>`, later `mail:<attachment>` — the cache knows no app. It is a kernel
  capability, installed into every world like [`Secrets`](../book/src/architecture.md),
  reachable from a worker's thread.
- **Bounded.** One byte budget, **1 GiB** by default, evicted least-recently
  used when a `put` would cross it. The index — key, file, size, last
  touched — is the cache's own small SQLite database in the cache
  directory, device-local and un-synced, so the budget survives a restart
  without riding the replicating store.
- **The main target.** TDLib downloads land here, not in a TDLib file
  cache: on a completed download the worker ingests the file into the blob
  cache by its remote unique id — a move, or a hardlink — and the row's
  `media_ref` resolves to the cached path. A miss re-requests the download.
  So there is one place media lives and one budget over it, whatever app
  filled it.

The cache is the first piece built, because it is the one that owes nothing
to the wire: it is plain files and an index, and it is proven on its own.

## Phases

1. **The blob cache.** The kernel capability, its LRU, its index, a fake for
   tests, the real one wired beside the store. Proven by unit tests: put,
   get, ingest a file, evict over budget, survive a reopen. *(In progress.)*
2. **The store, projected.** Schema V2 for what the wire carries beyond the
   demo (message flags, download state, the FTS index), the projection from
   an update to a row, the 10 000-message trim, local search with the wire
   fallback. Proven over a fake update stream, no network.
3. **The engine.** The `libtdjson` binding, the build plumbing, the
   per-account worker, the session in the keychain-adjacent directory, the
   `tg/` secret route and the `telegram` config reader. Sign-in by phone and
   code as an `add_account` kind, like mail's. Compile-and-link checked;
   the live wire is the account holder's to exercise.
4. **The verbs, live.** Send, reply, edit, delete, read, mute, archive —
   each an effect over TDLib with the desired/actual split the UI already
   draws, so an undo is a flip and the worker re-converges. Media sent
   through the attach panel reaches TDLib's upload.

## What this session can prove

Phases 1 and 2 are pure Rust and fully tested here. Phase 3's binding
compiles and links only where `libtdjson` is installed, and the live
sign-in needs the account holder at the phone; both are flagged, not
faked green. The demo world stays exactly what a store with no account
shows, so every round-one scene and suite keeps passing throughout.

## Progress — 2026-09-05

Built by Opus agents, each validated independently (clippy `-D warnings`,
tests, the telegram e2e) before the next began.

- **Phase 1 — blob cache. Done.** `kernel/src/caps/blobs.rs`: the `Blobs`
  capability, a 1 GiB LRU over a device-local directory with its own SQLite
  index, atomic writes, transactional eviction, crash reconcile. Wired into
  every world; a denied world has none. Kernel 210 tests.
- **Phase 2 — the projected store. Done.** Schema V2 adds an FTS5 index over
  messages kept by the trigger trio, with a tested V1→V2 upgrade.
  `project.rs` turns normalized updates into `tg_*` rows, idempotently;
  `HISTORY_KEEP` trims to 10 000 per chat; `search_local` runs over the
  index with an injection-safe query and a `WireSearch` seam. App 193 tests,
  e2e 162 steps green.
- **Phase 3 — the engine.** TDLib 1.8.0 installed. The credential plumbing
  is done: the `tg/` keychain route reads the filed api_hash, and
  `config.rs` reads the `telegram` file's api_id and phone. The binding is
  done: `tdjson.rs` wraps the modern JSON interface behind the `tdlib`
  cargo feature (default off, so the tree still builds with no native
  dependency), proven by an offline `td_execute` call. The login brain — the
  transport abstraction, the authorization state machine, the worker loop —
  is under construction, unit-tested against a scripted fake client so the
  flow is verified with no network. Then content projection and the sign-in
  panel.

**The one thing this build cannot self-verify** is the live sign-in: it
needs the account holder at the phone to enter the code. Everything up to
that is built and tested; the morning's first step is to run with the
feature on and log in.

## Final state — 2026-09-06 (overnight)

The client is built end to end and verified. Live login was proven against a
real account: the only blocker was Homebrew's TDLib being pinned at 1.8.0
(2022), which Telegram now rejects (`UPDATE_APP_TO_LOGIN`). Built current
TDLib from source (`brew install --HEAD tdlib` → 1.8.67); login then
succeeded through to the code. No Rust change was needed — the client already
spoke the modern protocol.

Done and validated (clippy clean in kernel, app, and app+`tdlib`; kernel 211
tests, app 223; telegram e2e 162 steps; full battery 41/42, only the
pre-existing `mail/triage`):

- Phases 1-2: the blob cache and the projected store (FTS, 10k trim, search).
- Phase 3: the `tdjson` binding behind the `tdlib` feature, the auth state
  machine, the per-account worker, content projection, the sign-in panel and
  worker registration.
- Phase 4: live sending — send, reply, edit, delete, read — reach Telegram
  through the shared client, gated so the demo build is unchanged.
- Media: photos and video posters render from the blob cache; the worker
  requests downloads at projection and ingests them into the cache.

The one step that needs the account holder is the first live login (the
code). Not yet done, flagged rather than faked: media **sending** through the
attach panel (still a toast), and sticker rendering.

Auto-download is deliberately narrow (Andrey, 2026-09-06: no auto-downloading
videos): on arrival the worker fetches only what the transcript draws — a
photo, and a video's, animation's or video note's **thumbnail** for its
poster. The clip itself, a voice note, a track, a sticker, a document are
never fetched on arrival; they are the player's or the opener's to ask for
when the line is opened, a later phase. A moving picture's `media_ref` is
therefore its thumbnail's key. The device-local `tg_session` would replicate if a sync bucket were
attached; excluding it needs a kernel seam.

## Media cache follow-up — 2026-09-06 (afternoon)

First signed-in run printed `telegram: caching a download failed: No such
file or directory` for photos that had in fact been cached. TDLib's own log
had the mechanism: `Need to redownload file N: Can't find real file path`.
The worker moved each finished download into the blob cache, but the engine
went on counting the file as downloaded at its old path — it re-announced
the file (`updateFile`, completed, dead path) for every later line naming
the same photo, and fetched it off the server again the next time it was
asked. Three changes in `sync.rs`, the blob cache being the one store of
media:

- After a successful ingest the worker sends `deleteFile` for the file id,
  so the engine forgets its copy (remote-only on its side; `tdlib/photos`
  stays empty).
- An announcement whose path is already gone is skipped, not an error.
- A download is requested only for a key the cache lacks
  (`updates::download_key` beside `download_id`), so a forward or a
  re-announced last message never pulls the same bytes twice.

Checked live, headless, against a copy of the signed-in session under a temp
`--db` (20 s of sync): 413 `updateFile`, 67 photos cached, nothing left in
the engine's directories, no cache error on stderr, no `Need to redownload`
in the engine's log. Note: the app's UI tests are not meant to run with
`--features tdlib` — the test session boots the real worker, which opens an
engine client and writes `connecting`.

## Why the chats never came — 2026-09-07

Overnight the sign-in panel read *your chats are syncing* and nothing moved.
The worker was alive — the trace showed 2,600 message updates in — and the
store had 210 chats, 868 peers and **no messages**. `V1` had been widened in
place for the media round after the machine's store had already run its
first shape, so the real `tg_message` lacked `media_ref` and six siblings;
every upsert failed with `table tg_message has no column named media_ref`,
and the worker dropped each failure with `let _ =`. Three changes:

- **A repair rung.** `V4` is a `Step::Run` that reads `PRAGMA table_info`
  and adds whichever of the seven media columns a store lacks, a no-op on
  a fresh `V1`. The rule it enforces from now on: a rung is frozen the day
  a store runs it; what a later round needs is a later rung.
- **No silent writes.** Every store write the worker makes goes through
  `Account::filed`, which puts a refused write in the trace (`!!` lines in
  `tg-debug.log`) and says each distinct error once on stderr.
- **History backfill**, which had been a TODO: with the message database
  off, a chat held only its last line and what arrived live. A chat now
  sends `getChatHistory` as it opens (the newest page, `@extra`
  `history:<chat>:fill:0`); the worker projects each `messages` answer and
  walks on from the page's oldest line — a *fill* down from the newest
  while pages still bring unknown lines (an absence's gap), turning into a
  *tail* from the oldest line held once a page is already known whole —
  until the window holds `HISTORY_KEEP` (10 000) lines or the chat ends.
  One request in flight per open; media is fetched for the first page only.
  Scroll-driven paging past the window is still open.
- The sign-in note now reads counts from the store (`syncing · N chats ·
  M lines`), so an empty message table is visible at a glance.

## The first morning's three — 2026-09-07, later

Relaunched on the repaired store, the client synced *some* of it, drew a
sent line twice, and left the first copy reading *sending…* after the far
side had it. Each is one seam:

- **`FOREIGN KEY constraint failed` on replies.** `V1` made `reply_to` a
  foreign key onto `tg_message` — wrong for a windowed table, where a reply
  may answer a line older than the 10 000 kept or not yet backfilled, and
  a trim past a replied-to line is refused the same way. **V5** rebuilds
  the table without it (copy, drop, rename, index and FTS trigger trio
  again, index rebuilt), proven on a copy of the real store.
- **The pending echo.** TDLib echoes a send under a temporary id with
  `sending_state` pending, then `updateMessageSendSucceeded{message,
  old_message_id}` with the server's copy. Nothing handled the second, so
  the echo stayed beside the real line. `on_sent` deletes the old id and
  projects the settled message (or the failed one). On `ready`, phantom
  `sending` rows from an earlier run are cleared: with no message database
  on the engine's side, no pending send survives a restart.
- **The far side's read cursor.** `updateChatReadOutbox` and the chat
  object's `last_read_outbox_message_id` now land on `tg_chat.read_outbox`
  (V5), and every projection marks sent lines up to it `read` — so the
  tick words move, and a backfilled line arrives already read.
- **The list past 200.** `loadChats` was sent once. It now carries an
  `@extra`; each `ok` asks for the next page, the 404 that ends the main
  list starts the archive, whose 404 ends the load. A chat object's own
  `last_message` is projected on arrival too.

## The transcript and the keyboard — 2026-09-07, afternoon

Reviewed as one flow after the first morning's use (Andrey: focus and
scrolling broken, arrows teleporting; `/` not reaching the filter; send
should scroll to the sent line; new lines should follow when at the
bottom; a way to jump to what a reply answers; a clear sign when
something is loading):

- **The teleport.** `follow` searched the rows the last draw had laid out
  and handed their position to the list — whose indices run over the whole
  transcript. Every walk off the screen scrolled to the top few lines. It
  now finds the line's place among all the rows, and only scrolls when the
  line is not among the drawn ones.
- **Rows shifting under the view.** A backfill lands a hundred older lines
  above the view at a time, and an index-anchored list then shows other
  lines. The widget remembers where it stood as a line (the first message
  row on screen, the rows above it, the scroll within it) and puts the
  list back on that line at the next draw, unless it is following the end.
- **The end.** The transcript's list has `auto_tail`: at the end it stays
  at the end as lines arrive, scrolled up it stays put, and scrolling back
  to the end re-arms it. A send sets the tail, so the sent line is met.
- **`original`** (`o`) on a line that answers another puts the cursor on the
  original and brings it on screen; one the window does not hold is said
  so. The transcript takes the jump as a wish, the way it takes a caret.
- **Loading.** `progress.rs` holds what the engine is busy with, in memory:
  a chat's history walk (`loading…` in the chat's status line) and the
  chat-list load (`chats · syncing…` in the title).
- **`/` by key.** The filter shortcut is known by the slash key as well as
  by the character it types, so a Cyrillic layout — where that key types
  `.` — reaches the filter too.

## Counts, chat verbs and leaving — 2026-09-07

Every group read *0 members*, and every verb about a chat was still a
toast. Both are the same shape of gap — a chat says who it is, not how many
are in it or what one may do to it — so both are closed here.

- **The counts.** `updateSupergroup`, `updateBasicGroup` and the two full
  infos carry the size, the online count and the description; nothing
  handled them. Each is now mapped in `updates.rs` — the group id turned
  into the chat id it stands for, `-1000000000000 - id` for a supergroup
  and `-id` for a basic group — and written with a targeted `UPDATE` onto
  `tg_peer`, the row stubbed first because the group updates arrive before
  the chats that hold them. A basic group's full info is also the one
  update carrying a membership, so it projects `tg_member` too, the creator
  and the administrators marked as running it. A **supergroup's** members
  come from `getSupergroupMembers`, a page at a time, and are a later
  phase.
- **A value once known stays known.** The counts alone would have been
  nulled by the next `updateNewChat`, whose derived peer carries a title
  and nothing else. `UPSERT_PEER` now coalesces every field a source may
  not know — username, about, phone, status, last seen, members, online —
  onto what the row holds. The rule that makes that safe: a mapper says
  what it *does* know outright, so `userStatusOffline` is now the word
  `offline` rather than an absence (else *online* would outlive the person
  being here, and they would sit in the people list's `@online` forever),
  and a nought member count is dropped at the mapper, where it means *not
  yet known*, rather than at the upsert.
- **The verbs, live.** Mute, pin, archive and unarchive on the peer's card
  and over the chat list's marks now reach Telegram —
  `setChatNotificationSettings` (the whole settings object, TDLib taking no
  patch), `toggleChatIsPinned`, `addChatToList`, and `viewMessages` for the
  list's `read` — with the local flip written at the same moment, so the
  bar says *unmute* on this draw rather than on the engine's answer, which
  arrives saying the same. Undo-free on purpose: a flip the wire will
  restate is not a thing to give back. `updateChatNotificationSettings`
  lands the mute the other way, so a chat muted on the phone is muted here.
- **Leaving.** A new verb on the card of anything that is not a person —
  *leave*, wearing `v`, `l` being the workspace's own chord. It sends
  `leaveChat` and takes the conversation out of the store in one write: the
  transcript, the membership, the folders and the chat row. **The peer
  stays.** Leaving is not forgetting — the name is still what a forwarded
  line points at and what a search finds — and it is also what lets the
  card go on drawing, since a card reads the peer and left-joins the chat.
  Then the card closes, there being nothing left to say about a group one
  is no longer in.
- **Off the wire nothing changes.** Where the build links no engine or no
  account has signed in, each verb keeps its toast and writes nothing, so
  the demo world is still exactly what a store with no account shows.
  Leaving is the single exception: it removes locally either way, and says
  what would have left.

## Video plays — 2026-09-07

The viewer's play button moved a drawn line across a timeline that was
counting the clock, not a clip: a video line held one file, its poster, and
the clip behind it had no name in the store at all. Now it has two, and the
button drives the platform's own player.

- **A sixth rung.** `V6` adds `media_clip` and `media_clip_rid`. The first is
  where the clip's bytes will be — the same `tg:<remote unique id>` key every
  other file has, so a landed download resolves through the one path media
  already takes. The second is what the clip is asked for by: TDLib's
  `remoteFile.id`, which outlives a session, where the `file.id` a
  `downloadFile` runs on is only this run's and would be a stale number by
  morning. `media_ref` is untouched — the poster is still what the transcript
  draws. The rule from `V4` held: a new column is a new rung.
- **Named, not fetched.** `updates::content` fills both for `messageVideo`,
  `messageAnimation` and `messageVideoNote`; `download_target` is unchanged,
  so what the worker pulls on arrival is still the poster and only the
  poster. Naming a file is not asking for it (Andrey, 2026-09-06: no
  auto-downloading videos).
- **Asked on opening.** Opening the viewer on a clip — or pressing play — is
  what fetches it: a `getRemoteFile` on the row's remote id, which the worker
  turns into a download at the front of the queue, landing in the blob cache
  under the row's own key through the same `updateFile` path as everything
  else. Once, per open; the panel says *downloading…* and looks again each
  draw, since a file appearing under the cache's name announces itself to
  nobody.
- **The player.** `MediaVideo` joins the shell's media kit beside
  `MediaPicture`: makepad's `Video` over a `Filesystem` source, its own
  overlay controls off, because play and pause are the panel's and a moving
  picture should be transported the way a sound is. A verb has no `Cx`, so
  the panel keeps the wish and the draw carries it out and hands back where
  the player was left — which is how a clip that has run out puts the button
  back to *play*. The box takes the poster's place only once the player
  really has a picture in it; a clip still preparing is a rectangle, and the
  poster is the better thing to look at. The timeline reads the player's own
  position and duration, falling back to the row's seconds until the platform
  reports one.
- **Unchanged without the engine.** A demo line names no clip and a build
  with no `tdlib` can ask for nothing, so both keep the poster and the fake
  timeline: the panels library, the demo world and `e2e/telegram/basic` draw
  exactly what they drew before (162 steps, headless, green). Playing a voice
  note or a track is still the fake timeline — a sound has no picture to hang
  a player on, and the `Playback` capability that would play one is a later
  phase.

What this build cannot self-verify is a clip actually playing: that needs a
signed-in run with a real video in a real chat, which is the account holder's.
Everything up to the `AVFoundation` call is compiled, clippy-clean with and
without the feature, and green headless.

## Only my chats — 2026-09-07, evening

The list held channels never joined ("дядя сэм"). The engine announces
every chat it learns of the same way — one I am in, one a line was
forwarded from, one a reply was quoted out of, a peer who was mentioned —
and the first shape of `tg_chat` took each announcement for a conversation
of mine. What tells them apart is the chat's *positions*: a chat of mine
has a place in the main list (or the archive), a chat merely seen has
none. `tg_chat.in_main` (rung V7) now carries that, read off the positions
at `updateNewChat`, moved by `updateChatPosition` (order nought is out) and
by the engine's own `updateChatAddedToList` / `updateChatRemovedFromList`.
The list is `archived = 0 AND in_main = 1`; the archive is `archived = 1`.
The rung resets a signed-in store's chats to *out* and lets the list load
re-list what is mine, so the next launch shows the list empty for the few
seconds the load takes (`chats · syncing…`) and then only my chats. The
demo world's rows keep the column's default and stay listed.

## The macOS client as the reference — 2026-09-07, night

Andrey's brief said to take the existing Telegram client for macOS as the
reference, and the day's fixes came from his reports instead. This is the
pass that should have come first: what that client does in the surfaces
this app has, and where this app stands. *Done* is in; *now* is this
round; *later* is named so it is not forgotten.

**The list.** Only my dialogs, pinned first, newest first — *done*.
Unread and mention badges, muted, drafts, `you:` prefix, sender prefix in
groups, typing — *done* (typing: *now*, from `updateChatAction`). The
archive — *done*. Folders as tabs — *later* (the folder tag exists).
Pin / mute / archive / mark read / leave — *done*. Delete a private chat —
*now*. Mark as unread, clear history — *later*.

**The transcript.** Opens at the first unread line with the divider, else
at the end — *now* (it opened at the end). Composer focused on open,
Enter sends, Shift+Enter breaks the line, Esc drops an edit then a reply —
*done*. Up in an empty composer edits my last line — *now*. Send scrolls
to the sent line; new lines follow only at the end — *done*. Home / End
to the oldest and newest line — *now*. A press on the quoted reply jumps
to the original — *now* (the verb was there). Reply, edit, delete for
everyone, forward — reply/edit/delete *done*; forward *now*, through a
pick of the chat. Copy a line's text — *now*. Date separators, sender
names, forwarded and edited marks, views, reactions as words — *done*;
reactions changing live — *now* (`updateMessageInteractionInfo`). Adding a
reaction, pinning a line, the pinned bar — *later*. Older history on
scrolling to the top past the window — *later* (10 000 lines are kept).
Search in the chat — *done*, on the about card.

**The header.** Online / last seen — *now* (`updateUserStatus`; the field
was mapped but the update was not handled). Typing — *now*. Members and
online for groups, subscribers for channels — *done*. A title change —
*now* (`updateChatTitle`). Avatars — never, by the design language.

**Media.** Photos inline and in the viewer, posters for moving pictures,
video playing on demand — *done*. Files: name and size — *done*; download
and open — *later*. Voice and audio playback — *later* (needs a decoder).
Stickers — *later* (webp / tgs). Locations on a map — *done*.

**Sending.** Text, reply — *done*. Photos, videos, files from the attach
panel — *now* (`inputMessagePhoto` / `Video` / `Audio` / `Document` over
`inputFileLocal`; a caption on the first). A location — *now*. Voice and
video-circle recording — *later* (capture). Paste an image, drag a file
in — *later*. Drafts kept per chat — *done*; drafts synced with the
server — *now* (`updateChatDraftMessage` in, `setChatDraftMessage` out
as the chat is left).

**Joining.** A channel or group merely seen offers *join* — *now*
(`joinChat`). Invite links — *later*.

**Elsewhere.** Notifications and the dock badge — *later*. Link
previews and clickable links — *later*. Polls, contacts — drawn as text,
*later* as content. Secret chats — never (TDLib parameters have them
off).

A guard found on the way: a sent photo's `updateFile` names the account
holder's own file on disk as the local copy, and the cache's ingest
*moves* its source. The worker now copies a file that is not the engine's
own into the cache, never moves it.

## Presence, drafts, joining — 2026-09-07

The reference pass named a dozen small things the client does and this one
did not. These are the ones that are a *store column with nobody filling
it*: the field was there, the update was dropped.

- **Presence.** `updateUserStatus` — the one update that moves *online* and
  *last seen* between the rare refreshes of a user object — now lands on
  `tg_peer.status` and `last_seen`. Written as a targeted update, the way a
  group's counts are: the status is all it knows, and a status this build
  has no word for is dropped rather than nulling the last one, the rule
  from *a value once known stays known*.
- **Typing.** `updateChatAction` puts the sender's name, as the store knows
  it, in `tg_chat.typing` — *someone* where it knows none — and the cancel
  takes it out. But the server's `chatActionCancel` is a courtesy and not a
  promise, so every client expires an action on its own clock: the worker
  keeps a deadline per chat, six seconds out, and the pass that finds one
  spent clears those rows in a single write. It is a thing the clock says,
  not the wire, so it belongs to the pass rather than to any update.
- **The rest of a chat's own updates.** `updateChatTitle` onto the peer's
  name (a chat and its peer share one id), `updateChatUnreadMentionCount`
  onto the badge, and `updateMessageInteractionInfo` onto the views, the
  comments and the reactions line. That last is written *whole*, unlike
  every coalescing projection beside it: TDLib sends the interaction info
  entire, so a reaction taken back is an absence that has to reach the row,
  else the last emoji a post ever wore would stay on it forever.
- **Drafts, both ways.** `updateChatDraftMessage` in — a line half-typed on
  the phone shows here, and a null draft clears it, an absence being the
  news — and `setChatDraftMessage` out, as the composer is left.
- **Joining, deleting.** `joinChat` on the card of a group or channel the
  engine merely learned of, and `deleteChat` on a person's. A chat of mine
  is one with a place in a list, so the card reads `in_main` (or the
  archive) and wears exactly one of *join*, *leave* and *delete chat*.
  *join* writes nothing: what follows it is the engine's own
  `updateChatAddedToList`, and a flag guessed here would only be one to take
  back if the join were refused. *delete chat* is *leave*'s shape over a
  person — the same local removal, the same close, the peer still known.
- **The guard on the way in.** A sent photo's `updateFile` names the account
  holder's *own* file on disk as the local copy, and the cache's ingest
  moves what it is handed: sending a picture would have taken it out of the
  folder it was chosen from. A path outside the engine's own directory is
  now read and `put` under the same `tg:` key, the file left where its owner
  keeps it, and no `deleteFile` follows — there is no engine copy to forget.

## Sending media, forwarding — 2026-09-07

Five of the *now*s of the pass above, taken from what the macOS client does.

- **Media out.** A composer carrying files sends each as a message of its
  own — `inputMessagePhoto`, `Video`, `Audio` or `Document`, by what
  `Carried::kind` makes of the name, over an `inputFileLocal`; the upload is
  the engine's. The words in the field ride as the **caption of the first**
  file, and the line it answers with them, so a picture with something
  written under it is one message and not two. The rest go bare — an empty
  caption is an empty `formattedText` rather than an absent field, TDLib
  taking the content object whole. Nothing local is written: each sent line
  comes back through `updateNewMessage` the way a text send's echo does.
- **A place.** The picker's *send* is `inputMessageLocation` with
  `live_period` nought — the one-off share, and with it the heading and the
  alert radius only a live location moves. *live 1 h* is still a toast: a
  location that goes on moving wants a `Location` capability to keep it
  moving.
- **Forward is a pick, not a verb.** The client raises a sheet of chats;
  here a picker is a panel, so the lines have to outlive the transcript that
  let them go. *forward* over the marks — or the cursor's line — puts them
  on the app (`Telegram::carry_forward`), drops the marks, opens the chat
  list joined and says *pick a chat to forward to*. While they wait, that
  list wears **forward here** (`f`), which sends `forwardMessages` to the
  chat under the cursor with `send_copy` false, so each arrives wearing the
  *forwarded from* the transcript already draws. The pick spends the
  forward whether or not the build is signed in — a pick made is a pick
  made — and *clear*, which the list now wears for a waiting forward as well
  as for a marked set, is the way out of one; esc means the same thing.
- **Drafts out.** The composer's text goes to the server as the chat is
  left: `setChatDraftMessage`, or a null draft where the field is empty.
  The panel keeps what the server was last told, so a chat opened and closed
  with nothing typed in it says nothing. The going is the panel's `Drop` —
  a `Panel` has no `close`, and the instance is the only thing that knows
  the draft moved.
- **Copy.** *copy* on the transcript's cursor and on a line's card puts the
  line's words on the clipboard, and a line with none — a recording, a
  picture — puts what the transcript says it is (*voice 0:12*). Through the
  `Clip` effect, like every other copy in the shell, so the log keeps the
  row and a world that may not touch a human's clipboard refuses it out
  loud.

Off the wire every one of them is what it was: the toast says what would
have left, the composer empties, and the demo world is unchanged (the
telegram e2e is 162 steps green). What no build here can prove is the far
end — that a photo lands as a photo, that a forward wears the right
attribution, that the draft is on the phone. That is a signed-in run's, and
the account holder's.

## Throttled — 2026-09-07, late

The clip never played because it never arrived: the account was being
throttled. Every history answer fired the next page at once, and every
chat opened walked on its own, so the trace showed some 140 pages a
minute; TDLib's log had 2,500 `messages.getHistory` queries delayed on
thirty-second flood waits, and `upload.getFile` waited too, with a
hundred thumbnails queued per chat opened. (A two-hour wait on
`contacts.getBirthdays` in the same log is the engine's own call, not
ours.)

History is now a queue on the account: one page on the wire at a time,
a second apart, the chat just opened at the front and every walk's next
page at the back, so opened chats take turns. A refused page with
*retry after N* holds the queue that long and goes again; any other
refusal ends that chat's walk and its *loading…*. Panels no longer send
history requests; they say `want_history` and the worker takes it on its
next pass. A chat opening fetches the pictures of its newest forty lines
rather than a whole page's. The clip request lets the engine infer the
file's type, and the answer is traced in full, so a clip that still
does not download is diagnosed from the trace rather than guessed at.

## The seconds that counted over a still — 2026-09-07, later still

With the throttling gone the clip still did not play, and the trace held
no clip request: the video lines opened were from before the clip's
remote id was kept on the row (V6), so the viewer had nothing to ask by,
and the stand-in timeline — the demo world's, over the row's seconds —
ran instead, counting over the poster. Two changes: a viewer on a moving
picture of the wire never runs the stand-in timeline; and a line without
the id is fetched afresh (`getMessage`, `sync::want_line`), lands
re-projected with the id, and is asked for on the next draw. The rows
from before the column — 1,659 video lines in the store that morning —
fill in one by one as they are opened, and no rung need touch them.

## The box that stayed hidden, and whose chat it is — 2026-09-07, night

With the clip in the cache the player still showed nothing. The platform
prepares a clip a moment after it is asked and tells the video box so —
but the box is kept invisible until it is playing, a box nobody has drawn
has no area, and its own redraw request is a request over nothing. The
viewer had stopped drawing the moment the download note went away, so no
draw ever found the player ready. The viewer now keeps drawing while
play is wanted and nothing shows yet; the box stays hidden until the
platform has it playing, so the poster stands until the first frame.

A review comment on the card's *delete chat*: TDLib's `deleteChat`
deletes for every member wherever it may — the other person's history
too, with nothing said. The card now sends `deleteChatHistory` with
`revoke` false, which is my side alone, in both of the client's shapes:
*clear history* (`r`) keeps the chat and empties it, *delete chat* (`d`)
takes it off the list as well.

## Review findings, the small ones — 2026-09-07, night

Fifteen review comments came in on the diff; one, `deleteChat`, was
already answered above. Nine were small and real, each now pinned by a
test: an archive position bound three parameters to a two-parameter
statement and failed every time; a content change did not carry the
clip columns; a send did not clear the server's draft (`clear_draft`,
which an edit must not set); a media line's edit went as a text edit
and was refused (`editMessageCaption` now); the files app's `~/`
spelling reached the engine unresolved; a full window ended a fill
before it had closed an absence's gap (the tail alone stops at the
cap now); reactions come wrapped in a `messageReactions` object on
current TDLib and were dropped; a channel's owner or posting
administrator could not post, the supergroup's `status` being
ignored (`admin` now, and a chat object's peer never demotes it); and
the viewer marked a picture shown before it had decoded, so one that
arrived later stayed blank (shown is recorded on decode, keyed by the
file). The structural ones — message identity per chat, photos on
demand, real clients leaking into fixture sessions, a refused send
losing the draft, Saved Messages resolving to the account, a phone's
draft reaching an open composer — follow in their own notes.

## Review: identity and photos — 2026-09-07

Two of the structural findings, both about a number asked to mean more than
it does.

- **A message id is not a message.** `tg_message` was keyed on `id` alone, as
  though Telegram numbered a line once for the whole account. It does not: a
  supergroup's and a channel's ids are the server id shifted twenty bits, so
  every channel's first post is 1 048 576 and any two channels collide on
  every number they use — a second channel's post overwrote the first
  channel's row and carried it into the other conversation. **V8** rebuilds
  the table with a key of its own: `seq`, a fresh integer per line, with `id`
  and `chat` plain columns under a `UNIQUE(chat, id)` that says what identity
  really is. Every row is copied across keeping its old rowid as its `seq`,
  the index is made again, and the full-text index — which is keyed by its
  content table's rowid — is dropped and made again with
  `content_rowid='seq'`, the trigger trio speaking of `new.seq` and
  `old.seq`, and rebuilt from the rows. A pair that had already collided is
  one row and stays one; that line was lost the day it landed, not here.
  Then every reader names the chat as well as the id: the upsert's conflict
  target, the search's join, the chat list's last line, the quote a reply
  carries, the trim, and the messages list's own key and order (a mark, a
  cursor and the row a hidden mark fetches all name one row). `V1`..`V7` are
  untouched — a rung is frozen the day a store runs it.
- **A photo could only ever be fetched once.** The worker asks for a picture
  as its line arrives, and for the newest forty lines of a chat as it opens;
  past that, and after the cache has evicted, there was nothing on the row to
  ask by — the file id an update carries is this run's and a stale number by
  morning. `media_rid` (V8) keeps the durable one, TDLib's `remoteFile.id`,
  of the file `media_ref` names: a photo's largest size, a moving picture's
  poster. `request_clip` is now `request_file`, under an `@extra` of `file:`
  (the old word is still answered — a session may have asked under it), and
  beside `want_line` there is `want_file`, a queue the worker spends on its
  next pass, deduplicated. The viewer asks when it opens on a picture with no
  bytes, once, and keeps drawing until they land; the transcript asks for a
  row drawn without them, once per line. The answer lands in the blob cache
  under the key the row already names, and the next draw finds it, exactly as
  a clip's does.

Off the wire nothing moved. The demo world now numbers its own lines rather
than reading the row key back as the message id — the one place the two were
ever the same thing — and `e2e/telegram/basic` is 162 steps green.

## Review: sessions, sends, self, drafts — 2026-09-07

Four findings from a reading of the client as it stands. Each is a seam
where the engine and the panels met on an assumption.

- **One session owns the engine.** `App::workers` registered the real worker
  for *every* store, and a Panels Library mount — like every test — runs its
  passes inline, so each opened a TDLib client of its own. The receive queue
  is the process's and not a client's: a second client drains the signed-in
  account's updates into a store of demo rows, and a fixture's verb, reaching
  the one shared transport, would send a real message from a drawing. The
  account holder's store is now named once — the directory the one world
  built `Mode::Real` and unscripted was opened over, filed as that world is
  built (`Telegram::outside`, `engine_store`) — and the worker is registered
  for that store alone. `wire` and `told` ask the same question of the store
  the panel reads, and a chat opening asks for its history only where the
  answer would be its own. The panel tests now pass with `--features tdlib`
  linked, which they could not before.
- **A refused send gives its words back.** `wire` answers *it went* the
  moment `td_send` takes the JSON, and the composer is emptied on that
  answer — so a request TDLib refuses outright (a line past the length limit,
  an attachment that is not there, a chat one may not write in) took the
  words with it. Each of the three sends now wears an `@extra` of
  `send:<chat>:<the words>`, which by the time the refusal arrives is the
  only copy of them left; `on_reply` puts them back as the chat's draft, and
  the refusal in the trace. Onto a composer typed in since they do not go:
  that draft is the newer one. The line answered and the files carried are
  not given back — neither is on the row — so what returns is the half that
  cannot be typed again from what is on the screen.
- **Saved messages is the account holder's.** The launcher's root names
  `seed::SELF`, the demo world's stand-in self, and nothing ever resolved it:
  signed in, the notes-to-self opened the demo conversation. `updateOption`
  is now handled — `my_id`, the one thing the engine says about itself that a
  row depends on, its int64 read as a number or as a string — and it marks
  that peer `is_self`, stubbing the row first, the stand-in giving the flag
  up. `ChatKind::open` resolves the root through `model::self_peer` at the
  moment it opens, so the launcher keeps one identity for the entry however
  many accounts pass through the store, and a fixture — which has no
  signed-in self — opens exactly the conversation it drew before.
- **A draft reaches the composer it is for.** `updateChatDraftMessage` writes
  `tg_chat.draft` and the list drew it, but an open transcript had read its
  draft once, at construction, and went on showing the stale string. The card
  is read on every draw, so that is where the row and the composer are
  reconciled: the panel keeps what the row last said, and a row that has
  moved since is taken into the field where the field still holds that same
  value or nothing at all. Where somebody has typed here, the local words
  stand — what one is in the middle of writing is not another device's to
  overwrite — and the leaving tells the server about it, as before. This is
  also what carries a refused send's words back into the open composer.

Two of the same shape are left, in files this pass did not own: the `wire`
copies in `panels/line.rs` and `panels/media.rs` are not gated on the store,
so a card's *delete* and a viewer's file request would still reach a real
account from a library mount — each wants the one line the other two got.
And `project::UPSERT_PEER` re-states `is_self` from a user object, which
never knows it (`updates::peer` says `false` outright), so a later
`updateUser` for the account holder clears the flag `my_id` set: that column
wants the coalescing rule the counts already have, `MAX(excluded.is_self,
tg_peer.is_self)`, or *saved messages* goes back to the demo self until the
next launch.

## Why no clip ever played — 2026-09-07, last

Read off makepad's own player rather than guessed: AVFoundation tells a
file's container by its extension, and the cache names a blob by its
hash alone. Every prepare failed on the bare name, the widget went back
to *unprepared* on the error and was asked again on the next draw, and
nothing was ever said on screen. Makepad's in-memory path sniffs the
container and writes a temp file *with* the extension for exactly this
reason; the filesystem path we used does not. A clip is now reached
through a link beside the cache, `blobs-play/<hash>.<ext>`, the
extension read off the bytes (`ftyp` is mp4, an EBML header webm).

Two things make the next report diagnosable rather than described: the
viewer writes the player's state to `tg-debug.log` on every change
(`video: line N player preparing → playing`), and the viewer's `open`
verb hands the file to the system's player — the sure way to see a clip,
and the proof that the file itself is sound. `MAKEPAD_FORCE_SOFTWARE_VIDEO=1`
in the environment makes makepad decode in software, the other half of a
bisection if the native player still refuses.

## Inline play, and seven more findings — 2026-09-07, late night

The viewer plays; the row did not. A row is a reused list item and the
platform's player is one widget with one file, so a real clip's play
button in the transcript now opens the viewer on the line, playing
(`progress::play_on_open`, spent as the viewer opens); a demo line and a
recording keep the row's own timeline.

Seven review findings, all real: a phone's draft could overwrite unsent
words, because typing writes the row too and the row agreeing with the
composer was read as nobody having typed — *untouched* now means the
composer holds what the server was last told, or nothing; a first load
of a big chat filled past the cap, each page trimmed on landing — a fill
goes past the cap only while it closes a gap above lines already held;
a relative `--db` made a relative link that pointed nowhere — the link's
target is the blob's real path; a file's or a sound's viewer drew forever
waiting for a picture it has none of — only a kind with a picture waits;
play pressed mid-download was lost when the draw with no file yet wrote
*not playing* back — the wish stands until the file comes; a photo
swapped under the same line kept its old picture in the transcript — the
box remembers the line *and* the file; and a photo row from before the
remote id was kept could not be fetched — it is fetched afresh first, as
a clip's row is.
