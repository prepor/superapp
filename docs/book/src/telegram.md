# Telegram

Telegram is an app over a local SQLite projection. Panels read that projection
and own their interaction state; a background worker owns the TDLib client and
projects its updates. The `tdlib` feature enables the native client and is on
by default. Library fixtures and scripted runs stay offline with demo data
even when the native client is linked.

Conversation panels use the `telegram-chat` tag, distinct from the agent app's
`chat` tag. List rows, search results and saved messages share that identity.

Chat previews prepare their cached transcript on a background reader. Draws
reuse those rows and prepare players only for visible messages. Rapid cursor
walks prioritize the latest chat and discard obsolete queued reads; the eight
most recent transcripts remain cached. Updates keep the current transcript
visible while its replacement loads. Photo reads, decoding and map rendering
also run on workers, with a bounded texture cache shared across chats. Opening
an unread chat still records its read claim and preserves its unread divider.

Unread chats open with the divider near the top and a small amount of context
above it. Short unread runs leave space below; incoming messages fill that
space without moving the reading position. Scrolling down at the end, sending,
or explicitly jumping to the newest message resumes the usual bottom view.

Scrolling and mention checks look up messages in the prepared transcript, and
unchanged message text reuses its formatted links. Typing updates the composer
immediately; local drafts save after a 300 ms pause or when leaving the chat.
Sending and explicit draft replacements cancel the pending save.

Automatic history, message and reaction refreshes wait until a chat has stayed
visible for 350 ms. Arrow-key previews show cached data immediately; traversed
chats leave no refresh queue behind. Visible copies of the same chat share one
history walk, including empty chats waiting for their first messages. Leaving a
chat cancels its queued pages and thumbnail requests; late history replies cannot
restart an abandoned walk. Telegram's page pacing and retry waits still apply.

Groups upgraded to supergroups show their original history in the new
conversation. Telegram's upgrade metadata links the two source chats;
`tg_chat_upgrade` stores that link across restarts. Cached older messages appear
as soon as the link is known, and the original group's history loads through the
same paced, cancellable queue. Each source retains the existing 10,000-message
cache limit. Forum-topic transcripts remain scoped to their own topic.

Messages retain both their source chat and message id, including cursor and
scroll anchors, selection, playback, reactions, edits, deletion and forwarding.
Replies can cross the upgrade boundary: `reply_chat` identifies the source chat
when it differs from the replying message's chat. Replying from the new group's
composer uses TDLib's `inputMessageReplyToExternalMessage`.

Viewing newer messages in the focused conversation advances its read position,
including messages loaded or received after the panel opened. Live unread counts
follow Telegram's acknowledgment; unacknowledged views retry while visible.
Hidden conversations and background windows send no viewport read receipts.
Switching conversations waits for the replacement transcript to be drawn before
acknowledging any of its messages.

With the transcript focused, **Ctrl+E** or **End** jumps to the end of the chat,
selects its newest message and resumes following new messages; **Home** selects
the oldest loaded message. In the composer, **Ctrl+E** moves the caret to the
end of the current line, and **Shift+Ctrl+E** extends the selection to that point.

Typing **@** at the start of a word offers participants' usernames above the
composer. Suggestions match names and handles, using cached participants first
and Telegram's mention search for the current chat and topic. Arrow keys choose
a suggestion; **Enter**, **Tab**, or a click inserts it, and **Escape** dismisses
the offer. The next Enter sends the completed message. Opening message search
puts the caret after the initial chat filter, ready for a query.

Click a reply's quote or use `original` (`cmd+o`) to jump to the message it
answers. The chat bar then offers `back` (`cmd+b`) to return to the reply.
Following several originals keeps each return point, so repeated `back`
retraces them in order. This history belongs to the open panel; replies that
have been deleted or are no longer loaded are skipped.

Returning or walking the transcript reveals the message using its measured
height, including in a short window or among large media messages. The reveal
stays pending until a draw confirms the message is visible; a new scroll
gesture takes over from it.

