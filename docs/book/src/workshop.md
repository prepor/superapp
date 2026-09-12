# Workshop

Workshop runs local agents around Git repositories. It is a separate desktop
app alongside [Agents](./agents.md), without a gateway or orchestration cloud.
Codex and Claude Code run as local processes using their existing sign-in;
GitHub actions use the local `gh` installation. Model providers and GitHub
remain external services. Workshop is excluded from Android builds.

## Projects and workspaces

The launcher offers **projects**, **workspaces**, and **Workshop settings**.
Add a local repository in Projects. Selecting it opens the shared Workspaces
rich table with an editable `@project:` filter. Removing that filter shows
workspaces across repositories, ordered by meaningful activity. Opening a view
does not make its workspace recently active. Unread agent results make rows bold.

**New workspace** immediately creates an automatic city label and opens the
workspace plus its default chat. Worktree preparation runs in the background;
failures appear on the workspace. There is no name form. The project filter
chooses the repository, then the selected workspace's project, then the most
recently active project. Without a repository, creation opens Add repository.

A Workshop workspace is an app record independent of the shell's nine panel
layouts. It can appear in several slots. Worktrees live beside the local store
under `workshop/worktrees/<project>/<label>`.

The compact workspace shows branch/PR status, chats, and a small terminal.
Review has its own list and joined preview. Ordinary panel navigation applies:
selecting a table row previews its child, Enter enters it, and Command-open
keeps an independent panel. Closing a view does not cancel a chat.

**Archive workspace** hides the workspace from the usual table and stops its
running/queued agents and embedded terminal. Worktree files, transcripts,
review marks, and independent terminal panels remain. Add the **`@archived`**
tag to the table to see archived workspaces, including within a project filter.
**Restore workspace** brings one back; cancelled prompts and pending GitHub
writes do not restart. An operation already sent to GitHub retains its outcome.

## Chats and providers

**New chat** immediately opens the default provider/model. Chats have ordinals
and no titles. Different chats in one worktree can run concurrently; requests
within one chat run in order. **Close chat** removes it from the open chat list,
stops its current run and queued prompts, and retains its transcript, draft,
provider session and change history. Open **closed chats** in the workspace to
read one again; **reopen chat** restores it without resending cancelled prompts.

An empty chat changes provider in place. After a user message, changing provider
opens a new chat and preserves the original transcript/session. The picker says
**opens in new chat**. Model changes stay in the same idle chat. The model menu
uses cached harness discovery and includes **custom model…** for a provider ID.

Settings shows executable/authentication status, sign-in actions and defaults.
Sign-in opens the provider's login flow in an ordinary terminal. Credentials
stay in the provider's authentication store, outside transcripts and SQLite.
Codex uses `codex exec --json` and its resume command; Claude Code uses streamed JSON.
**Work** permits workspace execution; **plan** uses read-only permissions.
Stop cancels the run and retains its transcript. A later explicit send can
resume the saved provider session. Restart recovery records interrupted runs.

**Preview diff** appears only when a turn's before/after comparison contains
changed files, including binary, rename and mode changes. No-op and commit-only
turns keep their transcript and history without offering an empty preview.
Step cards open ordinary diff panels, using a comparison list for multiple
files. Snapshots include overlapping writers' edits; an interval does not claim
that one chat authored every change in it.

## Whole-file review

The review list pins an explicit comparison. Its filter narrows filenames and
review state; clearing the filter cannot expose another workspace. A newer
snapshot is offered as **new changes available** without replacing displayed
code beneath the person reviewing it.

Review is **whole file or nothing**. The preview offers **mark file reviewed**
and **mark file unreviewed**. Opening, scrolling and AI review do not mark human
coverage. There are no range or hunk marks. The gutter's only line-specific
action copies `file:line`; deleted-side references identify the base side.

Progress counts added and deleted lines once each, across the whole comparison
including hidden files. Context does not count. A file contributes all its
changed lines or none. Binary, pure-rename and other changes with no textual
lines have a separate outstanding count.

Marks identify immutable complete-file diffs. Unique exact correspondence can
preserve a mark through a rebase or rename, ignoring locator-only changes such
as hunk line numbers. Changed content or relevant context, conflicting
resolutions, and ambiguous correspondence reopen the whole file. Its current
changed lines become **needs recheck**; newly changed files contribute **new**
lines. Historical snapshots and marks remain readable.

