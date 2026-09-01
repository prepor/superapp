# CR-004 · The world: isolation, effects, history

Status: **landed** 2026-09-01 (phases 1–3; the components library remains
the follow-up). One design covering three things that turn out
to be the same thing: an app instance you can construct, a strict boundary
around what leaves the process, and a history that operates on that
boundary. Replaces the session-changeset undo landed in CR-001 phases 2 and
6, and revisits the no-op-table decision of phase 4. The components library
is the follow-up this exists to enable, and is deliberately not specified
here.

No migration or compatibility work anywhere in this CR: the store is
personal and disposable, so schema changes drop tables outright.

## Intent

Three properties, one type.

1. **An app instance is a value, not a process.** Its store, its outside
   world, its registry, its configuration and its history are fields you
   construct — never globals, never a file path, never a thread you cannot
   see.
2. **Effects are declared, serializable data.** What leaves the process is a
   value with a one-line description and a JSON form. The ones worth
   retrying are rows in one table, executed by one machinery, with a status
   and a reply anyone can read.
3. **History is a tree of intents, in memory.** An action is a layout
   snapshot plus zero or more claims on the world. Undo reverses the claims
   it still can; the ones it cannot are transparent, not barriers.

Between them: deterministic tests that run in parallel, and a panel you can
draw in isolation against a world you wrote by hand.

## The line: what is an effect

> **An effect is anything whose result the store cannot reproduce.**

The store is the app's memory. Replaying its transactions replays the app —
which is why SQLite is emphatically *not* an effect, and why `Store::write`
is not on the effect path. A socket, the keychain, the clipboard, a file
beside the store, the screen and **the clock** are.

Two corollaries:

- **Archiving a mail is not an effect. Pushing the archive is.**
  `message.folder` is intent, a plain store write; the `Move` is what
  reaches the server.
- **The effect table is the write-side twin of the query trace.** The store
  records what a panel *read* (`Store::trace_of`), captured by the
  authorizer — "provenance is complete by construction, not by discipline"
  (store.rs:14). The `effect` table records what the app *asked of the
  world*, by the same standard.

## Effects

### The trait

Every effect is a serializable value that knows how to describe itself and
how to do itself.

```rust
pub trait Effect: Serialize + DeserializeOwned + Sized {
    /// Stable, greppable, the table's `kind`.
    const KIND: &'static str;
    /// What this call answers.
    type Reply;
    /// One line of English — the row's `detail`, the label in a status UI,
    /// and what an assertion failure prints. Never carries a secret.
    fn describe(&self) -> String;
    /// Do it.
    fn perform(&self, o: &mut dyn Outside) -> Result<Self::Reply, String>;
}
```

Serializability is a bound on the trait itself, so an effect that cannot be
written down is a compile error rather than a discovery.

### Persisted and in-memory

Two classes, distinguished by one question: **would anyone retry it, wait
for it, or want to see that it failed?**

| | examples | how it runs |
|---|---|---|
| **deferred** | `Move`, `Seen`, `Submit`, `Append` | enqueued as a row, claimed and executed by the pass, status and reply readable from the table |
| **in-memory** | `Now`, `Connect`, `Fetch`, `Uids`, `SecretGet`, `Clip`, `Shot` | performed at the call, answer returned, nothing written |

The deferred set carries two more obligations, so it gets its own trait:

```rust
/// An effect worth persisting. Its reply must survive a round trip through
/// the table, and it must say whether repeating it is safe.
pub trait Deferred: Effect
where
    Self::Reply: Serialize + DeserializeOwned,
{
    fn idempotent(&self) -> bool;
}
```

`idempotent` has no default. It is the one judgement a crash cannot guess,
so it must be made deliberately: `Seen` yes, `Move` yes (a moved uid fails
harmlessly and revalidation catches it), `Submit` **no**.

Keeping `Reply: Serialize` on `Deferred` rather than on `Effect` is what lets
`Fetch` answer `Vec<RemoteMail>` — raw bytes and all — without paying to
encode it.

The clock is an ordinary in-memory effect (`Now`, `Reply = f64`). That
deletes a separate `Clock` type: time is a verb on the backend, so a fake
world controls it exactly the way it controls everything else.

### The registry

Handlers register themselves per domain at startup; the executor decodes a
row by looking up its kind.

