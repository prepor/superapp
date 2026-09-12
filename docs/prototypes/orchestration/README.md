# Workshop UI draft 03

Revised September 12. Compact native panels follow
**Projects → Workspaces → Workspace → Agent chat**. Review uses a separate file
list and joined diff preview. Creation actions open their results immediately.

[Open the draft](index.html) · [Design proposal](../../planning/cr-016-orchestration.md)
· [Conductor reference](reference.md)

## Run

From the repository root:

```sh
python3 -m http.server 8766 --bind 127.0.0.1
```

Visit [Workshop on localhost](http://127.0.0.1:8766/docs/prototypes/orchestration/?draft=03).
No install or build is needed; opening `index.html` directly also works.
Sample state and review marks persist in browser `localStorage` when available.
Use **reset sample** in the footer to restore the starting fixture.

## Try the draft

The footer's scene buttons arrange panels; its **simulate** selector injects
sample events. These controls are outside the proposed app.

1. **Browse and create.** Opening a project prefills the Workspaces filter with
   its `@project:` tag. Edit the ordinary filter directly; the **workspaces**
   scene shows all projects by recent activity. Unread agent results make a row
   bold. **New workspace** creates a city-labeled workspace and opens it with a
   default chat. **New chat** immediately opens a default chat. Neither uses a
   naming form; chats have no titles.

2. **Review files.** Open the workspace's **review** action. Select a filename
   to open its diff beside the list. Mark the entire file reviewed or unreviewed;
   there are no partial review marks. The meter counts added and deleted code
   lines and starts at **46 / 67**. Clicking a code line number copies its
   `file:line` reference. Comparison and review filters stay in their fields.

3. **Recheck changes.** From the reset fixture, choose **simulate → agent edit**.
   Choose **load changes** if a diff is already open. The entire edited file
   reopens and progress becomes **23 / 70**, including
   **26 lines needing recheck**. Filter for changed files. **Simulate → rebase +
   rename** changes a path and line numbers while preserving coverage. Reload
   to check persistence. Viewing code or running **AI review** does not mark it
   personally reviewed.

4. **Switch chats and providers.** Chats expose provider and model controls.
   Changing provider in an empty chat updates it in place; changing provider
   after a conversation starts opens a new untitled chat and preserves the old
   one. Changing model within a provider stays in the chat. A chat's change row
   opens an ordinary joined diff panel. Normal opens replace their joined child;
   Cmd-click a row or navigation link to keep an independent panel.

5. **Open the terminal panel.** Type an unfinished command in the workspace
   terminal and choose **open panel**. That session, output, and input move to an
   independent terminal panel; the workspace immediately gets a fresh terminal.
   There is no return action. `pwd`, `git status`, and `cargo test` produce canned
   output; no command executes.

6. **Try GitHub actions.** **Push** is a button. With no PR, **Create PR** sends
   a prompt to the joined workspace chat, its most recently used chat, or a new
   default chat, in that order. Choose **simulate → PR created** to complete the
   sample agent request.
   **AI review**, **fix errors**, and **fix conflicts** open agent activity.
   Comments go to GitHub; with no PR, create a draft PR through the chat before
   posting. Merge and auto-merge follow the sample PR/check state; personal
   review progress never gates them. Footer simulations provide failures,
   conflicts, a missing PR, and offline GitHub.

## Scope

This is a browser UI draft. Worktree creation, harnesses, authentication, Git,
GitHub writes, checks, terminal execution, and agent tools are simulated. Rebase
and edit correspondence uses fixture identities, not a real matching algorithm.
SQLite, PTYs, and backend integrations come after UI review. Zurich contains the
complete review fixture; other workspaces demonstrate navigation.

The visual references are the existing [panel model](../../book/src/panel-model.md),
[rich table](../../book/src/richtable.md), and
[interaction grammar](../../book/src/interaction-grammar.md). The draft uses
native Geist Mono and IBM Plex Sans Text faces; the copied Plex font and its
[OFL notice](fonts/IBMPlexSans-OFL.txt) are bundled locally.
