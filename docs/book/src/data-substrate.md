# Data and Effects

One SQLite file stores mail and UI state that must survive a restart. This
includes open panels, their positions and joins, focus, drafts, and mail state.
The file can be opened with `sqlite3`.

## Writing data

There are three reasons to change stored data:

1. **Actions** record a user's choice and can usually be undone.
2. **Import** records facts received during sync and cannot be undone.
3. **Jobs** record work that must happen outside the database and may need a
   retry.

All three use one `write` entry point and one transaction per change.

The process has one writable database connection on its own thread. UI, sync,
and send code ask that thread to make changes and wait for the result. All other
connections are read-only, so an accidental write fails instead of conflicting
with another write.

## Effects

An effect is work whose result cannot be recreated from the database. Network
requests, keychain access, clipboard access, the clock, and file operations are
effects. A database change is not.

For example, marking a message as archived changes its desired state in SQLite.
Moving it on the IMAP server is the effect that later makes the server match.

Effects can describe themselves and run in one of three modes:

- `Real` uses the operating system and network;
- `Fake` uses isolated in-memory test data;
- `Deny` refuses every effect, which keeps component examples safe.

Each effect also states whether it changes the outside world or only reads it.
The Effects panel can filter on that distinction.

### Queued jobs

Effects that need retries are saved in the `effect` table. A worker claims a
pending row, checks that it is still needed, performs the work outside a
transaction, and records the result. Checking again prevents an old job from
running after the user has already reversed the action.

Workers claim only jobs they can run. An account's sync worker owns its IMAP
session and claims jobs for that account. The sender claims work that does not
need such a session.

Failed jobs retry with increasing delays. After six attempts they stop and wait
for the user. Each job says whether it is safe to repeat after a crash. Safe jobs
return to the queue; other jobs fail with `interrupted; outcome unknown` so a
send or other one-time action is not repeated by guesswork.

Payloads and replies are JSON text, so the queue remains readable with
`sqlite3`. Panels observe job results through the same cached-query system used
for all other database data.

### Recent in-memory effects

Effects that need an immediate answer are not queued. The process keeps the
latest 200 of them in a memory-only ring. It records the kind, owner, short
description, and any error. Clock reads are excluded because they happen many
times per frame.

The Effects panel combines the database queue and this ring with `UNION ALL`.
Memory rows use negative IDs, database rows use positive IDs, and both are
ordered by time. `@memory` and `@filed` select the two sources.

SQLite reads the ring through a registered `mem_effects()` function. Because
SQLite cannot discover this in-memory source, the Effects table source
declares it explicitly. A ring version invalidates cached pages when a worker
adds an entry.

Memory IDs are valid only for the current process. If a job panel points to a
memory row, session saving records the parent Effects panel instead. This avoids
opening an unrelated row with the same ID after restart.

### Effects and job panels

The **Effects** panel is a [rich table](./richtable.md) of queued and in-memory
effects, newest first. Each row shows its kind, owner, status, attempts when
relevant, and the description supplied by the effect. If this build cannot
decode an old job, the raw payload is shown instead.

The panel starts with `@wrote` in its visible filter field. This hides frequent
reads such as connect, search, and fetch while keeping them one edit away. Other
filters include `@read`, `@failed`, `@live`, `@retried`, `@risky`, `@memory`,
`@filed`, `@kind:`, `@entity:`, `@attempts`, and `@date`.

Opening a queued row shows a **job** panel with its description, error, times,
attempt count, retry safety, payload, and result. The panel updates when the job
changes. Memory rows have no durable job details and remain in the Effects
list.

## Device sync

SQLite's session extension records each transaction over durable tables as a
changeset in `repl_log`. The changeset and log row are written in the same
transaction.

Devices exchange snapshots and changesets through a small object-store
interface: read, create-only write, and compare-and-swap. A single `state`
object contains both the current lease and log head. Updating the head therefore
also proves that the device still owns the lease.

The first device uploads a snapshot and creates `state`. Another device installs
that snapshot, then applies later batches. The transport is chosen by URL:
local demos use `bucketd`; real sync uses Cloudflare R2 through its S3 API.
Conditional S3 writes implement create-only and compare-and-swap operations.

Device-sync settings are entered in the Settings panel. The endpoint and key ID
are stored in a `bucket` file beside the database. The secret key goes to the
platform secret store.

Only the lease holder may write. Other devices show a lock screen with the
holder and an option to take the lease. The holder releases it on sleep or
close. Taking a lease from a live device may discard changes that it has not
published, so the UI warns about that risk. A device that loses its lease while
writing becomes read-only and requires manual recovery.

## Files

The file browser uses the selected effect implementation for directory listings, metadata,
reads, opening files, creating directories, copying, moving, and trashing.
Delete always moves an item to the system trash; undo moves it back.

File changes run immediately so the UI can report their result and record undo
in the same action. A large copy can therefore block the UI; moving this work to
a background runner remains an [open question](./open-questions.md).

The real implementation refuses to overwrite an existing destination. Files and
directories are created exclusively, and macOS moves use an exclusive rename.
The copy code also refuses a destination inside its source, including paths
that only become nested after resolving links. It handles regular files,
directories, and symbolic links; other file types are refused.

If a copy fails, it removes only the incomplete destination created by that
copy. An existing destination is never removed. A move across file systems
copies first, trashes the source second, and removes the copy again if the
source cannot be trashed.

