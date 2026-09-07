# Telegram architecture review · 2026-09-07

The initial worktree was committed unchanged as `cc3955b` before this review.
The review covers the app registration, configuration and FFI; worker and
protocol handling; schema, projection and search; every panel/widget boundary;
shared media widgets, blob cache and the kernel store/worker contracts. The
[current architecture](../book/src/telegram.md) documents ownership after the
refactor.

The existing kernel / shell / app split is a useful foundation. Retain the
single store writer, pure update decoders, transactional projection, append-only
schema ladder, generic tables, and offline protocol tests. Most of the current
complexity comes from state crossing those boundaries without an explicit
owner, and from earlier development phases remaining in the code as comments
or unused abstractions.

## Implemented in this review

| Finding | Change |
|---|---|
| Global forwards, history/file queues, loading indicators and autoplay wishes let independent stores affect one another. | Added a small generic `Store::local<T>()` ownership seam and moved Telegram coordination into a store-scoped `Runtime`. Worker readers share it; independent stores do not. |
| Four copies of the transport gate could diverge. Sign-in directly used the global client and omitted the gate. | Replaced direct UI transport access with one in-memory command queue. The account owns its receiver, all panels use the same send boundary, and dropping the account disconnects it. Removed the global transport sender. |
| Model formatting and media lookup depended on the most recently drawn panel's thread-local clock and directory. | Passed time to rich-table rows and transcript rendering, and a media root to transcript rows. Playback cards use their own world's clock. Removed both thread locals. |
| `sync.rs` mixed protocol builders, worker logic and about 1,700 lines of tests. | Extracted `requests.rs` and `sync/tests.rs`. Panels depend on requests rather than the worker implementation. |
| An unused `WireSearch` abstraction promised fallback and inferred complete history from a row count. Neither was backed by the live UI. | Removed the speculative abstraction and completeness heuristic. Kept the tested FTS query and documented its actual use. |
| Reconciliation reread the Telegram config when constructing disposable worker descriptions. | Read config only when the retained worker initializes its native account. |
| Comments claimed active code had no callers, referred to unfinished phases, and described live mutations as undoable. | Rewrote the module contracts, narrowed dead-code allowances, and documented the difference between local fixture actions and live commands. |
| A runtime debug log was part of the initial worktree. | Preserved it in the checkpoint and locally; excluded it from subsequent source commits. |

Regression coverage includes two stores containing the same ids, readers on a
second thread, deduplicated fetch requests, forwards and autoplay that another
store cannot consume, login routing, disconnect after worker shutdown, and
playback labels that follow independent clocks without a widget draw.

## Follow-up priorities

These are remaining limitations, not guarantees provided by this refactor.

1. **Acknowledge live mutations explicitly.** `panels::told`, the chat composer
   and the list currently treat enqueue success as enough to clear input or
   apply a local flag. `sync::on_reply` handles selected failure cases, but most
   operations do not have an explicit pending/succeeded/failed state. Add a
   typed command id and an outcome for each operation. Use desired/actual state
   for reversible flags, and a durable non-idempotent send record with an
   explicit recovery policy. Login secrets must remain outside persistent
   payloads. Do not promise server-side undo for deletions.

2. **Track the identity of in-flight history work.** `Account::in_flight` holds
   only a timestamp. `pump` drops it after the patience timeout, and a history
   reply clears it without comparing it with the currently outstanding page.
   A late response can therefore release a newer request's pacing slot; a
   timed-out final request can leave its loading flag set. Extract a history
   scheduler that owns the page identity, retry deadline and completion state.
   Prove timeout, late reply, flood-wait and reopen sequences with virtual time.

3. **Make local authorization metadata actually local.** `schema::V3` creates
   `tg_session` as a normal app table. `kernel/src/store/repl.rs` selects such
   tables for replication. Separate authorization status from replicated
   content, including snapshot installation semantics. Keep the existing
   single-writer rule; avoid opening an ad hoc writable connection to the app
   database. The TDLib session files already live outside the projection.

4. **Give native client ownership an explicit lifetime.** The remaining
   `Telegram::engine_store` singleton compares directories and is set only
   once. It expresses the current one-account boot policy, not database
   identity, retirement or reconnection. Before supporting multiple live
   sessions, replace that admission policy with an engine handle tied to the
   admitted store and implement TDLib receive routing by client id. Removing
   the singleton alone would create competing consumers of its shared queue.

5. **Resolve media through the cache API, outside the draw loop.**
   `model::media_bytes`, `media_path` and `playable_path` know the cache's filename
   layout. Direct reads do not refresh `Blobs::get` recency, and transcript hit
   registration can read picture bytes again after rendering. Use a per-store
   resolver over the shared cache, track missing/loading/ready/failed by media
   key, and retain decoded images on widgets. File inspection and system opening
   also belong behind capabilities rather than panel-specific OS calls.

6. **Unify search after choosing its matching semantics.** `search.rs` and the
   message table use substring matching; `project::search_local` uses FTS prefix
   tokens. Replacing one silently changes what a query matches. Establish that
   contract, then share the indexed query and introduce server fallback with
   explicit coverage/cursors. Local row count alone cannot establish coverage.

7. **Keep SQL and interaction state in their own layers.** The worker still has
   many inline SQL patches beside the projection helpers. Move cohesive update
   operations into `project` as they change, with one transaction per update or
   batch. `Account` is confined to one worker thread, so its interior-mutability
   fields could become ordinary mutable state when the scheduler is extracted.
   Avoid a generic event bus or a repository interface for every table: they
   would obscure the existing explicit flow without removing a dependency.

8. **Separate template composition from widget behavior where it helps.**
   `ui.rs` embeds the sign-in widget while the other widgets have their own
   modules; `model.rs` also contains media filesystem resolution and simulated
   players. Move those responsibilities when introducing the resolver or
   changing the corresponding UI. Their size alone is not a reason to split
   every type into another file. Preserve the shared table and media widgets.

## Validation

The default workspace tests passed before edits. After the refactor:

- Workspace tests: 498 passed without TDLib; 500 passed with `--features tdlib`.
- Strict Clippy passed for all workspace targets in both configurations.
- The headless build and all 42 end-to-end suites passed, including Telegram's
  162-step suite. The mail/triage expectation fix from `origin/main` commit
  `f20ed0a` was applied before the final run.

Native-feature tests use fake accounts; the FFI smoke tests execute an offline
TDLib request and allocate an idle client id. Live login, sending and media
playback on a real account are not exercised by these checks.
