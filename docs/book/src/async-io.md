# Async I/O experiment

The purpose of this branch is responsive input under slow network, disk and
SQLite work. Tokio coordinates waiting; native work still needs an explicit
blocking boundary. Adding `async` to a function does not provide one.

## Ownership

- The UI owns panels, layout, selection and in-memory completions.
- Tokio services own network connections, protocol state, timers and queues.
  Worker discovery itself runs off the UI.
- SQLite has one serial writer. Transactions, undo capture and replication
  bookkeeping stay together; replies are asynchronous. Read snapshots use a
  bounded blocking pool and read-only connections.
- TDLib has one process-wide blocking receive loop, routing bounded per-client
  queues. Requests keep correlation IDs and ordered projection. Shutdown drains
  accepted commands, sends `close`, and waits for the native acknowledgement.
- Filesystem work, keychain access, document conversion, image decoding and
  database-backed context rendering use blocking tasks. Native watchers retain
  their platform event loops.

Mail conversation preparation, Calendar availability calculations and Telegram
media-path inspection also run outside drawing. Parsed display values carry a
revision; stale completions cannot replace newer input. Each reader owns its
pending HTML parse, with at most two parsing tasks running at once. Widgets
install completed documents through the shared reader adapter. Autocomplete
observes both commits and completed snapshots, so an offer can arrive without
another keystroke while preserving its dismissal and highlighted value.

Calendar snapshots distinguish the controls identifying a view from the data
revision behind it. Refreshing data retains the last complete display, including
the track widget holding an active drag. Changing the search controls starts a
new reading, so candidates for an old duration or date cannot appear under the
new controls. Editor refreshes also retain accepted local edits until a fresh
snapshot arrives; authoritative undo results may restore an earlier revision.

Availability tracks prepare tooltip text and a time index in that same background
load. Pointer movement searches the index and reuses the prepared text, with no
per-position snapshot or database request. Event boundaries switch details
immediately; free/busy coverage still decides where a tooltip can appear.

Owned capability operations release the UI world's capability borrow before
awaiting completion. Clipboard copies, for example, keep accepted order and
report the process's actual result while the UI remains free to read its clock
or use another capability. Session shutdown drains their completions too.

Readonly completions also mark display readiness before waking the platform.
Repeated notifications coalesce until the UI consumes them; they do not expire
table snapshots or keep the window drawing after the work finishes.

List reconciliation distinguishes pending lookups from confirmed missing rows.
Marks survive a cold lookup or refresh, including a previously empty snapshot
whose dependencies changed. Mark-all waits for its own query to complete and
is canceled by a subsequent clear, filter change, or manual marking gesture.

Filing or deleting list rows resolves the successor in the mutation's own
transaction and returns a small cursor result. Retained display rows cannot
choose which conversation opens next. The list accepts that result only if its
cursor and filter still belong to the initiating gesture. UI consequences of
one edit finish before the next ready edit records its history node, so preview
movement and undo stay attached to the same action.

`Edit<R>` separates a data transaction from the UI consequence of its committed
result. Preparing a tool may read or validate off the UI; it cannot perform the
mutation. SQLite tool edits record their result in the same transaction as the
change, and recheck that their call is still active. Native filesystem commands
have an explicit acceptance/execution boundary: once accepted, they finish and
retain compensation/undo even if their agent subsequently stops. Filesystem
writes and SQLite bookkeeping cannot share one atomic transaction.

History transitions move the owned data claims to a blocking worker. UI-only
claims restore panel marks on completion. Later navigation and edits queue
behind the transition; they cannot mutate its tree concurrently. Shutdown
flushes drafts and accepted UI completions, joins active and retired services,
then releases the replication lease.