Message text and media captions show selectable, underlined links in the
transcript and line cards. Web URLs, labeled links, email addresses and Telegram
links open their destination on a tap; dragging selects text. Incoming entities
are retained with the message and replaced on content edits; their spans are
authoritative even when the server reports no links. Only text without metadata
uses local detection, which requires a URL scheme or an email address so filenames
and ordinary dotted words stay plain. Older cached labeled links and URLs without
a scheme become available when their entities are fetched again. Code spans stay
literal.

## User actions

Sends (including attachments and forwards), edits, reactions, chat notification
changes, pinning and archiving appear in the undo tree. **Cmd+Z** requests their
reversal on Telegram; redo reapplies them. A pending command settles before its
reversal runs, and failures appear in the Telegram status strip and Problems.
The transcript continues to reflect Telegram's updates throughout.

Undoing a send requests **delete for everyone** using the delivered message ids.
Redo sends a new message; later edits and reactions in the same history branch
follow its new id. Undoing an edit restores the previous text or caption with
its original formatting. Reaction undo removes the added emoji and restores
your previous selection, including one displaced by Telegram's reaction limit.
Adding an emoji you had already chosen does not remove it on undo.
When Telegram omits a message's reaction list, the app loads reaction metadata
and confirms the previous state with a server read before adding the emoji.
Batch mute, pin, archive and unarchive commands over chats take one undo or
redo press for the whole selection; each chat waits for its own acknowledgement.

Deleting your own supported messages saves their content before sending the
deletion to Telegram. **Undo resends copies** as new messages, with new ids and
timestamps; redo deletes those replacements. Text and caption formatting,
topics and reply targets are retained, including replies between messages
restored in the same batch. Existing reactions, replies from other messages,
forward attribution and album grouping are not restored.

Photos, documents, videos, animations, audio, voice notes, video notes and
stickers keep independent local copies of their file bytes for undo. Missing
files are downloaded first; a failed or incomplete backup leaves the originals
on Telegram. Contacts, static locations and venues can also be resent. Backups
belong to this session's history and are removed when it releases them.

Messages from other people and unsupported content (such as polls and service
messages) retain the **cannot undo** label. Undo skips these nodes, since a new
send cannot reproduce their original sender or behavior. Protected,
self-destructing, scheduled or still-pending messages cannot be backed up for
resending; if the snapshot finds one, that deletion is canceled.
Other actions also become unavailable for undo if their previous server state
could not be read, or Telegram rejects the reversal. Pending or uncertain sends
are never automatically resent. History and its reversal data last only for the
current session. Offline edits, deletes and reactions remain locally undoable.

A conversation's **about** link opens the person's profile. **Block user**
prevents incoming messages and hides your status and photo; **unblock user**
reverses it. A blocked conversation keeps its history and draft, shows
**blocked**, and offers **unblock user** in place of attachments and sending.
Reply and edit actions also disappear from individual message cards; work
already in progress stays hidden until the person is unblocked.
Blocking and unblocking are unavailable on your own profile or on groups
and channels.

**Delete contact** removes the person from the address book and keeps their
conversation. **Delete chat** removes your copy of the conversation from your
chat list and local search, preserving the person's contact and blocked state.
The other person's copy remains. Blocking, deleting a contact and deleting a
chat first show their consequence, with **confirm** and **cancel**; Escape
also cancels. Unblocking takes effect without another confirmation.
Blocked people remain findable by name in search after deleting their contact
and chat, so their profile still provides a way to unblock them.

