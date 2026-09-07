# Telegram

Telegram is an app over a local SQLite projection. Panels read that projection
and own their interaction state; a background worker owns the TDLib client and
projects its updates. The `tdlib` feature enables the native client and is on
by default. Library fixtures and scripted runs stay offline with demo data
even when the native client is linked.

Conversation panels use the `telegram-chat` tag, distinct from the agent app's
`chat` tag. List rows, search results and saved messages share that identity.

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

## Builds

`cargo build -p superapp` and `cargo run -p superapp` link `libtdjson`.
The build looks under `/opt/homebrew/opt/tdlib/lib` by default; set `TDLIB_DIR`
to another installation prefix containing `lib/libtdjson.dylib` on macOS.
The same path is added to the executable's runtime library search path.

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

The worker connects its inbox on its first pass. Panels enqueue JSON commands
through one `panels::wire` function, including sign-in. Only the worker calls
TDLib. A successful enqueue means the worker can receive the command; it does
not mean Telegram accepted it. Dropping the worker disconnects the inbox.
Login codes and passwords never enter the persistent effects queue.

History and missing-file requests use a separate deduplicated pending set in
the same runtime. The worker drains it on each pass and maintains its own
history pacing. Loading flags stay with the store that requested the work.

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
| `panels`, `verbs` | Interaction state, live commands and undoable fixture actions |
| `widgets`, `ui`, `scenes` | Rendering, templates and library examples |

A row receives its clock explicitly. Transcript rows also receive a
`RenderContext` with their owning store's media directory. Scenes pass a fixed
clock and no live media root. The line and media cards retain their owning
world so their playback verbs can consult the correct clock even outside a
draw. Rendering does not set process-wide or thread-local model state.

Migration steps that have already shipped are immutable. Fixes belong in a
new step; the existing ladder includes repairs for earlier schema shapes and
preserves message identity as `(chat, id)` with a separate SQLite row key.

## Current limits

There is one live account per process. Admission still uses the real boot
store's directory because TDLib's modern receive queue is process-wide. This
is not a multi-account dispatcher.

Live commands have store-scoped request ids and pending, completed and failed
outcomes. Sends wait for final delivery updates, and transfers display byte
progress when TDLib supplies it. Every Telegram panel shares the status strip;
failures also appear in Problems and announce a toast. Errors go to stderr and
`tg-debug.log`, with the request type/id and chat, without command payloads or
login credentials. Normal `loadChats` 404 replies mean the list is complete.

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
`verbs` implements undo only for local fixture edits and deletes.

`tg_session` currently persists authorization status in the replicated store;
it is not excluded from replication. The actual TDLib session files and login
secrets are separate. Device-local status needs its own persistence policy
before multi-device authorization can be represented accurately.

The current search provider and message table use substring matching. The
projection has a tested FTS query, but the UI does not yet use it and there is
no server search fallback. A short local history does not establish that the
server has no older messages.

Media is held in the bounded blob cache. Some rendering paths resolve cache
filenames directly and perform synchronous reads, bypassing the cache's
recency update. A resolver that uses the shared cache and retains decoded
images is the next performance boundary to improve.
