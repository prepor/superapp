# Conductor reference

Research for the Workshop orchestration UI draft, inspected **2026-09-10** and
updated with shipped-code flow checks on **2026-09-12** for draft 03.
This records relevant evidence and design implications, not implemented features.

## Scope and evidence

The installed app is `/Applications/Conductor.app`, version **0.84.2** according
to its `Contents/Info.plist`. Inspection covered shipped application files and
official public documentation. Private chats, credentials, and user databases
were not inspected. Static executable evidence was not verified by live tracing.

The app bundles a GitHub CLI, a local orchestration runtime, and a Git checkpoint
script in `Contents/Resources/bin`. The runtime contains a Codex launcher using
`app-server --listen stdio://` and a Claude Code launcher using streaming JSON
input/output. These transport findings come from the installed code.

The September 12 inspection decoded embedded frontend resources
`/assets/renderApp-DAP_nquj.js`, `/assets/index-rOlkAfxh.js`, and
`/cities_top_100.txt` from the installed main executable. Minified function names
below identify evidence in this build; these are static observations, not live
UI tracing.

## Workspaces and chats

Conductor gives independent workspaces separate branches and working trees.
Chats within one workspace share the branch and files. Implementation, review,
and testing can happen in separate chats against that same code. Concurrent
agents can modify overlapping files.
[Parallel agents](https://www.conductor.build/docs/concepts/parallel-agents)

In the shipped `index-rOlkAfxh.js`, workspace creation (`JZ`) inserts the workspace
and immediately calls `sessions.createSession`; a user-supplied workspace name is
optional. `YZ`/`QTe` choose an unused city name; `VZ`/`Mp` check collisions and add
numbered suffixes. Our **New workspace** therefore creates a city-labeled
workspace and default chat immediately and opens both panels. **New chat**
immediately opens a chat with the configured default provider/model. Chats in
our app have no title concept.

The shipped provider picker (`uoo` in `renderApp-DAP_nquj.js`) changes provider
and model in place while the chat is empty. After `lastUserMessageAt` is set,
a provider change creates a new session and navigates to it; `r6o` labels those
choices **Opens in new chat**. Our draft follows that behavior: a started chat
is preserved, and switching Codex ↔ Claude opens a new untitled chat in the same
workspace. Model changes within one provider stay in the conversation. Native
session IDs and transcripts are not implicitly transferred between harnesses.

For this draft, the proposed default is **the same checkout, with concurrent
agents visible**. The workspace shows who is running and what they are doing.
Precise authorship must not be inferred from a snapshot interval containing
overlapping writes; that interval is labeled as shared workspace changes.
This remains a product decision awaiting the user's response.

## Review, GitHub, and step changes

Conductor's diff viewer offers changed-file navigation, unified diffs, commit
filtering, line comments, and GitHub thread resolution. Its suggested actions
follow the current workspace state.
[Diff viewer](https://www.conductor.build/docs/reference/diff-viewer)

Checks combines Git state, PR metadata, CI, deployments, GitHub discussions,
and todos. Review can invoke an agent against the current diff. PR creation can
ask an agent to draft a description from the diff and repository context.
[Checks](https://www.conductor.build/docs/reference/checks),
[Review and merge](https://www.conductor.build/docs/guides/review-and-merge)

The shipped Create PR callback (`O2i`) resolves a workspace session (`RK`) and
invokes the PR action. `WCt` queues **Create a PR** with generated PR instructions
through the normal message path (`Fv`); it does not open a title/body form. The
instructions account for the branch, base, PR template, draft flag, and provider.
`O2i` also exposes a separate direct Push callback. Our draft uses **Create PR**
and **Push** buttons and calls agent review **AI review**.

Conductor's inspected session resolver prefers the current workspace route,
then its stored active session, then the first visible session. Our chosen
routing is the currently joined chat, then the most recently used chat in the
same workspace, then a newly created default chat. That final fallback and the
recency ordering are our design choices, not verified Conductor behavior.

Conductor saves automatic checkpoints outside ordinary branch history. The
bundled script uses private Git refs and snapshots tracked plus nonignored
untracked files without moving the working branch HEAD. A snapshot includes
all changes made in its interval, including other chats or the user.
[Checkpoints](https://www.conductor.build/docs/reference/checkpoints)

The inspected evidence does **not** establish that Conductor preserves personal
review progress through rebases. Persistent **whole-file review** is a proposed
capability of this app. A uniquely matched unchanged complete file diff keeps
its mark through a pure rebase or rename. Editing a reviewed file reopens the
entire file; modified or ambiguous diffs require rechecking. Correctness requires
implementation tests.

Review marks apply to whole files only, while progress counts their added and
deleted changed lines. Binary or metadata-only changes remain separate items.
The file list displays filenames; there are no titled chunks, line/hunk marks,
or next action. Filters remain ordinary fields. The code gutter only copies a
specific `file:line` reference. Personal review progress is **informative**:
the person may merge with any percentage, subject to GitHub repository rules.

This draft uses GitHub for file or general PR comments. With no PR, offer to
create a draft PR through the joined/recent/default chat before posting the
preserved comment. Local-only files need a published matching diff or an
explicitly chosen general PR comment. Draft text can be saved locally; it does
not become a separate internal discussion system.

## Codex and Claude Code subscriptions

Conductor's documented setup uses managed or custom harness executables and
existing CLI authentication. Both Codex and Claude Code can use subscriptions.
Provider environment variables can instead select API billing, so setup should
show the active authentication mode rather than assume it.
[Providers](https://www.conductor.build/docs/guides/providers),
[Codex](https://www.conductor.build/docs/reference/harnesses/codex),
[Claude Code](https://www.conductor.build/docs/reference/harnesses/claude-code)

Codex's documented rich-client route is a local app-server. It exposes sessions,
streamed events, approvals, and account operations through JSON-RPC. Managed
ChatGPT login lets Codex own OAuth, token storage, and refresh. The installed
Conductor runtime also contains an external-token path; our adapter need not
copy that path. Integration schemas should match the chosen harness version.
[Official OpenAI app-server documentation](https://learn.chatgpt.com/docs/app-server)

Claude Code supports subscription login through its CLI. Its programmatic
interface provides streaming JSON and explicit session resume. The current
`--bare` option skips subscription credentials, so it cannot be our subscription
default. Keep authentication and session lifecycle with the actual harness.
[Claude Code authentication](https://code.claude.com/docs/en/authentication),
[Programmatic Claude Code](https://code.claude.com/docs/en/headless)

## Boundaries of this proposal

The user confirmed a separate app alongside the existing generic Agents app.
**Workshop** is the proposed name because the work includes code, research,
documentation, and other project files. Proposed tools use `workshop.*`;
technical prototype paths retain `orchestration`. Local SQLite state, tool
parity, whole-file review persistence, and terminal ownership are our design choices;
the research does not claim to reproduce Conductor's complete internal schema.
**Open panel** moves the existing embedded terminal session into an independent
terminal panel and immediately starts a fresh embedded session. There is no
return or dock action; this is the user's requested behavior.

Local execution means no hosted workspace, orchestration cloud, or app-owned
model gateway. The chosen harness still contacts its model provider, and GitHub
actions contact GitHub. Android is outside this proposal. The current artifact
is a UI draft; its actions do not start agents or publish GitHub changes.
