# CR-011 · Agents: a chat over the store, with the apps as its hands

Status: **proposed** (Andrey, 2026-09-04: agentic abilities for the whole
app — a panel as context for an agent, an agent API on the apps, direct
SQL, Cloudflare AI Gateway first, macOS and android alike, no binaries).
Nothing is built yet; this document is the plan, written as the book
should read once it lands.

## Why

Every app here already keeps its state in one SQLite file, every panel
already knows which queries drew it (`cmd+i`), and every change already
goes through one undoable door, `Session::act`. That is most of what an
agent needs: a readable world, a way to say what the person is looking at,
and a way to act that can be taken back. What is missing is the agent
itself — a chat, a model behind it, a description of the data in the
apps' own words, and a set of tools the apps offer — and one rule that
keeps it honest: **an agent acts only through what a person could do,
and every one of its acts is an ordinary undoable action**.

`shell/context.rs` says it already: *the agent handoff this feeds is
future work; the surface is ready.* This is that work.

## The words

- **Agent**: what answers in a chat — a model behind the gateway, a system
  prompt, and the tools this build offers it. One agent to start, the
  assistant; profiles later.
- **Chat**: one conversation with an agent, a row and a panel. Its
  **turns** are the messages, a person's and the agent's, and a turn may
  carry **chips**.
- **Chip**: a piece of context pasted into a chat as a thing rather than
  as text: *a panel*, today; a file, a letter, a selection, later. It
  stands in the composer and in the transcript as a small labelled block
  with its own `×`, and is rendered into text for the model at send time.
  Not *chip*: a chip is a count on something else, which the problems
  mark already is.
- **Run**: one round of the agent working on a chat: a request to the
  gateway, the tool calls it asks for, their results, until it stops. A
  run is a row; a worker drives it.
- **Tool**: one thing an app lets an agent do, by name, with a JSON
  schema. *Send this mail*, *rename this file*, *run this query*. A
  **call** is one use of a tool inside a run — a row, so the chat panel
  can run it on the UI thread and the transcript can show it.
- **Gateway**: where requests go — a Cloudflare AI Gateway in front of
  the model. The token that opens it is the one device sync already has.

## The model

### One agent app

`agent` is an app like mail and files: it registers its panel kinds, a
schema ladder, one in-memory effect, one capability of its own, a
problem source, its workers, and its roots. The kernel and the shell keep naming
no app — what changes in them is generic: a panel can say what it is
*about*, an app can offer *tools* and *describe* its tables, and the store
can hand an agent a changeset to undo.

### The store is the bus

A run has three parties on three threads: the person in the chat panel
(the UI thread), the worker talking to the gateway (its own thread and
world), and the writer thread that every write goes through. They meet in
rows, the way sync workers and mailboxes already do:

1. **A person sends.** The chat panel files one action: the turn row and a
   `run` row in one transaction, then kicks the run's worker.
2. **The worker asks the model.** It builds the request from the chat's
   turns, streams the reply, keeps the streaming text in memory beside
   the run (the effect ring's pattern — a live tail, never a row per
   token), and on `end_turn` writes the agent's turn as a row.
3. **The model asks for a tool.** The worker writes a `call` row per
   entry of `tool_calls`, sets the run to *waiting*, and sleeps `OnKick`.
4. **The tool runs on the UI thread.** The chat widget, on the event the
   worker's notify wakes it with, runs every pending call through
   `Session::act` — one history node per call, labelled by the tool —
   writes each result to its row, and kicks the worker. Nothing asks
   first: `cmd+z` is the answer to a call that should not have run.
5. **Back to 2**, until the model stops, the run fails, or the person
   stops it.

The chat panel is the run's hands: a run whose chat is shown nowhere
pauses at its next call and picks up when the chat is opened again. The
agents list says which runs are waiting. Nothing in the kernel schedules
anything for the app.

### Tables

All prefixed `agent_`, in the app's ladder:

| Table | What a row is |
|---|---|
| `agent_chat` | one conversation: `title`, `model`, `created`, `updated` |
| `agent_turn` | one message: `chat`, `seq`, `role` (`user`/`assistant`/`tool`), `body` (JSON, the wire's message verbatim — `content`, `tool_calls`, `tool_call_id`, `reasoning_content`, and the app's own `chip` parts), `created` |
| `agent_run` | one round: `chat`, `status` (`pending`/`streaming`/`waiting`/`done`/`failed`/`stopped`), `error`, `started`, `ended`, `usage` (tokens in, out, cached) |
| `agent_call` | one tool call: `run`, `turn`, `tool_call_id`, `tool`, `input` (JSON), `status` (`pending`/`done`/`failed`), `output` (JSON), `created`, `ended` |

Nothing about the gateway is a row. Its token is R2's (below), and a
secret never goes in the store — the one thing that must not replicate.
Turns replicate like every other row: a chat continued on the phone is
the same chat.

A turn's `body` is the wire's message, stored as text, because the next
request is built from it verbatim and a `sqlite3` reader can still read
it. The app's `chip` part is rendered into text before the request goes
out and drawn as the chip again when the transcript is read back.

## The gateway

### Cloudflare AI Gateway, and the model behind it

The model is hardcoded: `@cf/zai-org/glm-5.3-flash` (Andrey,
2026-09-04) — one of the models Cloudflare hosts itself, which the
gateway reaches as one more provider, `workers-ai`: a million tokens of
context, function calling, streaming, a reasoning mode, and priced so
that a day of chatting over one's own mail costs cents. It is one
`const` in the app, with the
reasoning effort (`medium`) beside it; changing either is a commit, not
a setting. Requests speak the **OpenAI-compatible chat-completions
API**, the one shape Workers AI, the gateway's `/compat` route, and
every provider behind it share, so a later model — or a later provider —
is the same wire under another name:

```
POST https://gateway.ai.cloudflare.com/v1/{account}/{gateway}/workers-ai/v1/chat/completions
Authorization: Bearer {cloudflare api token}
cf-aig-authorization: Bearer {the same token}
content-type: application/json

{"model": "@cf/zai-org/glm-5.3-flash", "stream": true,
 "stream_options": {"include_usage": true}, "reasoning_effort": "medium",
 "messages": […], "tools": […]}
```

One token: the R2 token, with *AI Gateway Run* and *Workers AI Read*
added to it in the dashboard, is the provider's key and the gateway's at
once — the same value in both headers — so there is nothing to bring,
store, or alias in the gateway (below). The gateway is used
for what it is good at — logs, analytics, rate limits, retries — set up
in its dashboard, not in the app. Phase 0 settles the exact route with
one `curl`: the provider route above, or Cloudflare's REST route
(`…/accounts/{account}/ai/v1/chat/completions` with
`cf-aig-gateway-id`), whichever the docs hold to that week; the app sees
a URL and two headers either way.

Not the Anthropic Messages API, and none of its extras: the model is not
Anthropic's, and the chat-completions shape carries what this change
needs — tools as `function` definitions, `tool_calls` on the agent's
message, a `tool` message per result, `finish_reason`, `usage` on the
last chunk, and the model's reasoning as `reasoning_content` when it
sends any. Prompt-caching directives, adaptive thinking and refusal
fallbacks are Anthropic's; they come with an Anthropic-native `Gateway`
implementation if a model ever needs one (*not done*, below).

One header the app sets by policy: `cf-aig-collect-log-payload: false`,
unless `SUPERAPP_AGENT_LOG_PAYLOAD=1`. A chat carries the person's mail;
the gateway keeps counts and status, not prose, by default. That is the
app's one environment knob, read by itself, because argv belongs to the
shell.

The model's reasoning, when it sends any, is shown as a muted, folded
line in the transcript. A chat's row records the model it ran on, so a
chat that predates a change of the const still says what answered it.

### One Cloudflare token, shared with R2

The app already holds a Cloudflare credential, for device sync: the R2
API token, its id on line 2 of the `bucket` file and its secret in the
keychain, filed by `superapp --r2-login`. The gateway uses **the same
token** (Andrey, 2026-09-04) — one Cloudflare API token carrying *Workers
R2 Storage Edit*, *AI Gateway Run* and *Workers AI Read* — and nothing
of its own: no file, no flag, no form, no environment variable.