These distinctions follow the macOS reference client's
[user info actions](https://github.com/overtake/TelegramSwift/blob/579cebbf0c01fd41b712eff3647fa7f69db9665d/Telegram-Mac/UserInfoEntries.swift#L662).
Requests use TDLib's `setMessageSenderBlockList`, `removeContacts` and
`deleteChatHistory` with `revoke: false`. Local changes wait for a successful
reply or a server update; failures are shown in a toast. Offline fixtures
describe the request without changing contacts, blocks or conversations.
Ending a session or stopping its worker clears pending actions and reports
the missing confirmation, so they can be retried after reconnecting. Late
replies cannot complete a later attempt.
Block state also follows chat snapshots and user full info, including changes
made on another device; the stories-only block list does not block messages.

## Unread replies

The chat list shows an `@` count beside the ordinary unread count. **Replies &
mentions** on its bar opens unread messages addressed to you in groups,
including muted and archived groups. The launcher has the same entry; a
group's bar opens the view with that group as its initial filter. Selecting a
message opens the conversation at that message.

Telegram counts replies to your messages and direct mentions together. The
view uses its unread-mention search, fetching older items even outside the
ordinary history window. Opening or previewing a group preserves this count;
notifications are acknowledged when their messages are visible in the focused
conversation. A live count clears on Telegram's acknowledgment, including
when another device reads the message. **Refresh** retries an incomplete load.

## Topics as chats

Use **topics** on the chats panel, choose a forum group (for example,
Вастрик.Берлин), then check the topics you want to see. The picker supports
text filtering, individual toggles by click or space, and showing or hiding
all matching topics. Changes take effect immediately and survive a restart.
Each toggle or bulk change can be undone and redone during the current session,
restoring the previous mix of shown and hidden topics.
The group stays available in the picker even when no topics are selected.

Selected topics appear beside ordinary chats, labeled **topic · group**.
Opening one shows its own transcript and composer. History, drafts, unread
counts, replies, attachments and forward destinations retain both the parent
chat id and the topic id. General is a topic too. **topics** on a topic's
conversation or the group's card opens the picker again.

Topic selection, pinning and archiving are preferences for this app's panel;
they do not change the official Telegram client's topic layout. Muting and
reading a topic do go to Telegram. Refresh reloads the group's topic catalog,
including pages beyond the first hundred topics, without replacing the
selection. A failed refresh keeps the cached catalog and offers a retry.
Topic updates are applied directly; they never trigger another request for
the same update. Metadata requests are deduplicated while pending, and topic
refresh and history wait for the current client's sign-in and chat-list load.
Forum capability follows group or bot metadata; ordinary-chat messages with
stray topic ids stay in their normal chat, and refreshed metadata repairs
earlier cached assignments.

The topic protocol uses the installed TDLib API's `messageTopicForum`,
`getForumTopics`, and `getForumTopicHistory` types. The V13 startup check adds
and repairs topic metadata, message membership and the chat-list view that
combines ordinary chats with selected topics. The existing link, block and
unread-mention migrations also check their columns, including topic builds
that already used V12, so upgrades preserve messages, link metadata, drafts
and selections.

## Agent drafts and sends

The `line` panel's agent context describes one message, identifies its chat
and message arguments, and explains its reply and editing actions. The full
cached text or caption and quoted reply are rendered as text blocks with
their whitespace preserved, alongside the SQL and row metadata.
They are re-read when the chip is sent or `panels.context` is called, so
edits made after attaching a panel reach the agent. Table previews retain
their 200-character cell limit; the full text uses the panel's 32 KiB budget.

Agents find recipients and read cached history with `sql.query`. Telegram's
data dictionary explains chat and topic ids, the cache's limits, and why raw
SQL writes cannot send messages or manage the live composer.

`telegram.file` reads an attached file by `chat` and `message` id. It refreshes
the source message before downloading the full attachment, then reads from
the local media cache; cached files work offline. It works without a
Telegram panel open, leaves read receipts alone, and creates no copy in
Downloads. PDF text layers and UTF-8/UTF-16 text files up to 32 MiB are
supported. Results contain at most 64 KiB of text; pass `next_offset` as
`offset` to continue. Download failures, unsupported binary formats and
PDFs needing OCR are reported to the agent.

`telegram.draft` opens or reuses the destination's composer with the requested
text, optional reply and `files`, an ordered list of local paths. Paths must
be absolute or start with `~/`; `files.list` can find them. PNG and JPEG images
send as photos, GIFs as animations, video and audio as their media types, and
other files as documents, using the same rules as the attachment panel. Each
file becomes its own message; text and the reply belong to the first. Use
`text: ""` to attach files without a caption. Files must be readable, nonempty
regular files and stay available until delivery.

Forum topics keep their own destination. Existing draft text requires explicit
`replace: true`; edits must be finished in the panel. Existing attachments
must remain at the start of `files` in their current order, even with replace;
new files can be appended. If the list doesn't match, the error includes the
existing paths as a JSON array in their current order. All open copies of the
composer receive the staged draft, and long drafts scroll inside a bounded
field. Draft text persists like typing; reply selections and attachment paths
belong to the open composer.

For example, `telegram.draft` accepts:

```json
{"chat": 123456789, "text": "The screenshot and report", "files": ["~/Pictures/screenshot.png", "~/Documents/report.pdf"]}
```

Use the recipient's actual chat id from `sql.query` and pass the returned
arguments to `telegram.send`.

`telegram.send` takes the returned slot, chat, topic, text, reply target and files.
It uses the same send path as Enter, after the agent's ordinary approval card.
If the text, destination, reply or attachment paths or order changed while
approval was pending, it refuses the send. It rechecks file availability before
queuing anything. Offline failures keep the draft. Queued text and attachments
clear from every matching open copy of the composer. Each send records one
history action; multi-file sends are labelled with the attachment count.
Undo requests deletion of every sent message, waiting for each delivery
acknowledgement. Rejected or uncertain sends still consume their own Undo step,
preserving the previous action; this applies to text, one attachment and multiple
attachments.
Rejected or uncertain files do not block deletion of delivered attachments.
Their receipts remain watched, so a late confirmation also follows the Undo.
Redo restores deleted attachments without retrying rejected or uncertain sends;
a known deletion failure blocks Redo for the whole send.

A successful tool call reports **queued**, with an `operations` list containing
one id per message; `operation` is the first id for compatibility with text sends.
Check each id with `telegram.status` to learn whether Telegram confirmed it or
returned an error. All files queue together on one worker connection; a queue
failure keeps the whole draft. Telegram still confirms or rejects each message
individually. Pending or uncertain delivery must not trigger an automatic
repeat of the original send. Operation ids last for the current app session;
completed send results remain queryable after their status line disappears.

## Reactions

Select a message and press **Cmd+J**, or choose **react(j)** in the chat's
bar or its line card. The shortcut also works while the composer has focus.
The bar shows the ordinary emoji Telegram allows for that message in evenly
spaced, borderless choices, six at a time; **more** and **back** move through
them. Choose an emoji to add it, or **cancel** / Escape to close the picker. Moving the chat's
cursor also closes it. Service messages and pending or failed sends do not
offer reactions, and marking messages keeps the batch actions on the bar.

When a reaction line appears or disappears, the reacting message grows or
shrinks upward, keeping the messages below it in place. The line card also
shows who reacted, with each author's emoji. It loads more authors on request,
shows recent senders where the complete list is unavailable, and identifies
reactions whose authors Telegram keeps hidden.
Periodic refreshes keep the loaded pages visible until their replacements
are complete, preserving how far the reader has expanded the author list.

The picker uses TDLib's [available reactions](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1get_message_available_reactions.html)
and [add reaction](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1add_message_reaction.html)
requests. The picker refreshes when Telegram changes the chat's permissions,
active emoji or message interactions; an initially empty cache keeps retrying
with a capped delay, showing **waiting for reactions…** and **retry** if it
takes longer. A temporary empty cache keeps already loaded choices. Explicit restrictions
explain why reactions are unavailable, and missing replies time out instead
of leaving the picker loading indefinitely.

Visible messages load their bodies and enable TDLib's ongoing reaction
polling while the panel is visible in the foreground; hidden panels and
background windows release their subscriptions. Failed or missing snapshots
retry without scrolling. On startup, saved messages and counts stay visible
while Telegram restores the chat. Message loads, reaction checks and the
picker wait for that chat's announcement, independently of the rest of the
chat list; signing in alone does not make a saved chat ready to open.
Reaction counts have their own durable projection;
history, media loads and viewport changes cannot overwrite them. A null
interaction update means the counts need checking, so the last known counts
stay visible until a server read confirms a change. Checks retry failures,
respect rate limits and reject replies that predate newer counts or metadata.
A fallback sweep every five minutes checks visible messages for missed push
updates. Initial loads, changed metadata, successful adds and the Line panel's
reaction author reads request checks without waiting for that sweep, keeping
the authors and message counts current together. Reconciliation is paced per
account and honors Telegram's retry delays.
Confirmed empty counts survive restarts and stale message loads.
A successful add also refreshes that message. Counts wrap at the panel width, including paid stars
and a text fallback for custom emoji. A refused request
shows **could not load reactions** or **reaction failed**, plus **retry**, and
automatically reports the reason in a notification; click the status to see it
again. Startup failures also appear in the sign-in panel. If another app
instance has locked Telegram's database, close it and restart this instance.
Offline demos offer a small fixture list and update the message's displayed counts
locally when an emoji is chosen. A live account with no connected worker reports
a connection error; it never falls back to demo reactions.
Custom emoji and paid reactions are not offered.

Count reconciliation uses `searchChatMessages` at the exact message id,
with its sender, topic, text or media type as the search criterion. Our TDLib
message database is disabled, so its [search path](https://github.com/tdlib/td/blob/master/td/telegram/MessagesManager.cpp)
goes to the server; `getMessage` and `getMessages` can still return cached
data. An absent or unsearchable result keeps the last known counts and retries;
it never proves a removal. Null counts require loaded reaction metadata and a
second matching server result within the same metadata generation.

## Builds

`cargo build -p superapp` and `cargo run -p superapp` link `libtdjson`.
The build looks under `/opt/homebrew/opt/tdlib/lib` by default; set `TDLIB_DIR`
to another installation prefix containing `lib/libtdjson.dylib` on macOS.
The same path is added to the executable's runtime library search path.

Only one running app can use a TDLib session directory. Development workspaces
share the default directory, so close the other app before restarting the
one you want to connect. Initialization failures appear in sign-in and empty
chat/topic lists; cached chats can still be present while the connection is
unavailable. These errors stay in the current process's runtime.

Tests, demos and targets without TDLib can drop the feature explicitly:

```sh
mise exec -- cargo test --workspace --no-default-features
mise exec -- cargo run -p superapp --no-default-features -- --library
MAKEPAD=headless mise exec -- cargo build -p superapp --no-default-features
```

CI uses this opt-out for Clippy, tests and the headless suites. Plain
`cargo test --workspace` also runs the TDLib FFI smoke tests; account tests
still use fake transports and do not sign in.

## Ownership and data flow

| Owner | State | Lifetime |
|---|---|---|
| Panel instance | Cursor, marks, reply, edit, attachments, player controls | Until the panel closes |
| Store runtime | Forward picker, autoplay request, loading flags, requested history and downloads, command sender | One database handle, shared by its readers and workers |
| Account worker | TDLib transport, command receiver, history pacing, typing expiry | Until the worker stops |
| SQLite projection | Peers, chats, messages, memberships, drafts, FTS index | Persistent; part of store replication |
| Device files | TDLib session, media cache, configuration, diagnostics | Local to this device |
| Keychain | Telegram api_hash | Local credential storage |

`Store::local<T>()` supplies the runtime's shared ownership. It is keyed by the
actual database handle, not a path or a chat id. Two independent stores can
contain identical Telegram ids without sharing forwards, downloads, playback
requests or loading indicators. A worker opening a reader with `Store::with_db`
joins the same runtime. Nothing in that runtime is serialized.

```text
panel -> requests -> runtime inbox -> account worker -> TDLib
                                         ^                |
                                         |                v
panel <- model <- SQLite <- project <- updates <---- TDLib JSON
```

The native account's runtime reserves its first inbox before restored panels
enqueue requests. The worker adopts that queue and its pending replies on its
first pass; message reads and read receipts then wait for authorization and
chat restoration. Panels enqueue JSON commands through `panels::wire`, including
sign-in. A successful enqueue does not mean Telegram accepted the request.
Dropping the worker disconnects the inbox, and later panel lookups cannot
reopen it or replay its commands.
Login codes and passwords never enter the persistent effects queue.

One native bridge thread owns TDLib's process-wide blocking receive queue and
routes packets by client id into bounded Tokio inboxes. Each account awaits
updates and commands, then projects up to 128 packets in receive order on the
shared blocking pool. Its SQLite reader is created inside each projection
task. Responses remain in the update stream so temporary send responses cannot
be mistaken for final delivery. Shutdown drains accepted commands, closes the
native client and keeps projecting until TDLib confirms closure.

History and missing-file requests use a separate deduplicated pending set in
the same runtime. The worker drains it on each pass and maintains its own
history pacing. Loading flags stay with the store that requested the work.

Document filenames in a transcript open the shared [file viewer](./viewers.md)
directly. The same viewer handles photos, text files, and continuously scrolling
PDF pages, with selectable text and fit and zoom in the verb bar. Opening an uncached file requests it through the account worker;
receiving a document alone does not download it. Cached files open offline.
Images and text size the panel from their contents; PDFs reserve a tall reader
and keep its size stable through scrolling. Video and audio retain playback controls.

The media viewer shows downloaded and total bytes while a clip or photo is
arriving. File replies and updates refresh these counts in the store runtime,
keyed by the media's cache reference. Estimated totals are prefixed with `~`;
an unknown total is shown as unknown. Finished and stopped downloads clear
their progress, and the note disappears once the media is available locally.

The **download** action on a chat's selected message, its line card, and the
media viewer saves an attachment to `~/Downloads`. Documents, photos, videos,
animations, video messages, voice notes and audio tracks can be saved. Documents
and named media keep the sender's filename; unnamed media gets a name based on
the chat and message. Paths, control characters and Unicode format characters
(including bidi overrides) in filenames are removed,
and an existing `report.pdf` makes the next copy `report (1).pdf`.

Saving runs on the account worker and continues after the panel closes. The
shared status strip shows progress and the saved path. It reports success only
after the copy reaches Downloads; download and disk failures offer **retry**.
Pressing **download** again after a failure retries the same operation.
Retries refresh the source message to repair expired file references, and late
answers from an earlier attempt cannot complete a newer one. Cached documents
can be saved without a network request. Documents and recordings are fetched
only on request, and the exported copy survives media cache eviction.

Downloads and history requested during startup stay queued until this client
is authorized. If initialization fails, the viewer and sign-in panel show
the connection problem, including a session held by another app instance.
Initialization retries every five seconds after a failure; queued media
starts automatically once Telegram is ready.

An uncached viewer first restores its source message in the current TDLib
session, waiting with history requests for that chat to load during startup.
A saved remote file id
alone cannot repair an expired file reference. The viewer then downloads the
fresh file at priority 32 with a completion response, while `updateFile` keeps
the byte counts current. Source errors, download failures and timeouts appear
in the viewer; the shared feedback controls offer an explicit retry.

The play button plays videos inline in the transcript or message card. It
downloads an uncached clip on demand. The preview and decoded frames share
one surface, fitted to the column within 320×480 using the message's video
dimensions. The preview remains until the first decoded frame arrives;
download feedback overlays the surface so starting playback does not move
the transcript. Clicking the video itself opens the dedicated viewer. Playback
pauses when its message leaves the viewport or the viewer opens. Starting
playback in another Telegram panel pauses the previous player, including when
a transcript and its message card remain visible together. Selecting or marking
a message keeps the same native player.

Click or drag the progress bar to seek in the transcript, message card or
viewer. Seeking preserves play/pause state, clamps at either end, and waits
for an uncached clip to download and prepare before applying the position.

Playing videos retain their original aspect ratio, both inline and inside the
viewer, including when the window is resized.

## Modules

| Module | Responsibility |
|---|---|
| `mod` | App registration and admission of the one real boot store |
| `config`, `tdjson`, `transport` | Local settings, native FFI, real and test transports |
| `requests` | Outgoing JSON and response-correlation metadata |
| `sync`, `sync/tests` | Authorization, update dispatch, history, downloads and offline protocol tests |
| `updates` | Decode incoming JSON into normalized values |
| `project` | Upserts, retention and the indexed search query |
| `schema`, `seed` | Append-only migration ladder and offline fixtures |
| `model`, `search` | Panel queries, value types, formatting and search provider |
| `runtime`, `trace` | Store-scoped coordination and local diagnostic output |
| `panels`, `verbs` | Interaction state, live commands and undoable local actions |
| `history` | User command intents, previous server state and acknowledged undo/redo |
| `widgets`, `ui`, `scenes` | Rendering, templates and library examples |

A row receives its clock explicitly. Transcript rows also receive a
`RenderContext` with their owning store's media directory. Scenes pass a fixed
clock and no live media root. The line and media cards retain their owning
world so their playback verbs can consult the correct clock even outside a
draw. Rendering does not set process-wide or thread-local model state.

Migration steps that have already shipped are immutable. Fixes belong in a
new step; the existing ladder includes repairs for earlier schema shapes and
preserves message identity as `(chat, id)` with a separate SQLite row key.

Startup restoration runs on the account worker in batches of at most 128
updates. Updates and commands wake it immediately; a 300 ms timer maintains
typing expiry, retry deadlines and settled viewports. Tasks yield between
batches. UI feedback reads operation summaries without copying request bodies,
and timeout checks only collect expired request ids. The V18 dialog view
starts from joined chats, so drawing the list does not scan unrelated peers
restored as message senders or mentions.

## Current limits

There is one live account per process. Admission still uses the real boot
store's directory. The native bridge routes by client id, but account
configuration and admission do not yet expose multiple accounts.

Live commands have store-scoped request ids and pending, completed and failed
outcomes. Sends wait for final delivery updates, and transfers display byte
progress when TDLib supplies it. Every Telegram panel shares the status strip
for command progress and connection state. Background history, topic and media
requests leave this strip unchanged; a chat's header shows loading for its whole
history walk. Background failures still appear in the strip with recovery
controls, and all failures also appear in Problems and announce a toast. Errors
go to stderr and `tg-debug.log`, with the request type/id and chat, without
command payloads or login credentials. Normal `loadChats` 404 replies mean the
list is complete.

Failed commands retain their input in memory for an explicit retry, including
file paths, captions and replies. An accepted but failed send retries TDLib's
message id. Missing delivery confirmation is marked uncertain and asks the user
to check the chat; it does not automatically resend. Edits, pinning, muting,
archiving and deletion settle through acknowledgements and updates. A disconnected
worker keeps the composer intact. Downloads finish after their bytes reach the
cache; missing files, cache errors and request timeouts are visible failures.

Drag files from the desktop onto a writable chat to stage attachments. The chat
shows a drop hint, the carried files, and a confirmation; Enter sends them.
Directories and missing paths show an error. PNG/JPEG are photos, GIF is an
animation, supported video/audio extensions use their media types, and other
formats (including HEIC/WebP) are sent as documents. Upload sources are preserved.
The media requests use the [TDLib schema](https://github.com/tdlib/td/blob/master/td/generate/scheme/td_api.tl)'s `inputPhoto`, `inputAnimation`, `inputVideo`,
`inputAudio` and `inputDocument` wrappers; the native tests probe the linked JSON
decoder so an incompatible TDLib schema fails validation.

Commands are still in memory, rather than a durable outbox. Restarting loses
retry payloads; unconfirmed projected messages remain visible as failed and ask
for a delivery check. Login secrets are never retained for retry. Recording
and location sharing report that they are unavailable in live accounts; attach
an existing recording instead. The location panel still shows its demo map.
`history` connects the main live message and chat actions to the undo tree;
`verbs` implements topic visibility preferences and offline edits, deletes and
reactions. Topic mute/pin/archive, read receipts and profile actions still use
their existing paths and are outside the live command history.

`tg_session` currently persists authorization status in the replicated store;
it is not excluded from replication. The actual TDLib session files and login
secrets are separate. Device-local status needs its own persistence policy
before multi-device authorization can be represented accurately.

The search provider and message table share an indexed, Unicode case-insensitive
substring query. `tg_message_substr` stores character n-grams of lengths 1–3 in
a contentless FTS5 index, so even the first character can use a posting list.
Longer queries intersect their trigrams and verify the literal substring only
in those candidates; punctuation and word order remain significant. Inserts,
text edits, service-line changes and deletions maintain the index transactionally.
Writers enable recursive triggers so replacement deletes also remove old grams;
stores built before that setting rebuild the substring index once.
Short queries count posting lists directly. Broad searches page in date order.
A chat filter resolves names once, then uses the posting list for selective
text or the chat index for broad matches; a bounded candidate count chooses
between them without scanning the entire chat for a rare term.
Existing stores build the index on their next open. The projection's separate
word-prefix FTS query remains available, and there is no server search fallback.
A short local history does not establish that the server has no older messages.

Media is held in the bounded blob cache. Some rendering paths resolve cache
filenames directly and perform synchronous reads, bypassing the cache's
recency update. A resolver that uses the shared cache and retains decoded
images is the next performance boundary to improve.