The fake implementation provides a writable demo tree for the Panels Library
and tests. The denied implementation reports that disk access is unavailable.
End-to-end tests use
`--demo-disk` so they never change the user's files.

Panels display paths with `~` and convert them to an absolute home path only at
the effect boundary. A restored panel therefore points at the current device's
home directory.

Directory listings are snapshots. The browser reloads after its own actions or
when it changes directory, but it does not yet watch changes made by other
programs.

## Cached queries and panel context

Panels read database data through registered queries such as
`store.rows(query, params)`. Results are cached by query and parameters.
SQLite's update hook records changed tables, and cached results that depend on
those tables rerun when they are next read.

SQLite records which tables a query reads while it is prepared.
Callers normally do not list dependencies by hand. The in-memory effect ring is
the exception because it is not a database table.

Each panel draw records the queries and parameters it used. `cmd+i` writes the
focused panel's identity, parameters, and query trace to the clipboard and to
`panel-context.md` beside the database.

## Mail sync

Each account has one worker thread and IMAP connection. It runs about once a
minute and on refresh. A pass discovers special-use folders, receives new mail,
and reconciles flags and deletions. Each folder keeps the newest 200 messages.

Folder roles come from IMAP special-use attributes: inbox, archive, sent, spam,
and trash. Only the first four have mailbox panels. Folders without one of these
roles are not mirrored.

`message` rows store the desired state. `server_msg` rows store the last state
seen on the server. A difference between them becomes a queued job. A sync pass
never keeps a database write transaction open during a network request.

Server deletions remove local rows. For other differences, the user's desired
state wins and is sent to the server. Undo changes the desired state again, so
the next pass can reverse a change without special compensation logic.

### Conversations and attachments

Thread membership is calculated when mail is received. References and
`In-Reply-To` headers connect messages within an account. The code also handles
a parent that arrives later and siblings that share a missing parent. It does
not use subject-line guesses.

`message.thread` stores the smallest message ID in the conversation as a stable
anchor. Mailbox rows group messages by that anchor. A reply includes the
parent's reference chain so it joins the same conversation after the Sent
folder is synced.

Attachment rows contain only metadata and a part index. The bytes remain in the
message's raw MIME data and are read on a worker when needed. Derived attachment
rows are rebuilt per device, including after replicated messages arrive.
Attachment panels use `(message, part)` rather than a local row ID, so their
identity is stable across devices.

Draft attachments are different: they store file paths selected on disk. The
bytes are read when sending, so a missing or oversized file fails with its name.
The row also records which installation selected the path; another device will
show it but refuse to send a path it cannot safely identify.

## Sending

Drafts are saved as the user types. Sending creates an outbox row with a default
10-second delay and closes the compose panel. Undo during that delay deletes the
outbox row and restores the draft.

At the deadline, the sender creates a submit job. It sends through SMTP, appends
the message to Sent over IMAP when the provider requires it, and stores the
result. Replies and forwards include the headers needed to join the Sent copy to
its conversation. Forwards also set `$Forwarded` when the server supports that
keyword.

A delivered message cannot be undone. A failed send can still be undone to
restore the draft, or retried and reopened from the Problems panel. Because the
outbox and job are durable, a restart delays pending work rather than losing it.

## Gmail sign-in

Gmail uses OAuth bearer tokens for IMAP and SMTP. Pressing **sign in with
google** starts the installed-application flow: the app listens on a temporary
local port, opens Google's consent page in the system browser, and receives the
redirect. It uses PKCE and never asks for the Google password.

| Value | Lifetime | Storage |
|---|---|---|
| Authorization code | Seconds | Only inside `oauth.rs` |
| Refresh token | Until revoked | Platform secret store |
| Access token | About one hour | Process memory |

The app checks that the granted scopes include full mail access before creating
the account. It also reads Google's XOAUTH2 error response so it can distinguish
a missing scope from disabled IMAP.

Gmail uses All Mail as the archive target. The app does not import All Mail,
because doing so would duplicate messages already mirrored from other folders.
As a result, mail archived on another device may not appear locally. Gmail also
creates its own Sent copy, so Superapp skips the usual IMAP append for Gmail.

Superapp needs the developer's Google Desktop-app registration. Set
`SUPERAPP_GOOGLE_CLIENT_ID` and `SUPERAPP_GOOGLE_CLIENT_SECRET`, or place the
downloaded configuration at `google-oauth.json` beside the database. A Web-app
registration is refused because it cannot accept the temporary loopback port.

The browser consent step is not part of end-to-end tests. OAuth URL, PKCE, token,
scope, XOAUTH2, sync, and send behavior are covered by unit tests and fake
services.

## Data kept outside SQLite

- Animation positions, active gestures, and other temporary drawing state are
  recalculated after restart.
- Undo history is in memory. Database changes made by an action remain durable,
  but the ability to undo them ends with the process.
- Passwords, OAuth refresh tokens, and object-store secret keys use the macOS
  keychain. Android currently uses a private app file until Keystore support is
  added.

## Session persistence

After a UI change, the shell compares the current workspace with the last saved
snapshot and writes it only when it changed. Startup restores the active
workspace, panels, joins, focus, filters, and other durable UI state.

A new store receives the demo mail and default layout. `--db PATH` selects a
different database. End-to-end tests use a fresh seeded temporary database by
default.

Panel parameters use typed columns such as `p_int` and `p_txt`, not serialized
blobs. This lets panel rows join directly to domain tables.