What makes that true is small and sits in R2's own module. By
Cloudflare's definition an R2 token's S3 credentials are its id, as the
access key id, and **the SHA-256 of its value**, as the secret access
key. Today the keychain holds that hash, because the dashboard shows it
and `--r2-login` files what it is given, and from the hash the token
cannot be recovered. So `--r2-login` files the token's **value**
instead, under the same key `r2/{key_id}`, and `creds()` hashes it on
the way to a signature — R2 sees the credentials it sees today, computed
one line earlier. The gateway reads the same entry and sends the value
as its bearer. Line 3 of the `bucket` file and
`SUPERAPP_R2_SECRET_ACCESS_KEY` carry the value the same way, and the
bucket form's write-only secret field takes it too, which is how a
phone gets it. A secret that is already a 64-digit hash — a device
filed before this change — still signs for R2 and cannot open the
gateway; the problem row says to run `--r2-login` once more, with the
value.

Where the gateway is comes from what R2 already has as well: the
account id is the first label of the bucket's host
(`{account}.r2.cloudflarestorage.com`), and the gateway's name is one
more `const` beside the model, `superapp`, made once in the dashboard.
A scripted run never sees any of this: the fake gateway needs no token,
and the world's `Secrets` are memory.

The route is one `Provider` const, the way mail's `GOOGLE` is — the
path under the gateway URL, the model, the headers it wants — so a
second one (Cloudflare's REST route, or another provider on the same
wire) is a second const behind the same capability, not a second
module.

### No library: one small client

There is no HTTP client in this tree on purpose, and this does not add
one. `ureq`, `reqwest` and their kin bring an async runtime or a second
TLS stack, and android must build the same crate. The need is one verb,
one host, no redirects, one long streaming body — the size mail's Gmail
sign-in and device sync's R2 already hand-roll, twice. So:

- `kernel/src/http.rs`: HTTP/1.1 over the `rustls` connector the kernel
  already carries, with `webpki-roots` (as R2 does — native certs are not
  a thing on android). A request with headers and a body; a response as
  status, headers, and a **body reader** that undoes `Transfer-Encoding:
  chunked` as it goes, so a stream is read as it arrives. Timeouts on
  connect, on the first byte, and between bytes.
- `kernel/src/sse.rs`: the server-sent-events framing over that reader —
  `data:` lines to one event at a time (chat completions name no
  `event:` and end on `data: [DONE]`), multi-line data joined, UTF-8
  assembled across chunk edges (the gateway has been seen to split a
  multibyte character across two frames; the parser holds the tail
  until it completes).

Mail's `oauth::post` and R2's `send` move onto `kernel::http` after this
lands, as a separate small change; this document does not touch them.

### The `Gateway` capability

```rust
/// The model behind a chat. One implementation per world: the real
/// gateway on a window's run, the scripted fake everywhere else.
pub trait Gateway {
    /// One chat-completions request, streamed. `on` is called per chunk
    /// as it arrives and answers whether to go on, which is how *stop*
    /// cuts a stream at its next chunk; the answer is the assembled
    /// message — text, `tool_calls`, `finish_reason`, `usage` — or the
    /// failure in words. Never retries: the gateway retries, and the
    /// run's row is what a person retries from.
    fn complete(
        &mut self,
        req: &ChatRequest,
        on: &mut dyn FnMut(Chunk) -> Flow,
    ) -> Result<Message, Failure>;
}
```

`ChatRequest`, `Message`, `ToolCall`, `Chunk` and `Usage` are serde
types in `app/src/apps/agent/wire.rs`, written against the
chat-completions shape as Workers AI documents it and tested on recorded
fixtures — a text turn, a tool-call turn whose `arguments` arrive as
string fragments by index and are parsed once whole, a `length` finish,
a mid-stream error — so a change in the wire breaks a test here before
it breaks a chat.

`FakeGateway` is the scripted one: it registers under the trait and under
its own type, takes a **script** — a list of turns, each either text or a
tool use with its arguments, matched to the person's latest text by a
keyword or taken in order — and answers whole, with no clock. A test
plants a script; the e2e fake ships a default one that covers the suites
below. It is what every test, every scripted run and every library mount
gets, so no suite ever reaches a network.

