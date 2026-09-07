# Telegram

Telegram is an app over a local SQLite projection. Panels read that projection
and own their interaction state; a background worker owns the TDLib client and
projects its updates. The `tdlib` feature enables the native client. Ordinary
builds and library fixtures work offline with the demo data.

Conversation panels use the `telegram-chat` tag, distinct from the agent app's
`chat` tag. List rows, search results and saved messages share that identity.

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

Live commands are in-memory requests followed by server updates. Their error
handling, persistence and undo semantics are not yet the kernel's deferred
effect model. `verbs` implements undo only for local fixture edits and deletes.

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