Personal progress is informational. Merge and auto-merge follow GitHub's
repository rules and never require 100% local review. Agent tool marks retain
agent attribution and do not count as human coverage.

## GitHub

**Push** explicitly pushes the expected commit. **Create PR** sends a normal
prompt to the joined chat in that workspace, then its most recently used chat,
then a new default chat. The harness prepares the title/body and creates the PR;
there is no separate PR form. **AI review** and **fix errors** send comparison
or failure context to a workspace chat.

The GitHub panel shows cached PR/check state, expected head, merge method,
personal progress, and merge/auto-merge actions. Unknown and stale data are
identified. External actions are durable operations; an interrupted publication
is not automatically retried.

Comments go directly to GitHub. File/general composers keep an unsent draft per
workspace/file. Without a PR, **create draft PR** uses the same chat routing
while retaining the comment. Once the PR exists, **post to GitHub** publishes
it. A file comment requires the corresponding published file; unpublished local
code cannot be presented as a GitHub diff. Publication failure leaves the draft
unsent. If GitHub's response is lost, the operation asks the person to check the
PR before posting again; a second manual submission could duplicate a comment
that GitHub already received. There are no internal comment threads.

## Terminals

The workspace embeds the existing [Terminal](./terminal.md) engine, starting
in its worktree. **Open panel** opens the same session independently, preserving
process, directory, scrollback and partially typed input. A fresh session
immediately replaces it in the workspace. There is no dock or return action.

Workshop owns its sessions independently of views. Closing the workspace does
not stop promoted terminals. Tools can list sessions, send input, read bounded
output and explicitly terminate a session. Restarting cannot restore a dead
process; SQLite retains metadata rather than presenting old PTYs as live.

## Store and tools

[Device sync](./device-sync.md) is opt-in per table and column. Workshop returns
no replication declarations: paths, transcripts, snapshots, marks, jobs and
terminal metadata do not enter the sync log. Every device owns its writable
store, without a cloud lease. Writes use the serial writer; panels use cached
queries. Runtime registries use `Store::local` and belong to one database.

| Tables | Contents |
|---|---|
| `workshop_project`, `workshop_workspace` | Repository/worktree identity, activity and GitHub observations |
| `workshop_chat`, `workshop_message`, `workshop_run` | Untitled conversations, transcripts, provider/model/session and execution |
| `workshop_snapshot`, `workshop_change`, `workshop_step` | Comparisons, file patches and agent intervals |
| `workshop_review` | Attributed whole-file review history |
| `workshop_job`, `workshop_comment_draft` | External operations and unsent GitHub text |
| `workshop_tool_call` | App-tool requests, approval state and results |
| `workshop_setting`, `workshop_terminal` | Defaults, provider discovery and terminal metadata |

`sql.schema` describes these tables and `sql.query` reads them. Prefer
`workshop.*` tools for changes: they share the UI command path, enforcing routing,
expected heads, session rules and review attribution. Workspace listing defaults
to active records; `archived: true` selects the archive. Chat listing defaults
to open records; `include_closed: true` also returns retained conversations.
The `workshop.workspaces.archive`/`restore` and `workshop.chats.close`/`reopen`
tools use the same reversible lifecycle actions as the UI. Workshop declares its tables
protected from `sql.write`; direct app-tool mutations use the validated commands.

Local harnesses receive a loopback app MCP connection with a token scoped to the
current run, for registered app tools, including SQL and panel tools. Plan runs
cannot call writing app tools. Requests marked as requiring approval appear in
the chat with **approve** and **refuse**. Arguments, state, result and error stay
visible. Refusal is returned to the harness. Stopping invalidates the run token;
interrupted calls are never replayed or silently approved.

## Validation

Fixture runs use fake GitHub/provider behavior and the terminal's demo shell.
`e2e/workshop/basic.txt` exercises real native controls. Unit tests cover Git
snapshots/correspondence in temporary repositories, local storage, command
routing, harness parsing and terminal ownership. These checks do not publish a
live PR, comment, push or merge on the user's behalf.