### The `Complete` effect

The request is an effect: it leaves the process. It is an in-memory one,
`Effect` and not `Deferred`, the way a sync pass's reads are: **never a
row in the `effect` table**, no payload, no retry, nothing for the queue
to claim. The run's own row is its state, and the worker is what drives
it. `Complete { run }` goes through the one door all the same —
`world.run(&Complete { run })` from the worker's pass — so the effect
log's ring shows every request with its description (*ask the model for
chat 7, turn 12*) and its error, beside the mail reads. Its `perform`
reads the chat's turns off the read-only `Ctx` it is handed, builds the
request, and streams through the `Gateway` capability into the live
tail; its reply is the assembled message, and the worker writes the
turn or the calls to their rows through the store's one writer. It says
`writes: true`: a request costs money, and the log's `@wrote` view
should show it. It never retries by itself (six blind retries of a paid
request is not a policy anyone wants): a failure fails the run with the
gateway's words, and the chat offers *retry*. *Stop* is the run's
status, read by the worker before it asks and by the stream between
events.

### Workers

`App::workers(store)` answers one `Worker` per run that is `pending` or
`waiting` — `agent-run-42`, kick address `run:42` — from one cached
query, so a run starts the moment its row exists and retires when it
ends. It claims no queued job — it has none — and its pass runs
`Complete` for its run, streams into the live tail, and writes either
the turn or the calls. Under virtual time every
pass runs inline from the frame loop, so a scripted `type` + `enter` is
followed by the fake's whole answer in the same tick.

## Context: a panel in the chat

### What a panel says about itself

Two additions to the contract, both with defaults, so no existing panel
has to change on day one:

```rust
pub trait Panel: Any {
    /// What this panel is about, for an agent: one paragraph in the app's
    /// words — what the rows are, what the arguments mean, what a person
    /// does here. The default is the title and the identity.
    fn about(&self) -> String { … }
}

pub trait App {
    /// The app's data in its own words: each table, what a row is, the
    /// columns that matter, the values a column takes, and what must
    /// never be written directly (a send is an outbox row through the
    /// app's tool, not an INSERT). Read into the system prompt.
    fn describe(&self) -> Option<&'static str> { None }
}
```

Mail's `describe` is the mail chapter's data section in prose; files'
says the disk is the state and there are no tables.

### The panel chip

A panel chip is a reference, not a snapshot: the slot's identity (tag
and arguments), its title, its workspace, and the **trace** of its last
draw — the queries with their parameters, exactly what `cmd+i` copies
today. At send time the app renders it for the model:

```
<panel id="inbox" title="inbox" workspace="1">
inbox: the account's inbox, one row per conversation …   ← Panel::about
## queries
### threads — mailbox rows (inbox)
params: …
```sql
SELECT …
```
rows (50 of 143, the panel's own page):
| thread | from | subject | date | unread |
| … |
## recent effects
move uid 91 from INBOX to Archive — ok, 2 min ago
</panel>
```

The rows are **re-read at send time** through the store's cached queries,
so the agent sees what the panel shows *now*, capped at one page per
query and 32 KiB per chip with a line saying what was cut. Recent
effects are the effect log's rows for the panel's entity, newest first,
ten at most. A chip whose panel has since closed still renders — the
identity and the queries are enough to re-run — and says so.

### Getting a panel into a chat

- **`cmd+shift+a`** on any panel: opens a chat joined to it, with that
  panel's chip in the composer and the caret in the field. In a chat
  panel, the same chord adds the chip of the panel the chat is joined
  from. The chord is the workspace's, ahead of the bars, as `shift+cmd+l`
  is.
- **Paste.** `cmd+i` keeps copying the panel's context as markdown, as it
  does today, and gains one line, `superapp-panel: <tag> <args-json>` at
  the top. A paste into the composer whose text begins that way becomes a
  chip, not text; any other paste is text. The composer sees the paste
  as a paste because Makepad's text input says so (`was_paste`), so a
  typed line that happens to start with those words stays typed.
- **Drop.** A panel's header dragged onto a chat's composer adds its
  chip. Not in this change; the drop path exists and the chip does, so
  it is a small one later.

