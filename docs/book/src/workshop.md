# Workshop

Workshop runs local agents around Git repositories. It is a separate desktop
app alongside [Agents](./agents.md), without a gateway or orchestration cloud.
Codex and Claude Code run as local processes using their existing sign-in;
GitHub actions use the local `gh` installation. Model providers and GitHub
remain external services. Workshop is excluded from Android builds.

## Projects and workspaces

The launcher offers **projects**, **workspaces**, and **Workshop settings**.
Add a local repository in Projects: **add repository** stands a path field and
offers **browse** (`b`), which opens the [files](./files.md#the-picker) browser
as a picker beside it. The folder chosen there is added at once. Selecting a
repository opens the shared Workspaces rich table with an editable
`@project:` filter. Removing that filter shows
workspaces across repositories, ordered by meaningful activity. Opening a view
does not make its workspace recently active. Unread agent results make rows bold.
Generated project tags retain a repository ID, for example `@project:"superapp #1"`,
so repositories with the same name stay distinct. A manually typed name can match
multiple repositories; creation asks for a specific project tag in that case.
Each result becomes read after it is displayed at the end of the focused chat;
reading an older result cannot acknowledge a newer one that arrives meanwhile.

**New workspace** immediately creates an automatic city label and opens the
workspace plus its default chat. Worktree preparation runs in the background;
failures appear on the workspace. There is no name form. The project filter
chooses the repository, then the selected workspace's project, then the most
recently active project. Without a repository, creation opens Add repository.

A Workshop workspace is an app record independent of the shell's nine panel
layouts. It can appear in several slots. Worktrees live beside the local store
under `workshop/worktrees/<project>/<label>`.

The compact workspace is titled *project / label*. It shows the branch, the
base it will merge into with the pull request — its number as a link, or
**create PR** — and the PR's state in a few words: *checks passed*, *draft*,
*2 checks failed*, *rebase conflict*, *merged*; *changes not pushed* with
**push** appears while the PR's head is behind the local one. Its chat table
names each chat, its agent, and the last thing said in it, or *working…*
while it runs; unread results are bold. A small terminal closes the panel.
Review has its own list and joined preview.

The chat table is a list like any other: arrows walk it and preview the chat
they land on while the keyboard stays in the hub, a click moves the cursor
there and previews it too, Enter enters, and Command-open keeps an independent
panel. There are no marks — nothing here acts on a set of chats. The cursor
follows whatever the hub has joined, so a new chat lands the cursor on itself.
The embedded terminal wants every key there is and keeps the ones it is given:
while its caret is in the grid the arrows are the terminal's, and a click on a
chat row, or moving focus away and back, gives them to the list again. The
*closed chats* panel is the same list and walks the same way. Closing a view
does not cancel a chat.

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
**Work** permits workspace execution and answers no approval prompts, since a
headless run has nobody to ask; **plan** uses read-only permissions.
Stop cancels the run and retains its transcript. A later explicit send can
resume the saved provider session. Restart recovery records interrupted runs.
App shutdown retires active harnesses, including those waiting for approval,
revokes their tool connections, and saves accepted output before closing.

## The transcript

A chat draws what the harness streams as the prototype lays it out: the
person's turn in a washed block, the agent's prose as Markdown, and one
bordered **card** per thing the agent did, in the order it happened. The
provider and model selectors sit above the transcript with the chat's state
at their right; the composer below carries the **work / plan** mode selector
beside **send** and **stop**.

A card's line says the tool, what it was asked in one phrase — the command,
the path, the pattern, the query, the subagent's description — and where it
stands: *running…*, *in background…*, *failed*, *denied*, *stopped* or
*interrupted*. Output is folded behind the line and opens on a press; a failed
or denied call opens by itself, in the error colour. Cards are one shape for
both providers: Claude Code's tool uses and Codex's command executions, file
changes, MCP calls and web searches all arrive as the same rows.

- A **todo list** (Claude's `TodoWrite`, Codex's `todo_list`) is one card that
  updates in place: its line counts what is done, its body lists every item
  as `[x]`, `[>]` for the one in progress, and `[ ]` for the rest.
- A **subagent** (Claude's `Agent`, Codex's collab calls) is a card with its
  description; progress reports fill a muted line with tool uses, tokens and
  the tool it is on. Its own calls arrive under it, one level in, and show
  when the card is open. Claude marks them with `parent_tool_use_id`; Codex
  reports the agents' states on the call itself.
- A **background task** — a shell command run in the background, or a task
  the harness starts on its own — keeps its card open at *in background…*
  until the harness's notification finishes it with its summary. The
  transcript is the one place to see what is still running.
- App tool requests that need approval are the same card, wearing **approve**
  and **refuse** while they wait.

Every item is a row of `workshop_item`, keyed by the provider's own identity
for it, so a later event updates the card rather than adding a line. A run
commits what it has streamed about eight times a second, in one transaction,
rather than once per token: each commit redraws the app, so the writer sets
the frame rate a long answer draws at. The run's final prose is also the body
of its `workshop_message`, for tools and context.

A chat opens where it was left. The position is the row that stood at the top
of the view and how far into it, kept per chat rather than per panel — a chat
is one conversation however many panels show it — and it survives a restart.
A chat sitting at its end saves nothing, which is how it goes on following
what arrives; scroll up and the row is remembered, scroll back down and it is
the tail again. The row is found by its own key, so a transcript that grew
meanwhile still opens on the line that was being read, and a row that has gone
falls back to the tail. Saving happens when the scrolling stops, and
again if a quit, an undo walk or a close comes first. None of them waits for
the writer — an undo is a keystroke — so what a chat opens on is the position
this process has in hand rather than the row, which may still be one write
behind. The offset
into that row is kept only where the row will be the same height next time: a
card the reader had opened is drawn closed again, so its own reading comes
back to the top of it rather than to a distance down output that is no longer
there. Reading is not using, so it moves neither the workspace's activity nor
the chat's. A chat reopened away from its end keeps
its unread mark, because a result is read only when it is drawn at the visible
tail of a focused chat.

**View changes** follows a turn's prose when its before/after comparison
contains changed files, including binary, rename and mode changes, with the
count of added and deleted lines. No-op and commit-only turns keep their
transcript and history without offering an empty link. The link opens an
ordinary diff panel, or the comparison list for multiple files. Snapshots
include overlapping writers' edits; an interval does not claim that one chat
authored every change in it.

## Naming the work

A new workspace's branch is a placeholder, `workshop/<label>`. Sending a
message while the branch is still that placeholder, with no pull request,
queues a **naming job**: a separate one-shot, read-only call through the same
provider and model as the chat, asking for a short hyphenated name for the
task in the message, or a sentinel when there is nothing to name. The answer
is cleaned into a slug, prefixed `workshop/`, suffixed `-vN` if another branch
holds it, and applied with `git branch -m` — only if the worktree is still on
the placeholder and still has no PR when the answer arrives. One job runs per
workspace at a time; a message that yielded no name lets the next one try. A
failed call is the job's failure alone, not the workspace's. This is how
Conductor names its workspaces; the difference is that Workshop keeps the city
label as the worktree's name and puts the task name on the branch only.

A workspace is then **called** what it is about: its pull request's title once
one exists, else the named branch read as words (`fix-login-timeout` becomes
*Fix login timeout*), else its city label. The hub is titled *project / that
name*, and the workspaces table shows it over the city and the project.
Snapshots retain ignored files explicitly added to Git's index. Rename and mode
changes show their metadata even when no text lines changed.

## Whole-file review

The review list pins an explicit comparison. Its filter narrows filenames and
review state; clearing the filter cannot expose another workspace. A newer
snapshot is offered as **new changes available** without replacing displayed
code beneath the person reviewing it.

The list opens with a meter: reviewed over total changed lines, what is left,
and a thin bar — ink for reviewed lines, hatched for reviewed files that
changed since — with the recheck and new counts under it. A file still to
review is bold; a reviewed one wears ✓ beside its count. The diff shows the
file's path and the comparison it belongs to, then each hunk with both line
numbers, the sign in a column of its own, added lines on a light wash and
deleted ones hatched. Lines never wrap: the code column is as wide as the
file's longest line and scrolls sideways as one, while the list scrolls down.

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
Merge defaults to squash; the GitHub panel also offers merge and rebase.

Comments go directly to GitHub. File/general composers keep an unsent draft per
workspace/file. Without a PR, **create draft PR** uses the same chat routing
while retaining the comment. Once the PR exists, **post to GitHub** publishes
it. File comments retain the exact comparison opened by the reviewer, including
historical previews, and queued publication reads that saved version. A file
comment requires the corresponding published file; unpublished local code cannot
be presented as a GitHub diff by substituting later edits. Publication failure
leaves the draft unsent. Older saved file-comment panels without a comparison
reference ask you to reopen the file's diff before posting. If GitHub's response
is lost, the operation asks the person to check the
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
| `workshop_chat`, `workshop_message`, `workshop_run` | Untitled conversations with their draft and reading position, transcripts, provider/model/session and execution |
| `workshop_snapshot`, `workshop_change`, `workshop_step` | Comparisons, file patches and agent intervals |
| `workshop_review` | Attributed whole-file review history |
| `workshop_job`, `workshop_comment_draft` | External operations and unsent GitHub text |
| `workshop_tool_call` | App-tool requests, approval state and results |
| `workshop_item` | The structured transcript: text, tool calls, todo lists, subagents and background tasks, keyed by the provider's item identity |
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

The browser prototype in `docs/prototypes/orchestration/` is a design reference
with simulated Git, provider and GitHub behavior. Workspace archive
and chat close/reopen are implemented natively; deleting retained worktrees
and broadening the provider set remain separate work.

Fixture runs use fake GitHub/provider behavior and the terminal's demo shell.
`e2e/workshop/basic.txt` exercises real native controls, including the chat
list's arrow walk. Unit tests cover Git
snapshots/correspondence in temporary repositories, local storage, command
routing, harness parsing and terminal ownership. These checks do not publish a
live PR, comment, push or merge on the user's behalf.