```rust
pub struct Registry { /* kind → decode-and-perform */ }

impl Registry {
    pub fn register<E: Deferred>(&mut self)
    where E::Reply: Serialize + DeserializeOwned;
}

// mail owns its own effects
reg.register::<mail::Move>();
reg.register::<mail::Seen>();
reg.register::<mail::Submit>();
```

Open set: a new domain adds its effects without touching a central enum.
The cost is that a forgotten registration is a runtime rather than a compile
failure, so the executor must make it loud — an unknown kind marks the row
`failed` with *"no handler for kind X"* rather than leaving it `pending`
forever. A job that never runs and never complains is the one failure mode
this design must not have.

### `Outside` — the backend

Object-safe, one typed method per verb. Three implementations ship in the
binary, not behind `#[cfg(test)]` — the components library must run from a
normal `cargo run`, and sync.rs already anticipated it ("tests +
`--fake-mail`", sync.rs:631):

| | what it does |
|---|---|
| `Real` | today's code, moved: an `imap` session per account, `lettre`, `/usr/bin/security`, `pbcopy`, `mac::screenshot`, the system clock |
| `Fake` | an in-memory world: `FakeTransport`'s folder map generalized per account, a keychain `HashMap`, captured clipboard, files and shots, and a clock the test moves |
| `Deny` | every verb is an error. The default for a components-library mount: a panel that quietly sends mail while you look at it fails loudly |

Connecting is itself an effect (`Connect { account, host, user }`), so
session churn is visible and credentials stay out of every other effect.
Credentials ride a `Creds` type whose `Debug` redacts, and `describe` prints
host and user only — a password must never reach the table.

### The table (schema v6)

```sql
CREATE TABLE effect(
  id         INTEGER PRIMARY KEY,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL CHECK (json_valid(payload)),   -- json: the effect
  entity     TEXT,                             -- 'panel:7' | 'account:2'
  status     TEXT NOT NULL DEFAULT 'pending',  -- pending|processing|done|failed|obsolete
  reply      TEXT CHECK (reply IS NULL OR json_valid(reply)),
  error      TEXT,
  attempts   INTEGER NOT NULL DEFAULT 0,
  not_before REAL NOT NULL DEFAULT 0,          -- backoff, or a scheduled time
  created    REAL NOT NULL,
  updated    REAL NOT NULL
);
CREATE INDEX idx_effect_due    ON effect(status, not_before);
CREATE INDEX idx_effect_entity ON effect(entity);
```

**JSON as TEXT, not JSONB.** SQLite has no JSON type — five storage classes
and a set of functions over TEXT. JSONB (3.45+, and the bundled 3.50 has it)
is a BLOB encoding: faster to extract from, and unreadable in a `sqlite3`
shell without wrapping every read in `json()`. Inspectability is the point
of putting this in the store at all, so TEXT wins a parse cost nobody will
measure at personal-mail scale. `json_valid()` CHECKs make malformed JSON
fail at the write rather than inside a handler three passes later.

**Columns for what you filter on routinely; JSON for the rest.** `kind`,
`status`, `entity`, `attempts`, `not_before` are columns — otherwise this
contradicts CR-001's own deviation ("panel params are typed columns, not
JSON — queryable and join-able, which is the point of the whole exercise").
But the payload is not *opaque*: `->>` reads into it, and an expression
index can be added later without a migration if some dig becomes routine.

```sql
SELECT id, payload ->> 'uid' FROM effect WHERE kind='move' AND status='failed';
CREATE INDEX idx_effect_acct ON effect(payload ->> 'account');   -- if it ever matters
```

`entity` uses the existing `action`-column convention — `panel:7`,
`account:2` — rather than a new panel identity. Panel ids are already stable
and persisted, and `draft`/`outbox` already key off them, so linking effects
to panels needs no churn. (A uuid panel identity would be a multi-device
sync decision, and belongs in that CR, not this one.)

### The reply is a channel

A caller reserves an id, writes it into whatever domain row it likes, and
reads the answer back out of the table later:

```rust
let eid = w.effect_id();                       // reserved up front
w.store().write(|tx| {
    mail::file_send_tx(tx, panel, eid)?;       // outbox row references it
    w.enqueue_in(tx, eid, mail::Submit { outbox: panel })?;
    Ok(())
})?;
```

Because enqueueing is a plain store write it composes into the caller's
transaction — the domain row and the job land together or not at all.

Reading the reply is **not polling**. A panel's `SELECT status, reply FROM
effect WHERE id=?1` goes through the reactive query layer, whose generation
clock bumps when the row commits, so the panel re-renders on its own. That
is what makes a table a good request/response channel here and would not be
true in an architecture without it.

One boundary: the reply serves **the requester**; anything *other* queries
need still lands in domain tables. `Move`'s handler writes the new uid into
`server_msg` in the same transaction as marking the job `done`, **and**
returns it as the reply. Not a conflict — different readers.

### Two rules about payloads

**Reference rows, do not embed content.** `Submit { outbox: 9 }`, not
to/subject/body; `Append { message: 12 }`, not raw bytes. Embedded payloads
go stale exactly when revalidation matters, and blobs are how job tables get
big.

**Effects carry no timing.** No delay, no deadline, no retry policy.
`Submit` is "hand this mail to that server", nothing more — scheduling lives
in `not_before`, reversibility in the status guard, retry in the executor.
Send already has this shape: `file_send_tx` writes `send_after = now + delay`
(mail.rs:383) and the pass takes it when due (send.rs:102). If an effect
wants to know *when* or *whether to try again*, that knowledge is in the
wrong place.

### The executor

One pass, one machinery, for every deferred effect:

```text
claim      UPDATE effect SET status='processing', attempts=attempts+1
             WHERE id=?1 AND status='pending'        ← one winner, always
decode     registry[kind](payload)                    ← unknown ⇒ failed, loudly
revalidate handler still wants it?                    ← else obsolete
perform    outside the store, no transaction open
close      status='done', reply=…   + whatever the success establishes
     or    status='failed', error=…, not_before = now + backoff
```

The claim guard is what makes undo and execution race safely — it is the
same `WHERE status='pending'` the sender already uses (send.rs:126).

**The crash sweep.** At boot, rows left `processing` are the ones whose
outcome nobody knows:

- **idempotent** → back to `pending`. Safe to retry; at-least-once.
- **not idempotent** → `failed`, *"interrupted; outcome unknown"*. A human
  decides. This is the whole reason `idempotent` has no default.

### Who enqueues, and why it matters

Not everything should be enqueued by the action that caused it.

- **Convergent** work — folder membership, read flags — is enqueued **by the
  pass, from the diff**. `message` is intent, `server_msg` is fact, and the
  pass materializes each disagreement as a job. Archive → undo before the
  pass runs means the diff never diverged, so **nothing is ever enqueued and
  the server hears nothing**. Offline just waits. That is phase 4's win and
  it survives intact.
- **Imperative** work — send, and the one-shot verbs that follow it — is
  enqueued **by the action**, because there is no diff to derive from. The
  claimed row is the intent.

Both land in the same table with the same machinery, the same statuses and
the same observability. The only difference is who writes the row.

**Revalidation** is the safety net for the convergent half: before
performing, the handler re-checks that the diff still asks for this. If undo
landed while the job sat in the queue, the job goes `obsolete` instead of
executing stale work. Undo also cancels pending jobs directly (cheap and
immediate) — revalidation covers the case where it could not.

## History

An in-memory tree on the `World`, dying with the process. Not persisted, not
in `sqlite3`, not restored at boot.

```rust
pub struct History {
    nodes: HashMap<NodeId, Node>,
    root:  NodeId,          // moves forward as the tree is trimmed
    head:  NodeId,          // the cursor
}

pub struct Node {
    pub parent:  Option<NodeId>,
    pub kind:    &'static str,     // "open" | "move" | "archive" | "send" | …
    pub label:   String,           // "archive “Q3 infra budget draft”"
    pub entity:  Option<String>,   // "panel:7" — coalescing scope
    pub ts:      f64,
    /// The layout as it was. Undo restores it and writes through.
    pub before:  core::WmSnap,
    /// What this action claimed of the world, if anything.
    pub intents: Vec<Box<dyn Intent>>,
    pub state:   State,            // Applied | Undone
}
```

`before` answers "most actions have no effects at all". Open, move, column,
close — the frequent ones — are pure layout changes, and `core::WmSnap`
already exists, is small, is pure, and already round-trips (`Wm::restore` /
`save_wm_tx`, proven by store.rs's `wm_round_trips_through_the_store`).
Undoing a navigation becomes restoring a typed value you can print.

### `Intent` — the claim

Object-safe, because a node holds a heterogeneous list. Purely in memory —
**never serialized.** What survives a restart is the row an intent wrote,
and that row is ordinary data.

```rust
pub trait Intent {
    fn describe(&self) -> String;
    /// Give it back if the world still permits. This is where "sent is
    /// reversible for N seconds" lives.
    fn reverse(&self, w: &World) -> Reversal;
    /// Claim it again (redo). A no-op if it was never reversed.
    fn reapply(&self, w: &World) -> Result<(), String>;
}

pub enum Reversal {
    Done,
    /// Physics said no — the node becomes transparent, not a barrier.
    Impossible(String),
}
```

`Archive { mail }` reverses by putting `message.folder` back and letting the
push pass re-converge. `Send { panel }` reverses by cancelling the job
`WHERE status='pending'` and deleting the outbox row — and answers
`Impossible("already sent")` when the guard finds nothing, because the
executor won the race.

This retires `Store::set_undo_guard` and the
`Box<dyn Fn(&Connection, &str, Option<&str>) -> bool>` the store currently
holds so the shell can inject domain judgement (store.rs:90). The judgement
moves to the intent that owns it.

### The walk

- **Apply** — mutate `Wm`, write through, push a leaf under `head`, move
  `head` onto it. A same-kind same-entity action within 2.5 s amends the head
  node instead (keeping the *earlier* `before`), so a burst of drags is one
  step. Acting mid-tree creates a sibling; nothing is lost.
- **Undo** — reverse the intents, restore `before`, write through, mark
  `Undone`, move `head` to the parent. A node whose intents all answer
  `Impossible` is **transparent**: the walk marks it and continues, because
  blocking all history behind one sent mail is wrong and silently pretending
  to undo it is a lie (CR-001's rule, kept).
- **Redo** — the most recent `Undone` child of `head`.
- **Travel** — undo to the lowest common ancestor, redo down the target's
  branch. What `cmd+u` walks.
- **Trim** — bounded at `HISTORY_KEEP`. The oldest survivor becomes the new
  `root` and its `before` is the floor.

### What this deletes

`Store::act`, `undo`, `redo`, `travel`, `set_undo_guard`, `history`,
`ActionNode`, `ACTION_TABLES`, `COALESCE_S`, schema v2's `action` table and
the `head` key in `meta`, and the `session` feature from `rusqlite` (with
it, the `buildtime_bindgen` the android cross-build has to carry). The `wm`
table stays — current state, not history.

## `World`

```rust
pub struct World {
    store:    Rc<Store>,
    outside:  RefCell<Box<dyn Outside>>,
    registry: Registry,
    history:  RefCell<History>,
    cfg:      Config,
}

let t: f64        = w.run(Now)?;                 // in-memory, answer now
let id: EffectId  = w.enqueue(Move { … })?;      // deferred; status + reply in the row
```

`run` performs and returns. `enqueue` writes a row and returns its id;
`enqueue_in(tx, …)` composes into a caller's transaction. Neither may be
called with a transaction open around the actual round trip — the executor
owns the transactions on both sides of `perform`, which is what stops a
write lock being held across the network. `fetch_account` does exactly that
today: `BEGIN IMMEDIATE` at sync.rs:180, then a fetch (:221) and two
searches (:237-238) inside it.

## The pump: passes, not threads

The engine passes are already the right shape. What changes is who calls
them.

```rust
pub enum Pump {
    /// Production: one thread per account plus the executor, as today.
    Threads(Vec<sync::Worker>, Option<send::Sender>),
    /// Tests and the components library: passes run inline, on demand.
    Manual,
}

impl World {
    /// One round: each account's sync pass, then one executor pass.
    pub fn tick(&self);
    /// Rounds until the queue is empty and nothing changes (bounded).
    pub fn settle(&self);
}
```

Under `Manual` the passes run on the calling thread against the world's own
store, so an in-memory store finally has a mail engine — today
`spawn_workers` bails outright without a file path (app.rs:1051), so an
in-memory app has no engine at all, which is exactly the app a test wants.
Under `Threads`, production is what it is today: same loop, same poll, same
kick channel; each thread builds its own `World` inside the closure, exactly
as it builds its own `Connection` now.

Ingest also stops hand-rolling `BEGIN IMMEDIATE`/`COMMIT` as SQL strings and
commits through `Store::write`, so it bumps the generation clock. Today that
is masked by workers living on other connections (`poll_external` catches
them wholesale); the moment a pass runs on the UI's own connection — which
is what `Manual` does — it stops being masked.

## Configuration

`Config` moves off the `OnceLock` (app.rs:71) and onto `World`. `config()`
dies; its ten readers take `&World`. `run()` parses argv once and hands the
result to the world it builds. `--db`, `--grid`, `--send-delay` behave
identically — they just stop being process-wide, which is what lets two
tests in one process disagree about them.

## What a test looks like

```rust
#[test]
fn archive_pushes_and_undo_converges_back() {
    let w = World::fake();                 // in-memory store, Fake outside, fixed clock
    w.seed(mail::demo());
    w.outside(|o| o.server(1).deliver("INBOX", true, RAW));
    w.settle();

    w.act(Archive { mail: 1 });
    w.settle();
    assert_eq!(w.deeds(), [("move", "done", "move uid 1 from INBOX to Archive")]);

    w.undo();
    w.settle();
    assert_eq!(w.deeds().last().unwrap().2, "move uid 1 from Archive to INBOX");

    // Undone before the pass ran, the server never hears about it at all.
    w.act(Archive { mail: 2 });
    w.undo();
    w.settle();
    assert!(w.deeds_since(mark).is_empty());
}
```

No temp directory, no PID, no keychain, no sleep, no thread — nothing
outside this `World`. Any number run in parallel because they share nothing.
Isolation is **per test and in memory**; the suite touches no filesystem,
which retires the `temp_dir()/superapp-send-{pid}` directory that every test
in a process currently shares and each one deletes on the way out.

## What this deliberately does not isolate

- **Window and platform setup.** `mac::visible_frame`, `activate`,
  `configure_macos_window` are not effects — they happen once at startup,
  above the isolation line. An isolated world has no window, so the answer is
  "the shell does not run", not "its window calls are stubbed".
  `mac::screenshot` *is* an effect, because e2e asserts on it.
- **argv, `HOME`, the PID.** Configuration, not effects. The fix is deleting
  the global, not wrapping it.
- **Makepad.** Panels draw against `PanelProps` (panels.rs:20) and emit
  `PanelAction`s; that seam already exists and is what the components library
  mounts into. `PanelProps` gains a `World` in place of its bare `Store`.

## Costs, named honestly

- **Undo dies with the process.** In-flight work **completes but stops being
  undoable**: crash three seconds after hitting send, reopen, and the mail
  goes out with no `cmd+z` to stop it. A ten-second window, so nearly
  theoretical — less theoretical is that yesterday's archive cannot be undone
  today. Accepted deliberately.
- **Per-action inverses replace zero-code undo.** The session extension made
  navigation undoable with no per-action code. `before: WmSnap` covers layout
  for free, but data mutations owe an `Intent` each — six or so, three lines
  apiece.
- **Undo can clobber where it used to skip.** Inverted changesets
  OMIT-skipped rows the world changed since (store.rs's
  `undo_skips_rows_changed_since`). An explicit inverse is a plain `UPDATE`
  and will win instead. Guarding is per-intent discipline now.
- **A forgotten `register` is a runtime failure.** The price of an open set.
  Mitigated by the loud unknown-kind path and a test that every kind
  round-trips.
- **A new dependency.** `serde_json` (serde is already in the graph
  transitively). Small, but this project has kept its dep list deliberate.
- **`sync.rs` gets restructured.** Gather-then-commit is a genuine rewrite of
  `fetch_account`, and it is where the risk in this CR lives.
- **A job table is a job table.** Attempts, backoff, obsolescence and a crash
  sweep are machinery the derived-diff model did not need. Bought
  deliberately, for observability.
- **The fakes ship.** `Fake` and `Deny` are in the release binary — a fake
  mail server compiled into a mail client.
- **Ten `config()` call sites become parameters.** Tedious, low-risk.

## Phases

Each lands green (unit + all e2e suites), book updated, committed.

1. **Effects.** `src/effect.rs`: `Effect`, `Deferred`, `Registry`,
   `Outside`, `Real`/`Fake`/`Deny`, `World::run`/`enqueue`; schema v6 and the
   crash sweep. `sync::Transport` and `send::Mailer` retire into it;
   `FakeTransport`/`FakeMailer` become `Fake`. Engine passes take `&World`
   and become gather-then-commit. *Visible win: `sqlite3` shows every job the
   app has, with its status, its reply and its failures.*
2. **History.** `src/history.rs`: the tree, `Intent`, `Reversal`, the walk,
   trimming. The `action` table and the `session` feature go. `cmd+z`,
   `cmd+shift+z` and the `cmd+u` overlay read the in-memory tree.
3. **The pump.** `Pump::{Threads, Manual}`, `World::tick`/`settle`;
   `spawn_workers` becomes pump construction. In-memory stores get a mail
   engine. Production behaviour unchanged.
4. **Config off the global.** `Config` onto `World`; `config()` deleted.
5. **The harness and the port.** `World::fake()`, `seed`, `act`, `undo`,
   `deeds_since`, and the clock a test moves; the existing suites port; the
   shared temp dir goes. *Visible win: the suite is deterministic, parallel,
   and touches no filesystem.*
6. **(Follow-up, not this CR) The components library.** `PanelProps` carries
   the world; a gallery mounting one panel widget per state against `Deny`
   plus a hand-written store. This CR's job is to leave it a one-evening
   task.

## What was built, where it differs

- **`Intent::blocked` + `reverse`**, not `reverse -> Reversal` (above).
- **`History::apply` takes an `Action` struct.** Seven positional arguments
  was one too many, and the struct reads better at the one call site.
- **`idempotent` is a column**, so the sweep is SQL (above).
- **`Pump` lives in `sync.rs`**, next to the passes it schedules, rather than
  in its own module.
- **`config()` stayed.** The phase-4 argument was that a process-global
  config stops two tests disagreeing about `--send-delay` — but no test
  reaches it. Tests construct a `World` directly; `config()` is read only by
  the makepad shell, which a test never instantiates. Moving it would have
  been indirection with no consumer, so it stays the shell's argv parser and
  moves when the components library actually needs per-mount config.
- **Send keeps its `outbox` row.** The row holds the schedule and the claim
  guard; the pass enqueues `Submit` when one is due. Collapsing `outbox`
  into `draft` was considered and left alone — the win is one join, and the
  split keeps per-keystroke draft writes off the row a background thread
  claims.
- **Removing an account is irreversible**, and says so. The changeset undo
  restored its rows; an `Intent` cannot restore its mail, so
  `AccountRemoved::blocked` refuses and the walk steps past. More honest
  than half-restoring.

## Decisions taken

- **Effects are serializable values behind a trait**, with a runtime
  registry per domain — not a central enum.
- **Two classes**: deferred effects are rows in one table with one
  machinery; in-memory effects are performed at the call and written
  nowhere. The clock is an ordinary in-memory effect, so there is no `Clock`
  type.
- **The reply is a channel**, read through the reactive query layer. Domain
  state still lands in domain tables for every other reader.
- **`idempotent` has no default**, and drives the crash sweep.
- **Convergent work is enqueued by the pass from the diff**; imperative work
  by the action. Phase 4's zero-traffic, offline-correct undo survives.
- **History is in memory, per `World`, bounded** — it dies with the process.
- **Panel identity stays an integer**; effects link through the existing
  `entity` convention. A uuid identity belongs to a sync CR.
- **Isolation is per test and in memory** — no PID-scoped temp directories,
  no filesystem in the suite.

## Still open

1. **`HISTORY_KEEP` and effect retention.** How many nodes the tree holds,
   and whether `done` rows are pruned by age or by count. Proposed 200 nodes
   and pruning `done` past 2000 rows; `failed` rows are never pruned
   silently.
2. **Backoff policy.** Fixed, exponential, capped, and how many attempts
   before a job stops retrying and waits for a human.
3. **Coalescing under the new node shape.** Amending keeps the earlier
   `before`; do the amended node's intents replace or append? Proposed:
   replace for same-entity same-kind, since the later claim subsumes.
4. **`Deny` or `Fake` as the components-library default.** Proposed `Deny`
   by default, `Fake` opt-in per mount.