A chip reads `inbox`, `Q3 planning` (a message's title), `~/Downloads`
— the panel's title, in the section register, with a `×`. Click it to
focus the panel it points at, if it is still open. The composer holds
its chips in a row above the field; the text field stays a text field.
That is the one honest way to put chips on a Makepad text input, and it
is how most agent chats do it anyway.

`Chip` is an enum with one variant, `Panel`, and room for `File`,
`Mail`, `Selection`; each knows how to draw itself and how to render
itself for the model. Nothing else in the app matches on it.

## Tools: the agent API on the apps

### The contract

```rust
/// One thing an app lets an agent do, by name.
pub struct Tool {
    /// Stable, prefixed with the app id: `mail.archive`, `files.rename`.
    /// Never renamed once a chat has used it.
    pub name: &'static str,
    /// For the model: what it does and *when* to call it.
    pub description: &'static str,
    /// JSON Schema for `input`, `additionalProperties: false`, sent as
    /// the function's `parameters`. The arguments are checked against
    /// it on arrival — required keys, types — before `run` sees them: a
    /// model's JSON is a claim, not a promise.
    pub input: serde_json::Value,
    /// Whether the world changes. Same word as an effect's; it is what
    /// the card's look, the log, and a later gate key on. Today every
    /// call runs on arrival either way.
    pub writes: bool,
    /// The whole behaviour, on the UI thread, with the session: one
    /// `act` per call, labelled by the tool, so it is one undo.
    pub run: fn(&mut Session, &serde_json::Value) -> Result<serde_json::Value, String>,
}

pub trait App {
    /// The tools this app offers an agent. Collected into one list at
    /// boot; two apps offering one name stop the process, naming both.
    fn tools(&self) -> Vec<Tool> { Vec::new() }
}
```

`Apps::tools()` is the list the request carries, in app-list order, and
the registry the chat runs a call by name from. A call for a tool no app
in this build has fails with `no such tool in this build`, which is what
the model reads.

### The kernel's own tools

Listed first, because every build has them. These are the *direct access
to sqlite* — read and write — and the workspace itself:

| Tool | Writes | What it does |
|---|---|---|
| `sql.query` | no | one statement on the store's reader, which is read-only by construction, parameters as JSON, rows as JSON, 200 rows and 64 KiB at most. Runs on the worker: it needs no session |
| `sql.write` | yes | one statement or a batch, in one transaction on the writer. The session extension records the transaction's changeset; its inverse is the node's `Intent`, so `cmd+z` puts the rows back. The kernel's own tables (`meta`, `workspace`, `panel`, `effect`, `repl_*`) are refused by name |
| `sql.schema` | no | the store's `sqlite_master`, plus each app's `describe`: the data dictionary on demand, so the system prompt can carry a summary and the model can ask for the rest |
| `panels.list` | no | what is open: every slot's identity, title, workspace, focus, and joins — the workspace as context |
| `panels.context` | no | one open panel's chip text, by slot — what the person is looking at, without a paste |
| `panels.open` | no | opens a panel by tag and arguments, joined to the chat, focus staying in the chat: a layout action, undoable, changing no data |

`sql.write` is the one tool that cannot promise the apps' invariants (a
mail row inserted by hand reaches no server). The system prompt says so
and lists what each app wants done through its tools instead; the
`describe` texts say it again per table. It is offered anyway, because
the person asked for it and because undo covers it.

### The apps' tools, first cut

Mail: `mail.search` (the FTS the launcher uses), `mail.thread` (one
conversation, letters as text), `mail.archive`, `mail.delete`,
`mail.not_spam`, `mail.read` / `mail.unread`, `mail.draft` (a compose
row, opened as a panel for the person to look at — the agent never sends
what nobody read), `mail.send` (a draft by id). Files: `files.list`,
`files.read` (text, 64 KiB), `files.rename`, `files.move`, `files.copy`,
`files.trash`, `files.mkdir`, `files.write`. The system app:
`problems.list`, `effects.recent`.

Each tool is the verb's own code path over ids instead of over a cursor:
mail's tools call the filing that *archive* calls, files' call the ops
that *rename* calls, so a tool and a verb cannot disagree, and undo works
the same for both because it is the same action.