Native quit requests begin this lifecycle before the window event loop stops.
Accepted UI completions keep pumping; app flushes, worker retirement, the SQLite
barrier and final replication release run as background tasks. IMAP retirement
interrupts passive IDLE waiting and completes `DONE` before reusing the session;
an unfinished read-only IDLE handshake closes its watch connection. It does not
cancel accepted mailbox mutations. Telegram seals command admission, forwards
accepted commands, then projects final updates without initiating authorization
or media requests while waiting for TDLib to close its databases. Files finishes
its accepted runs through the same worker passes. After workers retire, the
session consumes their final UI handoffs and drains resulting writes or native
compensation before the replication lease can be released.

IMAP connection reuse also bounds its read-only liveness check and discards an
unacknowledged connection. Notes snapshots retain the last explicit save's
completion and outcome, so later typing cannot erase an unseen save result or
leave the editor waiting for a completion it already missed.

Opening claims derive their prior state on the writer, alongside their change.
For example, opening a conversation records precisely which letters it marked
read, even when the UI continues navigating before the transaction completes.

## Measurements

These are local synthetic measurements, not production application benchmarks.

An identical SQLite recursive sum over two million rows took **168.6 ms** when
called synchronously. Requesting it through a UI snapshot returned in
**32.4 microseconds**; the background query completed in **171.8 ms**. This
moves the wait away from input handling; it does not speed up the SQL.
Reproduce with:

```sh
cargo test -p superapp-kernel store::queries::tests::expensive_query_ui_latency -- --ignored --nocapture
```

The HTTP comparison uses a local server returning delayed 26 KiB responses in
four chunks, with four rounds in alternating order:

| Concurrent requests | Previous client + threads | Tokio client | Elapsed change |
| ---: | ---: | ---: | ---: |
| 1 | 49.28 ms | 50.24 ms | 2.0% slower |
| 8 | 51.42 ms | 50.92 ms | 1.0% faster |
| 32 | 51.53 ms | 51.16 ms | 0.7% faster |
| 128 | 55.32 ms | 53.06 ms | 4.1% faster |

The 2 ms heartbeat did **not** improve: worst gaps were 2.53–3.02 ms for the
previous setup and 2.73–3.42 ms for Tokio. Thread startup is included in the
previous-client case, while the original application's account workers were
persistent. The test excludes TLS, TDLib, account-provider throttling, UI
rendering, RSS parsing and memory consumption. It cannot establish a broad
performance win. See `benchmarks/async-io/README.md` for commands and method.

The repository's existing Telegram timing tests were also run unchanged against
the saved baseline and migrated test binaries, in four alternating rounds:

| Telegram fixture | Baseline median | Migrated median |
| --- | ---: | ---: |
| UI poll with 10,000 pending requests | 87.30 µs | 85.64 µs |
| Open disk-backed 10,000-message chat, first preparation | 276.17 µs | 257.02 µs |
| Background transcript ready, measured from opening | 32.74 ms | 31.39 ms |
| Cached transcript access | 157 ns | 169 ns |

These small differences do not establish a broad improvement. The first
baseline chat-open sample was 5.13 ms; all rounds remain in
`benchmarks/async-io/telegram-results.csv`. The tests use fake providers and
inline workers, and do not create or rasterize a native window.

The source count compares every `*.rs` file under `app/src` and `kernel/src`
with the saved pre-migration tree in `.context/async-baseline`, on 2026-09-09:

| Scope | Baseline files | Current files | Baseline lines | Current lines | Added lines |
| --- | ---: | ---: | ---: | ---: | ---: |
| `app/src` | 309 | 325 | 131,397 | 141,866 | 10,469 |
| `kernel/src` | 34 | 40 | 29,771 | 31,645 | 1,874 |
| **Total** | **343** | **365** | **161,168** | **173,511** | **12,343** |

That is **22 more Rust files and 12,343 more lines (+7.658%)**, rather than a
code-size reduction. These are physical lines, including tests, comments,
blank lines and embedded UI definitions. They exclude dependencies, generated
build output, benchmarks, documentation and files outside those two source
trees; they are not a count of executable statements or binary size.

Reproduce the count from the repository root with the saved baseline present:

