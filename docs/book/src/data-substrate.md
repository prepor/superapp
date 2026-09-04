# Data and Effects

One SQLite file stores the UI state that must survive a restart and every
app's own rows. The file can be opened with `sqlite3`.

The kernel owns `meta`, `workspace`, `ws_col`, `panel`, `wm`, `effect`, and the
two `repl` tables, and nothing else. Every other table belongs to an app and
arrives through that app's [schema ladder](#schema-ladders).

## Writing data

There are three reasons to change stored data:

1. **Actions** record a user's choice and can usually be undone.
2. **Import** records facts received during sync and cannot be undone.
3. **Jobs** record work that must happen outside the database and may need a
   retry.

All three use one `Store::write` entry point and one transaction per change.

The process has one writable database connection on its own thread. The UI and
every worker ask that thread to make changes and wait for the result. All other
connections are read-only, so an accidental write fails instead of conflicting
with another write. A test in the kernel keeps `Connection::open` inside
`store.rs`, so no code can quietly open a second writable handle.

An undoable change is a `Session::act`: it mutates the layout, writes the
session and the action's data in one transaction, records a history node with
the layout before and after plus the intents that reverse it, then kicks the
workers and replication. See [Apps](./apps.md#the-session).

### The changeset's inverse

An intent is normally an app's own sentence about its own rows — *mail:7
archived*, *renamed “a” to “b”* — because the app knows what it did. One
caller cannot say that: the [agent](./agents.md#the-kernels-own)'s `sql.write`
ran a statement somebody wrote, and all it knows is which rows moved.

So it claims the transaction's **changeset**, the very bytes the session
extension recorded for device sync. Undo applies the inverse and redo applies
the original, both through the one writer, so the reversal replicates to the
other device and invalidates the queries that drew the rows. Before it applies
anything it rehearses in a transaction that is always rolled back: a row that
has since changed is replaced, a row that has gone is skipped, and anything
else — a constraint, a foreign key — is refused, so a node that could not be
undone cleanly expires rather than half-applying. A table with no primary key
records nothing in a changeset, so writing one is refused outright instead of
promising an undo that would do nothing.

## Effects

An effect is work whose result cannot be recreated from the database. Network
requests, keychain access, clipboard access, the clock, and file operations are
effects. A database change is not.

For example, marking a message as archived changes its desired state in SQLite.
Moving it on the IMAP server is the effect that later makes the server match.

Effects describe themselves, and each states whether it changes the outside
world or only reads it. The Effects panel can filter on that distinction.

An effect reaches the outside through a **capability**: a trait the world holds
one implementation of. `Ctx::cap::<dyn Disk>()` is how an effect asks, and a
missing capability is the error, in words. A world is built in one of three
modes:

- `Real` uses the operating system and network;
- `Fake` uses isolated in-memory implementations;
- `Deny` gives nothing but the clock, which is what a panels-library mount
  gets, so an effect it files fails out loud instead of quietly working.

The kernel defines `Clock`, `Secrets`, `Clipboard`, `Screen`, `Disk`, and the
`Watcher` over it, because the harness, attachments, and a file browser all use
them. An app defines its own and supplies them in `App::outside`; mail's are
`Imap`, `Smtp`, and `OAuth`, and the agent's is `Gateway`. The kernel installs
its own first, so an app or the shell may replace one: `app/src/shell/boot.rs`
puts the real screen, the real clipboard, this machine's disk, and a watcher
over it in place of the fakes on a
windowed run.

### Queued jobs

Effects that need retries are saved in the `effect` table. A worker claims a
pending row, checks that it is still needed, performs the work outside a
transaction, and records the result. Checking again prevents an old job from
running after the user has already reversed the action.

Workers claim only jobs they can run. An account's sync worker owns its IMAP
session and claims jobs for that account. The sender claims work that does not
need such a session.

Failed jobs retry with increasing delays. After `MAX_ATTEMPTS` (six) they stop
and wait for the user. Each job says whether it is safe to repeat after a
crash. Safe jobs return to the queue; other jobs fail with
`interrupted; outcome unknown`, so a send or other one-time action is not
repeated by guesswork.

Payloads and replies are JSON text, so the queue stays readable with `sqlite3`.
Panels observe job results through the same cached-query system used for all
other database data.

### Recent in-memory effects

Effects that need an immediate answer are not queued. The process keeps the
latest `KEPT` (200) of them in a memory-only ring. It records the kind, owner,
short description, and any error. Clock reads are excluded because they happen
many times per frame.

An [agent](./agents.md)'s request to its gateway is one of these and never a
row: a request costs money, nobody would retry one blindly, and the run's own
row is its state, so there is nothing for the queue to claim. It goes through
the one door all the same, so the log shows *ask the model for chat 7, turn
12* with its error beside the mail reads, and it says it wrote, because a
request costs something.

The Effects panel combines the database queue and this ring with `UNION ALL`.
Memory rows use negative IDs, database rows use positive IDs, and both are
ordered by time. `@memory` and `@filed` select the two sources.

SQLite reads the ring through a registered `mem_effects()` function. Because
SQLite cannot discover this in-memory source, the Effects table source declares
it explicitly as a dependency. A ring version invalidates cached pages when a
worker adds an entry.

Memory IDs are valid only for the current process. A job panel on a memory row
saves as the parent Effects panel instead, through `Panel::persist`. This
avoids opening an unrelated row with the same ID after restart.

### The live tail

The ring's rule — what is transient and needs no restart lives in memory, not
in a row — has one more instance. While a model is writing an answer, what has
arrived of it is kept on the agent app's own static, per run, with a counter a
widget can compare instead of comparing strings. A token a row would be a
thousand writes a turn. The chat draws it under the last turn, and the engine
asks for a frame after every chunk; when the answer is whole it becomes one
`agent_turn` row and the tail is dropped. A run the person stopped keeps what
had arrived, as a turn of its own.

### Effects and job panels

The **Effects** panel is a [rich table](./richtable.md) of queued and in-memory
effects, newest first. Each row shows its kind, owner, status, attempts when
relevant, and the description supplied by the effect. If this build cannot
decode an old job, the raw payload is shown instead.

The panel starts with `@wrote` in its visible filter field. This hides frequent
reads such as connect, search, and fetch while keeping them one edit away.
Other filters include `@read`, `@failed`, `@live`, `@retried`, `@risky`,
`@memory`, `@filed`, `@kind:`, `@entity:`, `@attempts`, and `@date`.

Opening a queued row previews a **job** panel with its description, error,
times, attempt count, retry safety, payload, and result. The panel updates when
the job changes. Everything below the subject is selectable, because a payload
is something one copies into a report. An in-memory effect has no payload and
no reply, and those sections are absent rather than empty.

## Workers

A worker is one background pass with its own thread and its own world: its own
store reader and its own real capabilities. `App::workers(store)` says which
the app wants running now, derived from the store. The kernel asks at boot and
again after every action, at the moment the workers are kicked, and diffs the
answer by name, so a pass that a new row calls for starts without a restart.

A pass answers `Wake::After(d)` or `Wake::OnKick`. `Session::workers().kick(entity)`
wakes one by the address it answers to; `kick_all()` wakes everyone and re-asks
the apps for the set. A pass may wake another itself, through the `Kicker`
capability — how one that learns something another one owns hands it over
instead of doing the work on the wrong thread. Under virtual time there are no
threads: every pass runs inline from the frame loop, and the queue is then
drained until it stops moving, bounded, so a job that files another job shows
as a backlog rather than a hang.

## Schema ladders

Each app supplies a `Schema`: a list of steps and the key
`meta['schema:<app>']` that records how many have run. Ladders are applied at
every store open, the kernel's first, then the apps' in app-list order.

`Step::Sql` and `Step::Run` are applied once, in order. `Step::Derived` is data
rebuilt from other rows (a search index, an HTML narrowing, attachment rows)
and is versioned by the walk that made it rather than by the ladder's counter:
bump the version and every store rebuilds on its next open, however old it is.
`Step::Always` runs at every open, in its place in the ladder, which is where a
crash is put right before any worker is asked for.

An app never alters another app's tables, and a new app prefixes its table
names with its id. The kernel's schema is at version 1 and a store of any other
shape is refused in one line; there is no migration from an older design.

## Cached queries and panel context

Panels read database data through registered queries such as
`store.rows(query, params)`. Results are cached by query and parameters.
SQLite's authorizer records which tables a query reads while it is prepared,
and its update hook records which tables a commit changed; cached results that
depend on a changed table rerun when they are next read. `rows_sql` is the same
for a query whose text is built at run time, such as a rich table's page.

Callers normally do not list dependencies by hand. The in-memory effect ring is
the one exception, because it is not a database table, and `rows_sql_deps` is
where it says so.

Each panel draw opens a trace, so the queries and parameters it used are
recorded by construction rather than declared. `cmd+i` copies the focused
panel's identity, arguments, and query trace to the clipboard. The copy is an
effect, so a world that may not touch a human's clipboard refuses it out loud.

The trace is also what an [agent](./agents.md#the-chip) is handed: the panel's
own paragraph, then each traced query with the rows re-read now, off the same
parameters the draw bound. The copy's first line is the panel's identity, which
is what makes it reversible — pasted into a chat it is read back as the panel
it came from.

## Data kept outside SQLite

- Animation positions, active gestures, and other temporary drawing state are
  recalculated after restart.
- Undo history is in memory. Database changes made by an action remain durable,
  but the ability to undo them ends with the process.
- Passwords, OAuth refresh tokens, and object-store secret keys use the macOS
  login keychain, and a mode-0600 file inside the app directory elsewhere. A
  secret never goes in the store: it is the one thing that must not replicate.
  Only a `Real` world gets the platform store; a scripted run keeps the
  kernel's shared in-memory one, so a suite never writes to a human's keychain.

## Session persistence

After a UI change, the shell compares the current workspace with the last saved
snapshot and writes it only when it changed. Startup restores the active
workspace, panels, joins, focus, filters, and other durable UI state. A slot
that closed and one that opened are settled after the event, never inside the
action that moved the layout.

A slot is stored as `panel(kind, args)`: the tag in one text column, and the
arguments as one JSON array in another. The column carries a `json_valid`
check, so it is readable in `sqlite3` and reachable with `args ->> 0` should a
query ever want one. The kernel never reads an argument; their meaning is the
owning app's. What a slot saves as is its instance's `Panel::persist`, which is
usually its identity.

A row whose tag no app in this build owns comes back all the same: the session
opens a `Missing` panel for it and saves it again unchanged, because another
build has the app and the session is shared. An empty but booted store restores
as genuinely empty; closing everything is a state, not an accident.

A new store receives every app's demo rows on its first open, and comes up on
the first root the app list offers. `--db PATH` selects a different database.
End-to-end tests use a fresh seeded temporary database by default.
