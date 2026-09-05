# Agents

The agent app is a chat over the store, with the apps as its hands. It
registers two panel kinds, its own schema ladder, one capability, one
in-memory effect, one problem source, one worker per round in flight, and two
launcher roots. It stores no secret and adds no dependency.

One rule holds the whole of it up: **an agent acts only through what a person
could do, and every one of its acts is an ordinary undoable action.** A tool is
the verb's own code path over ids instead of over a cursor, so a tool and the
button beside it cannot disagree; `cmd+z` is the answer to a call that should
not have run, and only the handful of calls undo cannot take back — a send, a
delete, a raw write — [ask first](#the-gate-what-asks-first).

## Tags and roots

| Tag | Argument | What it shows |
|---|---|---|
| `chat` | a chat id, or `new` | one conversation: the transcript and the composer |
| `agents` | none | every chat, as a list |

Two roots: **agents** and **new chat**. The app is listed after files and
before `system`, so its roots close the apps' and lead the shell's own.

A `chat(new)` panel has no row behind it yet. The first send makes the
conversation and replaces the slot with `chat(7)` in the same node, so the slot
and the saved session name the real chat rather than the blank one it started
as.

## The words

- **Agent**: what answers in a chat — a model behind the gateway, a system
  prompt, and the tools this build offers it. One agent, the assistant.
- **Chat**: one conversation, a row and a panel. Its **turns** are the
  messages, a person's and the agent's, and a turn may carry **chips**.
- **Chip**: a piece of context put into a chat as a thing rather than as
  text — a panel, today. It is rendered into text for the model at send time.
- **Run**: one round of the agent working on a chat: a request to the gateway,
  the tool calls it asks for, their results, until it stops. A run is a row and
  a worker drives it.
- **Tool**: one thing an app lets an agent do, by name, with a JSON schema. A
  **call** is one use of a tool inside a run — a row, so the chat panel can run
  it on the UI thread and the transcript can show it.
- **Gateway**: where requests go — a Cloudflare AI Gateway in front of the
  model.

## The store is the bus

A run has three parties on three threads: the person in the chat panel on the
UI thread, the worker talking to the gateway in its own world, and the one
writer thread every write goes through. They meet in rows, the way a mailbox
and its sync pass already do.

1. **A person sends.** The chat panel files one action: the conversation if
   there is not one yet, a `run` row, and the person's turn, in one
   transaction. `Session::act` kicks the workers, which is what starts it.
2. **The worker asks.** It builds the request from the chat's turns, streams
   the reply into the live tail, and writes the agent's turn as a row when the
   answer is whole.
3. **The model asks for a tool.** The worker writes one `call` row per entry of
   `tool_calls`, sets the run to *waiting*, and sleeps on a kick.
4. **The tools run on the UI thread.** The chat panel runs every pending call
   through the session — one history node per call, wearing the tool's own
   label — writes each result to its row, and kicks the worker.
5. **Back to 2**, until the model stops, the run fails, or the person stops it.

The chat panel is the run's hands: a run whose chat is shown nowhere pauses at
its next call and picks up when the chat is opened again, and the agents list
is what says which runs are waiting. Nothing in the kernel schedules anything
for this app.

### The tables

All prefixed `agent_`, all with an `INTEGER PRIMARY KEY`, because device sync
records a table by its primary key and a table without one replicates nothing:
a chat continued on the phone is the same chat only if its turns travel. A
run's key is `AUTOINCREMENT` as well — the ladder's third rung rebuilds the
table to make it so, carrying every row across under its own id. A plain
`INTEGER PRIMARY KEY` is `rowid`, and SQLite hands the highest deleted one out
again: undo a send while its run is streaming, redo it, and the fresh run takes
the id the old one had, so the worker still inside the gateway finds a run very
much alive and appends its answer to the round that replaced it. `AUTOINCREMENT`
is the whole of the fix; `sqlite_sequence` is SQLite's own and replication skips
every `sqlite_*` name, so no device receives another's counter. A new rung goes
at the **foot** of the ladder, after the sweep, because the sweep holds a place
on it like any other rung and the counter records it.

| Table | What a row is |
|---|---|
| `agent_chat` | one conversation: `title`, `model`, `created`, `updated` |
| `agent_turn` | one message: `chat`, `seq`, `role`, `body`, the `run` that wrote it, `created` |
| `agent_run` | one round: `chat`, `status`, `error`, `started`, `ended`, `usage` |
| `agent_call` | one tool call: `run`, `turn`, `tool_call_id`, `tool`, `input`, `status`, `output`, `label`, `created`, `ended` |

A run's status is `pending`, `streaming`, `waiting`, `done`, `failed`, or
`stopped`; a call's is `pending`, `asked` — waiting for the person's word —
`done`, `failed`, `refused`, or `cancelled`, the word for one a stop caught
before anybody had run or refused it. A turn's `body` is the wire's own message,
stored verbatim as text, because the next request is built from the rows and
`sqlite3` can still read them; the app's own two keys ride in
the same object — `chips`, the context the turn carried, and `finish`, the word
the model stopped on. The role is a column as well as a key of the JSON, so a
query can ask without parsing.

Nothing about the gateway is a row. Its token is device sync's, and a secret
never goes in the store: it is the one thing that must not replicate.

### The workers

`App::workers` answers one worker per **live** run — `pending`, `streaming` or
`waiting` — `agent-run-42`, kick address `run:42`, from one cached query, so a
run starts the moment its row exists and retires when it ends. It claims no
queued [job](./data-substrate.md#queued-jobs): it has none. A pending run is
asked; a waiting one is asked again once no call of the round is `pending` or
`asked` — a call the person refused counts as answered, because a refusal is
what the model reads back.

`streaming` is in that list because the set is diffed after **every** action:
any write at all while the gateway streams would otherwise retire the very
worker reading it, closing the kick channel the round is woken through when its
calls come back. A run keeps its worker for as long as it is going. A *second*
worker that finds a run already `streaming` re-sends nothing and waits on a
kick — the pass that is asking has not returned yet, a request costs money, and
the only honest word about a stream nobody is holding is the sweep's at the next
open. For the same reason a written call result asks for the whole set again
rather than knocking at one address: a worker that is missing has to be asked
for, not woken.

Every write a worker makes checks, inside its own transaction, that the run it
is writing for is still there and still its chat's. A worker is inside the
gateway for as long as an answer takes, and an undo or a *delete* on the UI
thread can take its rows away while it is; without the check the answer lands
anyway, as a turn for a run that no longer exists in a chat that may not either.

A device that may not write runs **none of them**. A run row replicates like
any other, so the follower would otherwise start a second worker for a round
the lease holder is already paying for.

### What a crash leaves

A run that says `streaming` has an answer nobody is holding, no job in the
queue, and no worker coming back for it. The ladder's `Step::Always` sweep
marks such runs `failed: interrupted` at every store open, before any worker is
asked for; the chat offers *retry*. Nothing re-sends a paid request by
guesswork. `pending` and `waiting` are left where they are, because both are
resumable.

### Undo

*send*, *stop*, *retry* and *delete* are each one undoable action, and each says
what it undoes to.

Undoing a **send** is *the chat as it was before you sent it*: the turn goes,
and everything that came after it goes with it, since an answer to a question
nobody asked is not worth keeping — and a run still in flight is told to stop
first, in a write of its own, so the worker mid-stream on another thread
actually sees it. Redo files the turn again with a fresh pending run, which the
kick sets going.

A **stop** is a node that refuses. The request is gone: the stream was cut, the
gateway has been paid, and there is no asking for the rest of an answer it
already sent — so its claim answers *a stopped round cannot be resumed — retry
asks again*, the way mail's send refuses once the letter has left. `cmd+z` walks
transparently past it to the send underneath and says what it really undid. The
node stays in the tree, because a person who pressed *stop* did something.

Undoing a **retry** is the send's rule with the question left out: the run it
filed goes, and the turns and calls that run produced go with it; redo files a
fresh pending run under an id of its own.

Deleting chats keeps their rows on the node, so undo puts the turns, runs and
calls back under the ids they were named by — except that a run which was
`streaming` when the chat went comes back **stopped**. Nobody owns that stream
any more; the chat offers *retry*. `pending` and `waiting` come back as they
were, because the walk's own kick asks for their workers again.

One more thing happens before any new request: the round the last one left open
is **settled**. A stop lands between a round's calls being written and their
results being written, and nothing picks it back up — so the assistant turn with
its `tool_calls` would still be there with no `tool` turns behind it, which is
invalid on the wire, and the results of the calls that did run would be lost
from the model's context. So *send*, *retry* and *continue* each write, inside
their own transaction, one `tool` turn per call of that round: what a call that
ran came to, and for one nobody got a word about, `cancelled: the round was
stopped`.

## The gateway

The model is `@cf/zai-org/glm-5.3-flash`, one of the models Cloudflare hosts
itself, which the gateway reaches as one more provider, `workers-ai`. It is a
`const` in the app, with the reasoning effort — `medium` — beside it; changing
either is a commit, not a setting. A chat's row records the model it ran on, so
a chat that predates a change still says what answered it.

Requests speak the **OpenAI-compatible chat-completions API**, the one shape
Workers AI and every provider behind the gateway share, streamed as
server-sent events:

```
POST https://gateway.ai.cloudflare.com/v1/{account}/superapp/workers-ai/v1/chat/completions
authorization: Bearer {cloudflare api token}
cf-aig-authorization: Bearer {the same token}
cf-aig-collect-log-payload: false
```

One token opens both doors: it is the provider's key and the gateway's at once,
so there is nothing to store in the gateway and nothing to alias. Where the
requests go is one `Provider` const — the path, the model, the reasoning
effort; a second route, or another provider on the same wire, is a second const
behind the same capability rather than a second module. The gateway itself is
used for what it is good at — logs, analytics, rate limits, retries — and all
of that is set up in its dashboard, not here.

`cf-aig-collect-log-payload: false` is the app's own policy: a chat carries the
person's mail, and the gateway keeps counts and status, not prose.
`SUPERAPP_AGENT_LOG_PAYLOAD=1` leaves the header off and lets the gateway's own
setting decide. That is the app's one environment knob, read by itself, because
argv belongs to the shell.

Not the Anthropic Messages API and none of its extras: the model is not
Anthropic's, and the chat-completions shape carries what this needs — tools as
`function` definitions, `tool_calls` on the agent's message, a `tool` message
per result, `finish_reason`, `usage` on the last chunk, and the model's
reasoning as `reasoning_content` where it sends any.

### One Cloudflare token, shared with R2

The gateway has no credential of its own: no file, no flag, no form, no
environment variable. It uses the token
[device sync](./device-sync.md#the-bucket-form) already holds — one Cloudflare
API token carrying *Workers R2 Storage Edit*, *AI Gateway Run* and *Workers AI
Read*.

That works because an R2 token's S3 secret access key is, by Cloudflare's own
definition, the SHA-256 of the token's value. So what is filed is the
**value**, and R2 hashes it on the way to a signature; the gateway reads the
same entry and bears it whole. Where the gateway is comes out of R2 when there
is one: the account is the first label of the bucket's host — `--bucket`,
`SUPERAPP_BUCKET`, or the `bucket` file, in that order — and the gateway's
name is one more const, `superapp`, made once in the dashboard. A device with
no bucket on R2 asks Cloudflare whose token it holds instead, once per process
(`GET /accounts` with the same token), and a token that opens more than one
account is told to name one with `--bucket`. The key id the token is filed
under is remembered by `--r2-login` in the secret store, so a laptop that never
joined a bucket needs neither a file nor an environment variable to find it.

A device configured before this existed filed the hash the dashboard showed it,
and from a hash no token can be recovered. Such a secret is recognised by its
shape — 64 hex digits, which a 40-character Cloudflare token can never be — so
that device keeps syncing on what it holds, and only the gateway asks for
anything: *the secret for … is the S3 hash, not the token — run `superapp
--r2-login` again, with the token's value*.

A scripted run sees none of this: the fake gateway needs no token, and the
world's secrets are memory.

### No library: one small client

There is no HTTP client in this tree on purpose, and this did not add one. The
need is one verb, one host, no redirects, one long streaming body — so
[`kernel::http`](./tech-stack.md#still-no-http-client) is HTTP/1.1 over
`rustls`, verified against the Mozilla roots rather than the machine's,
because a phone has no machine roots to verify against; its body undoes
`Transfer-Encoding: chunked` as it arrives, and `kernel::sse` is the event
framing over that reader. Both are the kernel's, and both are driven by tests
over an in-memory cursor, so the wire's edge cases are pinned without a
network.

## Context: a panel in a chat

### What a panel says about itself

Two additions to the [contract](./apps.md#what-an-app-registers), both with
defaults, so no existing panel had to change:

- `Panel::about` is what this panel is about, for an agent: one paragraph in
  the app's words — what the rows are, what the arguments mean, what a person
  does here. The default is the title and the identity. Every panel kind in
  this build answers it, `Missing` included, which says that no app here owns
  the tag and that its arguments mean whatever the build that has it means.
- `App::describe` is the app's data in its own words: each table, what a row
  is, the columns that matter, the values a column takes, and what must never
  be written directly. Mail's is the data half of the [mail chapter](./mail.md)
  in prose, down to *a send is an `outbox` row filed through mail's own send,
  never an INSERT*; files' is the shortest one there can be, because the disk
  is the state and there are no tables at all. It is read into the system
  prompt, so it is prose rather than a schema dump: the schema is one tool call
  away.

### The chip

A panel chip is a **reference, not a snapshot**: it keeps the slot's identity,
its title, its workspace, the panel's own paragraph, and the trace of the draw
it was made from. The rows come off the store when the request is built, so a
chip made an hour ago carries what the panel shows now, and a chip whose panel
has since closed still renders — the identity is enough to run the queries
again, and where nothing is showing it, the chip says so.

Rendered for the model, a chip is one block:

````
<panel id="inbox" title="inbox" workspace="1">
inbox: the account's inbox, one row per conversation …
## queries
### threads — mailbox rows (inbox)
params: …
```sql
SELECT …
```
| thread | from | subject | date | unread |
| --- | --- | --- | --- | --- |
| … |
rows (50 of 143, the panel's own page)
## recent effects
move uid 91 from INBOX to Archive — done, aug 30 14:22
</panel>
````

The rows are re-read at render time off the store's own reader, bound with the
values the draw bound: at most 50 a query, 200 characters a cell, 32 KiB a
chip, with a line saying what was cut. The recent effects are the [effect
log](./data-substrate.md#effects-and-job-panels)'s newest ten for this panel's
arguments, and only what **wrote** — the same narrowing the log panel opens
on, because a chip full of *connect*, *select*, *fetch* tells a model nothing
about what happened to the thing it is looking at. A panel with no arguments
stands for no one thing, so it gets none.

### Getting a panel into a chat

- **`cmd+shift+a`** on any panel offers the focused slot to the apps in list
  order and stops at the first taker, which answers a panel with a chat joined
  to it. The chord is the workspace's, ahead of the bars, as `shift+cmd+l` and
  `shift+cmd+s` are; the macOS menu offers the same move as *Ask About This
  Panel*. A build with nothing that takes a panel says so rather than doing
  nothing.
- **Paste.** `cmd+i` copies the focused panel's context as markdown, as it
  always has, and now leads it with one line: `superapp-panel: message ["42"]`.
  A paste into the composer whose **first line** is that becomes a chip; any
  other paste is text. The composer knows a paste is a paste because Makepad's
  text input says so, so a typed line that happens to start with those words
  stays typed.
- **Drop.** A panel's header dragged onto a composer is the obvious third way
  and is not built. The drag path exists and so does the chip.

A chip reads as the panel's title — `inbox`, `Q3 planning`, `~/Downloads` —
and knows which slot still shows it, so a click can focus it. `Chip` is an enum
with one variant, `Panel`, and room for `File`, `Mail` and `Selection`: each
variant renders itself, which is why nothing else in the app matches on one.
Only the identity, the title, the workspace and the paragraph are written into
a turn; the trace is not, because a chip points at a panel and not at a moment.

## Tools: the agent API on the apps

### The contract

A `Tool` is a stable name prefixed with the app id, a description written for
the model — what it does and *when* to call it — a JSON Schema for its input,
a `writes` flag in the same word an effect uses, an `asks` flag for the few
whose call [waits for the person](#the-gate-what-asks-first), and the
behaviour itself: a function of the session, run on the UI thread, filing one
action labelled by the tool so that it is one undo.

`Apps::tools()` is the list a request carries and the registry a call is run by
name from: the kernel's own first, then each app's in app-list order. Two apps
offering one name stop the process at boot, naming both. A call for a tool no
app in this build has fails with *no such tool in this build*, which is what
the model reads.

The schema is the other half of the contract. A model's arguments are a claim,
not a promise, so they are read against it before `run` sees them: every
`required` key present, each declared property of the type it was promised
(`integer` means a whole number; a union takes either; an `enum` takes only
what it lists), objects and arrays the same again inside, and, under
`additionalProperties: false`, no key nobody declared. The refusal is a
sentence the model can act on — *missing `to`*, *`path` must be a string*,
*unknown key `foo`*. It is a gate on the obvious mistakes and not a validator:
a schema that says nothing about a key says nothing about it here either.

### The kernel's own

Listed first, because every build has them, whatever apps it was given. These
are direct access to SQLite, read and written, and the workspace itself.

| Tool | Writes | What it does |
|---|---|---|
| `sql.query` | no | one read-only statement on the store's reader, which is query-only by construction, parameters as JSON, rows as JSON, at most 200 rows and 64 KiB |
| `sql.write` | yes | one statement with its parameters, or a batch of statements that binds none, in one transaction, as one undoable action |
| `sql.schema` | no | the store's tables and indexes with the SQL that made them — the kernel's own bookkeeping and a virtual table's shadows left out — plus each app's `describe`: the data dictionary on demand |
| `panels.list` | no | every panel open on every workspace: tag, arguments, title, workspace, which has focus, and what each is joined to |
| `panels.context` | no | one open panel's chip text, by slot — what the person is looking at, without a paste |
| `panels.open` | no | opens a panel at the end of the focused panel's joined chain, so what is open stays open, focus staying where it is: the same preview a cursor walk makes, and undoable the same way; a panel already open there is answered, not opened twice |

`sql.write` is the one tool that cannot promise the apps' invariants — a
`message` row moved by hand reaches no server — so the system prompt says to
prefer an app's own tool wherever one exists, and each `describe` says it again
per table. It is offered anyway, because it was asked for and because undo
covers it. What it will not do is decided before the transaction, off the
reader, and then asked of every statement SQLite prepares:

- **the kernel's own tables**, by name: `meta`, `workspace`, `ws_col`,
  `panel`, `wm`, `effect`, replication's log, and SQLite's own catalogue. A
  model that rewrites the window it is talking through has broken it;
- **a table with no primary key**, because the session extension records
  nothing for one and the undo would lie;
- **a table this store did not have when the call began**, for the same
  reason: what a `CREATE TABLE` in the same batch makes is outside the set the
  writer's session is attached to;
- **the shape of a table**, whosever it is: a name and its columns belong to
  the app's schema ladder, which is a commit and a migration, not a call;
- **transaction control** — `BEGIN`, `COMMIT`, `END`, `ROLLBACK`, `SAVEPOINT`,
  `RELEASE`. A call is one transaction and the session owns it: a `COMMIT`
  halfway through a batch ends that transaction under it, so the statements
  before it stay written while the node, the changeset and replication's capture
  record none of them.

The authorizer that says all this stands only for the length of the call: the
writer is one connection for the whole process, and one left in place would
refuse the next action anybody filed.

What it claims is the changeset the transaction recorded. Every other claim in
this tree is an app's own sentence about its own rows, because the app knows
what it did; a tool that ran a person's `UPDATE` knows only which rows moved,
so the inverse of the changeset is the whole of what it can promise. See
[Data and Effects](./data-substrate.md#the-changesets-inverse).

### The apps'

Each one is the verb's own code path over ids instead of over a cursor: mail's
filing goes through the same write the *archive* button files and claims the
same intents, files' verbs go through the same disk operations, so a tool and a
button cannot disagree, and undo works the same for both because it is the same
action.

| App | Reads | Writes |
|---|---|---|
| mail | `mail.search`, `mail.thread` | `mail.archive`, `mail.delete`, `mail.not_spam`, `mail.read`, `mail.unread`, `mail.draft`, `mail.send` |
| files | `files.list`, `files.read` | `files.rename`, `files.move`, `files.copy`, `files.trash`, `files.mkdir`, `files.write` |
| system | `problems.list`, `effects.recent` | — |

A conversation is named by any of its letters, because every query in mail
resolves the thread from the id it is handed, and `mail.search` answers the
conversation each letter belongs to. One rule the agent does not get to break:
**it never sends what nobody read.** `mail.draft` writes the letter and opens
it in a compose panel for the person to look at; `mail.send` takes only a draft
that already exists, and the send's own window is what `cmd+z` is for.

The files tools carry the app's own refusals with them: nothing is removed
outright, a trash is a trash, a destination that exists is refused rather than
written over, and `files.write` refuses a file over a megabyte — and a text over
a megabyte — because what it would have to keep for undo is memory, both what
was there and what it put there, the second being how a reversal tells that the
file on disk is still the one it wrote. The system app changes nothing, so it
offers nothing that writes: its two tools are the problems list and the effect
log answered as rows, so a chat can say *the send to Vera failed, here is why*
without the person going to look.

### Running a call

Nearly every call runs as soon as it arrives, because undo is the net. Three
refusals come before the tool, each a sentence the model can act on: a name no
app in this build offers, arguments the schema will not have, and a writing
tool on a device that may not write — which gets the same words a person's own
verb gets, as the error the model reads. The run goes on either way, and the
model says what it could not do.

The call's row takes `done` or `failed` and the text the model reads back; the
tool's own action is what the history shows and what `cmd+z` takes back, and
the bookkeeping around it is no node at all.

A writing tool files exactly one node, and that node's label is the sentence
the history shows — *rename “README.txt” to “readme-renamed.txt”*. It is read
off the head of the history the moment the tool returns, where the head moved,
and kept on the call's `label`, which is the one thing the card says. The model
never sees it: what the model reads back is the tool's own JSON. So the card
and the undo tree say one thing in one voice, and neither quotes the other.

### The gate: what asks first

Five calls in this build do not run on arrival: `sql.write`, `mail.send`,
`mail.delete`, `files.trash` and `files.write`. What they have in common is
what `cmd+z` cannot honestly promise — a letter that has gone has left the
machine, a file written over is memory, a statement nobody's app is speaking
for. `Tool::asks` is the flag, and it is a flag of its own rather than
`writes`, because a rename, an archive or a mark-read is one undo away and a
gate on all of them would be a dialog box on everything.

Such a call becomes a **card that waits**: the tool and what the model wrote
for it on its line, *waiting for you* under it, and two buttons — **allow** and
**refuse** — drawn as the bar draws its own. The chat's bar wears the same two
words while it waits, *refuse* on `f` and *allow* on no letter at all, because
every letter of that word is a chord the workspace or the composer already
keeps.

**The walk stops there.** The calls the model asked for after it stay
`pending` until this one is answered, since order can matter — a draft before
its send. *allow* runs it exactly as an arriving call would have run, then goes
on to the next; *refuse* never runs it, and *refused by the person* is the
`tool` message's content, so the model reads what it was not allowed to do and
says so. A refusal is an answer, not a hang: the round is settled when no call
of it is `pending` or `asked`, and the run goes round again either way. A stop
while a card is waiting is an answer too, written when the next request is:
the call becomes `cancelled` and the model reads *cancelled: the round was
stopped*.

*allow all for this run* is [not built](#not-done-on-purpose).

### What the model is told

The system prompt, in this order: what superapp is — one person's workspace,
where everything is panels over a single SQLite database on their own machine;
that `sql.query` reads it and `sql.write` writes it, and that an app's own tool
is preferred wherever there is one, because a tool is the same code the
person's own button runs; that every act is an ordinary undoable action, so
most calls simply run, and that the few that cannot be undone wait for the
person's word and answer *refused by the person* when it does not come — so do
what was asked and say plainly what was done. Then each app's `describe` under
its id, then the panel in context if there is one, then a short word on style:
answer in the language the person
wrote in, name things by what a person calls them rather than by row id, keep
it short — this is a panel in a workspace, not a page. No chords, and no
picture of the workspace: the model acts through tools, not through the
keyboard.

The `tools` array follows as `function` definitions, then the turns. Nothing in
the prompt changes between two requests of the same chat — no clock, no counts,
no ids of the moment — so whatever the model and the gateway cache of a
repeated prefix, they cache without being asked; this wire has no
`cache_control` to place. `usage` arrives on the stream's last chunk, is kept on
the run, and is drawn as a muted line under the agent's turn: *2.1k in (1.9k
cached), 310 out*.

## The surface

### The chat panel

A transcript above, the composer below. The person's turns are on one side and
the agent's on the other, chips in the person's; the model's reasoning, where
it sends any, is one folded muted line; a tool call is a card, saying on its
first line what it did — a writing tool's own undo sentence, a reading tool's
name and arguments — and, folded behind it, what it came to, or, while it is
waiting to be allowed, the two buttons that answer it; the live tail
streams into the last turn while the answer is being written. A long transcript
is a list of turns, so a thousand-turn chat costs what it shows, and everything
the agent wrote is a selectable run, because an answer is something one copies
out.

The composer is a chip row over a multi-line field. `enter` sends and
`shift+enter` is a newline. What is typed is not written down — an unsent
message is not a row — and a send the store refuses leaves the words in the
field, because they are the only copy.

The bar is **send** (`cmd+s`) while there is something to send and nothing
going, **stop** (`cmd+k`) while something is, **retry** (`cmd+r`) on a round
that failed or was stopped, and **continue** (`cmd+o`) on an answer the model
ran out of room for, so a bar with none of the four is a chat waiting to be
written in. Then **allow** (no letter) and **refuse** (`cmd+f`) while a call is
[waiting for a word](#the-gate-what-asks-first) — the same two the card wears,
because the bar is where a panel's verbs live and a card is not always in
view. Then **add panel** (`cmd+p`), which is always there because it is the
phone's way into context and harmless where the chord exists. Nothing on
the bar leaves the conversation: a fresh chat and the list of them belong to
the agents list, which the launcher opens.

The title is the chat's own — the first line of the first thing said in it,
clipped at sixty characters — and `chat` before anything has been. It wishes
for four by six: a third of the desktop's width reads as a conversation
should, and leaves room for what the chat opens beside it.

### The agents list

A [rich table](./richtable.md) over the chats, newest first: the title, the
model that answered, when it last moved, and what its newest round is doing, so
the list is where one finds the chat that stopped short — a run *waiting* on a
call nobody has shown, a run that *failed*. The filter tags are `@waiting`,
`@failed`, `@model:` and `@date`; free text matches the title and every turn's
body, which is where the words a person remembers actually are. `@model:`
completes against the models chats have actually run on.

The cursor previews a chat beside the list, by the shell's
[preview](./interaction-grammar.md#preview-the-one-open-that-does-not-go) rule.
Its bar always wears **new** (`cmd+n`), a blank chat joined to the list where
its previews go, and, once rows are marked, **delete n**
(`cmd+d`), which takes the chats with their turns, runs and calls as one node.
The cursor then stands where it stood, on whichever row is there now, as part
of the same action.

### The phone

The chat fills the grid, and the composer needs no code of its own to stay
above the soft keyboard: the shell shortens the workspace by the keyboard's
occlusion on `Event::VirtualKeyboard`, so the panel is drawn shorter, the
transcript shortens with it and the composer stays at the foot of whatever room
is left. That is what every panel with a field at its foot does, the compose
sheet included.

`cmd+shift+a` has no glass equivalent — a long press on a header is already
*pick the panel up* — so it joins the
[gestures the glass has no word for](./open-questions.md), and **add panel** on
the bar is the phone's way in. It stands a field in the composer's chip row
with a completion box under it, offering every panel open on every workspace by
its title — this chat excepted — the ones that begin with what is typed first
and the ones that merely contain it after. `enter` takes what the offer is
showing and makes it a chip; `esc` puts the field away.

## Failure and problems

- **No token, or the old hash.** A request with no Cloudflare token to be found
  fails at once, in R2's own sentences, naming the places it looked. A keychain
  holding the S3 hash from before this change is told to run `--r2-login` again
  with the token's value. A device with no bucket asks Cloudflare for the
  account, and when that fails too the row says why, with the three places a
  bucket goes.
- **The gateway refuses** (401, 403, an account that is nobody's): the run
  fails with the gateway's own sentence. A 401 wears *unauthorized*, because a
  bad token is a standing condition and not one round's bad luck; a 403 wears
  *refused*, since it is as often the plan — *this model is not available on
  the Workers Free plan* — as the token. Providers answer a refusal as JSON
  when they answer JSON at all and as an HTML page when the account in the
  URL does not exist, so both are read and what a person sees is the sentence
  if there is one and the page if there is not.
- **The network is down**: the run fails and the chat offers *retry*. Nothing
  retries by itself: six blind retries of a paid request is not a policy
  anyone wants, and the gateway does its own retrying.
- **The model stops on a filter** or **runs out of room**: the turn keeps the
  word the model stopped on, `content_filter` or `length`, drawn as *filtered*
  or *cut short* in the muted line under it, and the chat goes on. On `length`
  the bar wears **continue**, which sends *Continue.* as the person's own turn
  — a cut answer is finished by a round like any other, because the wire has
  no word for *finish what you were saying* and the model reads its own cut
  answer just above.
- **A call fails**: the tool's error is the `tool` message's content, so the
  model reads it and can say what it could not do.
- **The process dies mid-run**: the ladder's sweep, above.

The app's [problem source](./apps.md#problems) is a gateway that will not open.
It is derived, never stored: the condition is the newest run of any chat having
failed with the gateway's own word in front of its sentence, and the next run
that answers clears it. One row, not one per chat — there is one gateway, and a
person who has not run `--r2-login` has not run it for every chat at once. The
row carries the sentence and, under it, the chat and the date.

## Environment knobs

| Variable | Meaning |
|---|---|
| `SUPERAPP_AGENT_LOG_PAYLOAD=1` | let the gateway keep the request and the reply; by default the app sends `cf-aig-collect-log-payload: false` |

## Not done, on purpose

- **Allow all for this run.** The [gate](#the-gate-what-asks-first) asks once
  per call, and a round with three sends in it is answered three times. A
  third button, remembered on the run, is the obvious next step; what it must
  not become is a setting that turns the gate off for good.
- **Agent profiles**: named system prompts and tool subsets. The chat carries a
  `model` and nothing else; a `profile` column is a later ladder step.
- **Other chips**: a file, a letter, a selection. The enum has the room.
- **Web search and fetch**: tools of the app's own over `kernel::http`, when
  wanted.
- **Compaction and context editing.** A chat that fills the window is a new
  chat for now, and *new* is one chord.
- **Another model, or an Anthropic-native gateway.** The model is a const,
  another route is a second `Provider`, and the Messages API with its caching
  and thinking would be a second implementation of the same capability.
- **Voice, images in turns, files as attachments.** A `Chip::File` rendered as
  a content part is the path.
- **Scheduled agents**: a run nobody asked for in a chat. The workers can do
  it; what a person sees is the question, and that is its own change request.