### Running a call

Every call runs as soon as it arrives; nothing asks first. A call is a
card in the transcript: a writing tool's card says what it did in a
readable sentence (*renamed `~/notes.md` to `~/notes-2026.md`*) — the
same sentence is the action's label, which is what the history shows and
what `cmd+z` takes back; a reading tool's card is a folded line, the
arguments and a size, opened by a click. A card that failed says so in
the colour errors get. There is no allow-list per tool and no policy
file: undo is the safety net, and it is one chord away for as long as
the process lives.

A device that may not write (the sync lease is elsewhere) refuses every
writing call with the same toast a verb gets, as the error the model
reads; the run goes on, and the model says what it could not do.

### What the model is told

The system prompt, in this order: what superapp is and the person's
name from the account; that
the store is SQLite, the reader is `sql.query`, the writer is
`sql.write`, and the apps' tools are preferred wherever one exists; each
app's `describe`; the panel that is in context, if any; and a short
style: answer in the chat's language, name rows by what a person calls
them (a conversation, a file), say what was done and what can be undone.
No mention of chords, no ASCII art of the workspace.

The `tools` array follows as `function` definitions, then the turns.
The prompt contains nothing that changes per request (no clock, no
counts), so whatever the model and the gateway cache of a repeated
prefix, they cache without being asked; this wire has no `cache_control`
to place. `usage` comes on the stream's last chunk, is kept on the run,
and is shown as a muted line under the agent's turn: *2.1k in (1.9k
cached), 310 out*.

## The surface

### Panels

| Tag | Argument | What it shows |
|---|---|---|
| `agents` | none | the chats, a rich table: title, model, when, what the run is doing |
| `chat` | a chat id | one conversation: the transcript, the composer |

Roots, in this order: **agents**, **new chat**. The
app is listed after files and before `system`, so its roots close the
apps' and lead help.

### The chat panel

A transcript above, the composer below. The transcript is the person's
turns right-aligned in the ink colour on the paper, the agent's
left-aligned; chips in the person's turns; reasoning as one
folded muted line; a tool call as its card; the live tail streaming into
the last turn, with a cursor block at its end. A long transcript is a
`PortalList` of turns, so a thousand-turn chat costs what it shows.
Everything the agent wrote is a selectable run (`SText`), because an
answer is something one copies out.

The composer is the chip row over a multi-line field, the way compose's
body is a field. `enter` sends; `shift+enter` is a newline. While a run
is going, `enter` is disabled and the bar says so.

The bar: **send** (`cmd+s`), **stop** while a run is going (`cmd+k`),
**retry** on a failed run (`cmd+r`), **new** (`cmd+n`, a fresh chat,
un-joined), and the link *agents* (dotted, replaces). No letter of the
workspace's, none twice.

The title is the chat's: the first line of the first turn until the
person renames it, `chat` before that. `wish` is 6×6 on the desktop and
the whole grid on a phone.

### The agents list

A rich table over `agent_chat`, newest first: the title, the model, the
last activity, and the run's word — *streaming*, *waiting* (a call the
chat will run when it is next shown), *failed* in the ink colour — so
the list is where one finds the chat that stopped short. Tags
`@waiting`, `@failed`, `@model:`, `@date`; text matches the
title and the turns. The cursor previews the chat beside the list.
Marked: **delete n** (`cmd+d`, the chat and its turns, one node).

### The phone

