# CR-016 · Workshop: local agent orchestration

Status: **native implementation of approved revision 03**, 2026-09-12.
The user approved implementation after rebasing onto the new device-sync model.
The lasting behavior and architecture are documented in
[Workshop](../book/src/workshop.md).

[Interactive design reference](../prototypes/orchestration/index.html) ·
[Draft run instructions](../prototypes/orchestration/README.md).
The HTML draft remains a design reference; the native app uses the actual shell
widgets, app registry, SQLite schema and local runtime.

## Accepted design

Workshop is a separate desktop app alongside the existing general-purpose
Agents chats. Its initial project boundary is a local Git repository. Projects
open the shared Workspaces rich table with an editable project tag. Without a
project filter it shows workspaces across projects by meaningful recent activity.
Unread agent results make rows bold. Panels retain native typography, compact
widths, standard headers and ordinary joined navigation; body headings,
breadcrumbs, marketing copy and modal diff views are absent.

The main chain is **projects → workspaces → workspace → agent chat**. The
workspace hub holds branch/PR information, chats and a small terminal. Review
uses a separate **file list → diff preview** pair. Step comparisons open normal
panels. Repository workspaces are app records independent of the shell's panel
layouts, even though both use the word workspace.

Settled interactions:

- New workspace immediately creates a generated city label and opens its hub
  and default chat; worktree preparation runs in the background. No name form.
- New chat immediately starts the default provider/model. Chats have no titles.
- Changing provider in an empty chat applies in place; after a user message it
  opens a new conversation and preserves the original. Models can change in an
  idle chat. Codex and Claude Code are the initial local subscription harnesses.
- Create PR is a prompt button routed within the same workspace: joined chat,
  then most recently used chat, then a new default chat. Push and AI review are
  buttons. There is no PR title/body form.
- Opening the embedded terminal in a panel keeps that live session and starts a
  new embedded one. There is no dock or return action.
- Review is whole file or nothing. Coverage counts changed lines, including
  filtered-out files, rather than file counts. A content edit reopens the entire
  reviewed file; unique unchanged diffs can retain review across rebases/renames.
  Only gutter copying is line-specific. No partial marks, filter verbs, generated
  change titles or next action.
- Personal review coverage does not gate merging. GitHub rules still apply.
- Comments publish to GitHub. Before a PR exists, offer draft-PR creation and
  preserve the unsent comment. There are no internal comment threads.
- SQLite inspection and tools expose app state/actions. Agent review marks retain
  their provenance and do not silently become human review. App MCP requests that
  require approval use native approve/refuse cards in the chat.

## Updated local-store boundary

The old proposal assumed full-table replication and a shared device write lease.
That assumption was removed by the main-branch sync changes before implementation.
Each device now owns a writable SQLite store. Replication captures only explicitly
registered table/key/column declarations; `App::replicated()` defaults to none.

Workshop registers its ordinary `workshop_*` schema and declares no replicated
tables. Repository paths, transcripts, snapshots, review marks, jobs and terminal
metadata stay local. Writes still use the existing serial writer and cached-query
invalidation. Live process/session coordination belongs to `Store::local`, scoped
to one database. No separate database, cloud setup, or sync write-gate exception
is necessary. Provider authentication stays outside SQLite.

## Implementation and validation

The app is registered only on desktop. Native Projects, Workspaces and Review
use the shared rich-table engine; other surfaces are ordinary panel instances.
Local Git/worktree operations, snapshots, harness adapters and GitHub jobs run
outside UI draws. Codex/Claude resume identifiers, interrupted runs, unsent drafts,
expected GitHub heads and whole-file correspondence are explicit persisted state.

`e2e/workshop/basic.txt` exercises native controls with deterministic fixtures.
Unit tests cover temporary Git repositories, review correspondence, command
routing, local storage, harness protocol parsing, and terminal lifetime. Fixture
GitHub actions do not validate or publish against a live user repository.

The accepted first implementation uses concurrent chats in the same worktree;
step intervals can contain shared changes. Merge defaults to squash and exposes
merge/rebase alternatives in the GitHub panel. Workspace archive/cleanup and
broader provider support remain future design work.