```sh
python3 - <<'PYTHON'
from pathlib import Path
for base in (Path('.context/async-baseline'), Path('.')):
    for scope in ('app/src', 'kernel/src'):
        files = list((base / scope).rglob('*.rs'))
        lines = sum(len(path.read_bytes().splitlines()) for path in files)
        print(base, scope, len(files), lines)
PYTHON
```

The hand-written HTTP framing became smaller, while the complete migration
added explicit pending states, completion handling, shutdown paths and
contention tests. These totals include the follow-up Calendar hover and conflict
recovery fixes and shared select component. Formatting and test coverage also
affect this line count.

## Validation boundaries

The final native-feature workspace run passed **1,363 tests**, with 12 ignored
manual/integration checks. Strict all-target Clippy passed. The independent
HTTP/SSE benchmark crate passed its 17 offline regression tests.
The earlier full headless run passed all **92 interaction suites**; an additional
headless Calendar check passed **61 tests**. The mixed form-focus check and
six select interaction checks also passed under headless configuration. Three earlier
local-bucket sync runs passed 64 scripted steps, including a running follower
displaying a peer's updated draft before and after taking over the lease.

Eight scripted native flows exercised Mail, Calendar editing/availability,
Telegram, Agent tools, Files operations, file-backed Notes, RSS and Accounts:
**532 steps, zero failures**. These used native asynchronous scheduling with
fake providers and `--no-draw`, which runs widget layout without rasterization.
The Calendar and recording regressions were reproduced and fixed without
extending script waits. A concurrent Telegram media-link regression also passed
100 repeated runs after its filesystem race was fixed.

The follow-up native pass exercised Calendar editing, Mail triage and mailboxes,
Files operations and Telegram: **524 steps across five flows, zero failures**,
with every process exiting through the draining quit lifecycle. Mail's original
native triage and mailbox failures were reproduced with stale display rows;
the unchanged scripts passed after filing began returning the successor from
its transaction. Tests also hold display readers and the writer independently,
cover intervening cursor/filter changes, and verify that one undo restores the
filing gesture's data, preview and marks.

The select and hover follow-up passed another **70 native scripted steps**
across the new select flow and the existing Calendar editor flow, using fake
providers and `--no-draw`. The persisted test draft retained its Calendar,
Free, and Private choices after keyboard dismissal, and both processes exited.
Widget regressions cover continuous hover through 119 pointer positions,
stationary-pointer detail refresh, 10,000 overlapping events, select option
refreshes, long-menu hit geometry, scrollbar dragging, and focus on dismissal.

Fourteen conflict-recovery regressions cover obsolete deletion alerts on restart,
reviewing the fetched event version, preserved local drafts, missing events,
partial recurring writes, lost responses, process interruption, and a draft save
prepared before a later failure. Recovery never resubmits a known rejected
version or dismisses an operation whose earlier outcome remains uncertain.
The rebuilt native Calendar editor flow passed another **34 scripted steps**
with fake providers and `--no-draw`, then exited successfully.

Final PR validation passed **236 native scripted steps** across Calendar editing,
Mail mailboxes, Notes, and file-backed Notes, with all four processes exiting
successfully. Additional regressions cover an unanswered IMAP liveness probe
and Notes save results retained across later typing before the next UI poll.

The regression tests cover a held SQLite writer, input during a held native
undo, ordering of navigation and commits, coalesced draft saves, late protocol
responses, disconnected IMAP fetches, interrupted SSE events, tool approval and
Stop races, atomic tool results, conditional object-store writes and graceful
native shutdown. Native background-path tests complement the deterministic
headless harness; passing the harness alone cannot establish UI responsiveness.

A remaining audit must consider synchronous point lookups in panel construction
and immediate platform APIs. Domain readers deliberately retain real
missing-row semantics; they must not be replaced wholesale by an empty
unloaded display snapshot. Startup migrations also run before the interactive
session. No production frame-time, resident-memory or battery measurement has
yet established an end-to-end performance result.