The chat fills the grid; the composer sits above the soft keyboard, the
transcript shortens. `cmd+shift+a` has no glass equivalent yet — a long
press on a header is already *pick the panel up* — and joins the list of
gestures the glass has no word for; the chat's own *add panel* button
(the launcher's list of open panels, filtered to one pick) is the
phone's way.

## Failure and problems

- **No token, or the old hash**: a send with no Cloudflare token to be
  found fails at once, and the `agent` problem source lists *gateway:
  no cloudflare token* with the places it looked — *run `superapp
  --r2-login`, set `SUPERAPP_R2_SECRET_ACCESS_KEY`, or put it on line 3
  of the `bucket` file* — in R2's own sentences; a keychain that holds
  the S3 hash from before this change gets *run `superapp --r2-login`
  again, with the token's value*. Filing it clears the row, since a
  problem is derived, never stored. A device with no bucket has no
  account to ask and gets the same row.
- **The gateway refuses** (401, 403, a bad account id): the run fails
  with the gateway's sentence, and the same source lists *gateway:
  unauthorized* with it while the latest run stands failed that way;
  the next run that answers clears it.
- **The network is down**: the run fails with *retry* in the bar; nothing
  retries by itself.
- **The model stops on a filter** (`finish_reason: content_filter`):
  the turn says so, muted; the chat goes on.
- **`finish_reason: length`**: the turn is cut, marked *cut short*, and
  *continue* is offered as a verb that asks for the rest.
- **A call fails** (the tool's error): the error is the `tool` message's
  content, shown on the card in the colour errors get; the model reads
  it.
- **A tool the build lacks**: same, in words.
- **The process dies mid-run**: on the next open the run is `streaming`
  with no worker; the ladder's open sweep marks such runs `failed:
  interrupted` and the chat offers *retry*. Nothing re-sends a paid
  request by guesswork: there is no job to come back to the queue, and
  the sweep runs before any worker is asked for.

## Testing

- **Unit**, no window: the wire types on fixtures; the SSE parser on
  split frames and split characters; the HTTP reader on chunked bodies;
  the changeset inverse restoring rows; the chip renderer on a fake
  session with a mailbox open; each tool on `Session::fake` (rename a
  file, archive a thread, undo, look); the fake gateway's script
  matching; the app's bars for letters.
- **e2e**, `e2e/agent/`, on the fake's default script: `basic` (new
  chat, a question, the answer as a label), `chip` (`cmd+shift+a` on the
  inbox, the chip's title, send, the answer names the panel), `paste`
  (`cmd+i` on a message, paste into the composer, a chip not text — the
  grammar gains `paste "…"`, a `TextInput` with `was_paste`), `tool`
  (ask for a rename, the card, the file's new name in the listing,
  `cmd+z`, the old name), `stop`, `phone` on `4x3`. The token's two
  readings — the hash for R2 off the value, the value for the gateway —
  the account id off the bucket's host, and the sentence for each thing
  missing are held by unit tests beside R2's, not by a suite: a scripted
  run has the fake and no gateway to find.
- **Scenes**, in the panels library: the chat empty, streaming, with a
  call done, with a call failed, failed; the chip; the agents list; each
  as a still off the fake.

## The book

A new chapter, `docs/book/src/agents.md`, under *Apps*, written from the
sections above minus the plan. Elsewhere:

- `apps.md`: `Panel::about`, `App::describe`, `App::tools`, and the
  kernel's own tools, in the contract's table.
- `vocabulary.md`: agent, chat, turn, chip, run, tool, call, gateway.
- `interaction-grammar.md`: `cmd+shift+a` among the reserved chords;
  the paste rule for a context chip.
- `data-substrate.md`: the changeset-inverse intent beside the other
  intents; the live tail beside the effect ring.
- `dev-x.md`: the fake gateway and its script; the app's environment
  knobs; `paste` in the script grammar.
- `tech-stack.md`: `kernel::http` and `kernel::sse`, and why there is
  still no HTTP client.
- `device-sync.md`: `--r2-login` takes the token's value, the S3 secret
  is its hash, and the gateway shares the token.
- `open-questions.md`: what is left below.

## Phases

Each phase is one PR that leaves `main` green and the app usable as far
as it goes.

0. **The floor.** `kernel::http`, `kernel::sse`, the wire types, the
   `Gateway` trait, `FakeGateway`, the real gateway behind a `Provider`
   const; `Panel::about`, `App::describe`, `App::tools`, `Tool`,
   `Apps::tools`; unit tests. Nothing draws yet. Done when `cargo test`
   proves a tool-call turn round-trips through the fake and a recorded
   fixture, and a real request from a test binary answers — which is
   also where the exact route is settled.
1. **The chat.** The app: schema, the three panels and their templates,
   the worker and the in-memory `Complete` effect, streaming into the
   live tail, the token shared with R2 — `--r2-login` filing the value,
   `creds()` hashing it, the account off the bucket's host — the problem
   source, the request's line in the effect log. No chips, no tools. Done when `e2e/agent/basic`
   passes and a windowed run holds a conversation through a real
   gateway.
2. **Context.** `Chip::Panel`, the chip row, the renderer with live
   rows and recent effects, `cmd+shift+a`, the paste rule and the
   `superapp-panel:` line on `cmd+i`, `paste` in the e2e grammar,
   `about` on every existing panel kind and `describe` on mail. Done
   when `chip` and `paste` pass.
3. **Tools.** The kernel's six, the changeset inverse, the cards,
   mail's and files' tools over their verbs' code paths. Done when
   `tool` and `stop` pass and an
   agent archives a conversation the person is looking at, undone by
   `cmd+z`.
4. **The finish.** The usage line, the phone
   layout and its *add panel* button, the scenes, the book chapter and
   the edits above, this document deleted.

## To decide in review

- **`cmd+shift+a` reserves a chord, not a letter.** The shell today
  keeps `l` whole because of `shift+cmd+l`; keeping `a` whole would take
  *archive n* from every mailbox and select-all from every field. The
  proposal: the workspace's shifted chords are reserved as chords, bars
  keep wearing plain letters, and the bold-letter promise is unchanged
  since bars only ever promise plain ones. If that rule is not wanted,
  the chord moves — `cmd+shift+g` (*go ask*) is free.
- **Where a call runs.** The chat panel's widget runs calls on the UI
  thread, so a hidden chat pauses its run. The alternative is a kernel
  hook, `App::tick(&mut Session)`, run every frame the store changed; it
  is generic and small, but it is the first thing the kernel would
  schedule for an app. The proposal starts on the panel.
- **`sql.write` at all.** It is offered because it was asked for, undo
  covers its rows, and the apps' tools are preferred in the prompt. The
  case against is that a model given a writer uses it; the case for is
  that this is one person's store and the changeset comes back on
  `cmd+z`. If the inverse turns out costly to keep under the session
  extension's rules (tables without a primary key record nothing), phase
  3 ships `sql.write` refusing such tables by name, and the node expires
  rather than lies.
- **The account id.** Read off the bucket's host, so a device without
  a bucket has no gateway; or one more const beside the gateway's name.
  Off the host is proposed: the store the agent reads is the synced one
  anyway.
- **Re-filing the token.** Devices already configured for R2 hold the
  hash and run `--r2-login` once more with the token's value. Accepting
  both forms — hashing only what is not already a hash — would spare
  that, at the cost of judging a secret by its shape; asking once is
  proposed.
- **The turn as wire JSON.** Stored verbatim for fidelity; the cost is
  that a wire change is a `Step::Derived` walk over old turns. The
  alternative, an app-shaped row per block, costs a mapping both ways
  now. Verbatim is proposed.

## Not done, on purpose

- **Asking before a call.** Every call runs on arrival, and undo is the
  net (Andrey, 2026-09-04: allow all tool calls without asking, for
  now). A gate — a card that waits, *allow*, *refuse*, *allow all* for
  the run — is the obvious next step once it is clear which tools want
  one; `agent_call.status` has the room for `asked` and `refused`, and
  `Tool::writes` is what it would key on.

- **Agent profiles** — named system prompts and tool subsets (*triage*,
  *files*). The schema has a `model` on the chat and nothing else; a
  `profile` column is a later ladder step.
- **Other chips** — file, letter, selection. The enum has the room.
- **Web search and fetch** — tools of the app's own over `kernel::http`,
  when wanted; a card draws their result as text like any other.
- **Compaction and context editing** for long chats; a chat that fills
  the window is a new chat for now, and the *new* verb is one letter.
- **Another model, or Anthropic-native.** The model is a const: another
  Workers AI model is a one-line change, another provider on the same
  wire is a second `Provider` const, and an Anthropic-native `Gateway` —
  the Messages API with its caching and thinking — is a second
  implementation of the capability, if a model ever needs it.
- **Voice, images in turns, files as attachments** — a `Chip::File`
  rendered as a content part is the path, later.
- **Scheduled agents** — a run nobody asked for in a chat. Workers can
  do it; the question is what a person sees, and that is its own CR.
