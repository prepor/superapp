# CR-022 · The knowledge base: pages an agent keeps, files a bucket keeps

Status: **proposed, second round** (Andrey, 2026-09-16: "new app KB (Knowledge
Base)! `~/cloud/KB` is my current Knowledge Base · an app to add and extract
info from this KB · primary interface to this KB is agent · this KB also
could be used by agents app for skills/memory storage, it should be part of
context for agent · use R2 to store files and local cache (common with other
apps) · import existing KB · present me initial plan / UI before
implementing"). The first round was drawn from the folder as checked out;
this one is drawn from the folder's `origin/main`, the kernel and the apps
as they are on superapp's `main` at 3527e583, and Andrey's answers, which
are recorded under [Decided in review](#decided-in-review). Nothing below is
built except the panels-library prototype named in
[The library and the suites](#the-library-and-the-suites), which is drawn
first so the surface can be judged before the store and the agent are
touched.

## Why

**The KB is already an agent's wiki, and it lives where no agent of ours
can reach it.** `~/cloud/KB` is a git repository synced by iCloud: about a
hundred Markdown pages in the wiki layer — projects, concepts, entities and
source summaries, cross-linked with `[[wikilinks]]` and catalogued by hand
in `index.md` — over about a hundred raw files under `sources/`,
`attachments/` and `taxes/`: PDFs, scans, screenshots, saved articles, some
60 MB. Its `CLAUDE.md` is a schema for a model: *add a source* as one
concise page with the facts needed for recall, *answer a question* by
searching the wiki and citing pages, *lint* for contradictions, stale facts
and broken links. It is Karpathy's LLM-wiki pattern, and it works —
thirty-six pull requests of ingests since May. What it cannot do is be
there when the [agent](../book/src/agents.md) in this workspace is asked
what the residence permit's renewal window is, or which shop built the
bike's wheels: that agent has mail, files, chats and notes as its hands and
knows nothing of the wiki, and the wiki's own agent is a terminal on one
Mac with the folder mounted.

**The agent has no memory and no skills.** Every chat starts from the
system prompt: what superapp is, each app's data dictionary, the panel in
context. Nothing the person told it yesterday is there today, and a
procedure worth following — how to file a tax document, how this person
likes a letter drafted — is retyped or lost. The agents chapter lists
*agent profiles: named system prompts and tool subsets* under not done.
Claude Code's answer to the same gap is a memory directory and a skills
directory of Markdown files; Andrey's brief asks for the same two things to
live in the KB, because they are pages like any other — facts about a
person and instructions for a task are exactly what a wiki holds.

**The files have nowhere to go.** A page is a few kilobytes and belongs in
the store, where [device sync](../book/src/device-sync.md) carries it to
the phone as cells and `sql.query` reads it. A scanned passport is not a
cell. The kernel has an R2 client that nothing writes through yet — the
backup form files credentials and checks them, and that is all the bucket
does today — with `put_new`, `get`, `list` and `delete` over SigV4, and a
bounded [blob cache](../book/src/data-substrate.md#blob-cache) that mail,
Telegram and the map already share. The KB's files are the first thing the
bucket is for: written once under their content hash, fetched into the
cache when a page is read, on whichever device is asking.

## The reference

Three things are the reference, and the plan keeps to each where it can.

**The folder as it is on `origin/main`.** The layout and the rules in
`CLAUDE.md` as rewritten in "Simplify and trim knowledge base" (#33):
kebab-case slugs unique across the tree, `[[slug]]` and `[[slug|text]]`
links, minimal frontmatter (`type`, `aliases`, `tags`), `sources/` read
but never edited, `index.md` the catalogue of curated pages, git history the
changelog. Three operations: *add a source* (a raw copy when useful, one
concise page, related pages only when a fact there changes), *answer a
question* (search, cite, distinguish records from inference, save only
what will be useful again), *lint*. A writing style: plain language, keep
only what will be useful later, prefer a source link to a retelling, no
essays, preserve the person's wording and uncertainty. An evidence rule:
never fill a gap with a likely answer; separate documented facts, the
person's choices and open questions; date prices, laws and software
behaviour. The local checkout is four commits behind this and the import
reads `origin/main`, not the checkout.

Things in the folder the import has to know: `berlin.md` exists twice
(root, and `city-safety/cities/`) and so does `README.md` (root, and
`dawarich/deploy/`); `index.md` is curated, not exhaustive — subpages,
`berlin`, `gruene-hauptwege` and the inbox have no line; attachment links
mix `%20` for spaces with a literal `%2F` that is part of a filename;
`sources/heptabase-2026-05-18/` is a 743-file, 178 MB Heptabase export
that curated pages reach only through its manifest page, and two of its
cards hold password-like values.

**Karpathy's LLM-wiki pattern**, which the README cites: raw sources are
immutable, the wiki is the model's to maintain, the schema tells it how.
The app keeps the three layers — files, pages, and the app's own `describe`
plus one skill page in place of `CLAUDE.md`.

**Claude Code's memory and skills.** A memory is one fact per file with a
one-line description, indexed in `MEMORY.md`, which is loaded into every
session; a skill is a `SKILL.md` whose name and description are always in
context and whose body is loaded when the task comes up. The KB does the
same with two page kinds and one rule about what goes into every chat.

## The words

- The **KB** is the knowledge base: pages, files, and the links between
  them. One per person, across their devices.
- A **page** is one Markdown document, named by its **slug** — the
  kebab-case word a wikilink targets — and of one **kind**: the folder's
  four (project, concept, entity, source) and three of the app's own
  (skill, memory, inbox). A page has a **title**, a one-line **summary**
  (the line `index.md` kept for it), **aliases** and **tags**.
- A **file** is anything that is not a page: a PDF, a scan, a screenshot, a
  raw article. It is named by its **path** in the KB
  (`sources/aufenthaltstitel-2027-06-20.pdf`) and its content is its
  **hash**, the SHA-256 the bucket stores it under.
- A **link** is a page's reference to a page or a file, read out of its
  body: a wikilink, a Markdown link to `x.md`, a link or an image whose
  target is a file's path. A **backlink** is the same thing from the other
  end. A link to a slug no page has is **dangling**; a page nothing links
  to is an **orphan**.
- A **revision** is one write of a page: when, on which device, by whom —
  the editor, the import, or an agent chat — with what message, and the
  body it left. Append-only; the KB's commit log.
- The **catalogue** is the list of every page with its kind and summary —
  `index.md`, derived rather than kept.
- The **brief** is what the KB puts into every chat's system prompt: the
  memory pages whole, the skills by name and summary, the catalogue one line
  a page.
- A **skill** is a page of kind `skill`: instructions for a task. Its name
  and summary are in every brief; the agent reads the body when the task
  comes up; **use** on its bar opens a chat with it already read.
- A **memory** is a page of kind `memory`: what the agent keeps about the
  person between chats. In every brief, whole.
- The **outbox** is where a file's bytes wait beside the store until the
  bucket has them; the **cache** is the kernel's blob cache, where fetched
  and uploaded bytes live under `kb:<hash>`.

## The agent first

> **The chat is the KB's front door; the panels are where the person sees
> what the agent did and corrects it.**

Andrey's rule for the design, and it decides five things.

**Every way in and out is a tool.** Search, read, write, rename, delete,
remember, a file's contents, attaching a file, the lint: whatever a panel
can do, a tool does, with the same names in the [tools table](#the-tools),
so a person who never opens the catalogue still has the whole KB through a
sentence. The only thing a panel does that a tool does not is the one-time
import of the folder.

**The agent knows what the KB holds before it is asked.** The brief puts
the catalogue — every page as *slug (kind) — summary* — into every chat's
system prompt, with the memory pages whole and the skills by name. "What
does my kb say about the flat" needs no `kb.search` to know that
`pappelallee-52` exists; it reads it. The catalogue is the index the
folder's rules made the model keep by hand, now derived and always current.

**Adding is telling.** A source arrives as a chip or a tool result — a mail
attachment, a Telegram file, a file the picker chose, a page the browser
showed — and the person says *file this*. The agent calls `kb.attach` to
put the bytes under a path, `kb.write` to make the summary page, and the
folder's *add a source* operation has happened without a form. A fact
arrives as a sentence — *remember that Galina's mobile changed* — and
`kb.remember` appends it to a memory page. A thought with no home yet is a
page of kind `inbox`. There is no capture form, on purpose: the chat's
composer is the capture form.

**The agent's work is signed.** A revision names who wrote it: the editor,
the import, or the chat — `kb_revision.author` holds `chat:<id>`, and the
page's history says *by the agent in "file the tax letter"*, a link that
opens that chat beside the page. An **ask** on a page opens a chat with the
page as a chip; **history** shows what the chats did to it; **restore** is
the undo that reaches across days. Every tool write is also one ordinary
undo while the chat is open.

**The panels are for reading and checking.** The catalogue's `@orphan`,
`@dangling` and `@stale` filters show what the lint would say; its **ask**
verb is first on the bar, before **new page**; the page's bar starts with
**ask**; the catalogue's **lint** verb is not a report but a chat opened
with *run kb.lint and propose fixes*, the road Fluent's tutor takes. The
editor exists because a person sometimes wants to write a paragraph by
hand, not because that is how pages are made.

## The model

> **A page is a row, a file is a hash, and the agent reads the catalogue
> before it answers.**

### Pages are rows

A page is a `kb_page` row keyed by its slug, the body without its
frontmatter, the frontmatter's fields as columns. It replicates as
[notes](../book/src/notes.md) do — cell by cell, last writer wins — so the
phone has the whole wiki without a folder, `sql.query` reads it, and an
FTS5 index over title, aliases, tags, summary and body answers a search in
a keystroke. Slugs are unique, as the folder's rule says, so a wikilink is
a key lookup; an alias resolves too, case-folded, because `[[Berlin]]`
means `berlin`; and a wikilink whose word is a file's stem —
`[[heptabase-export-2026-05-18]]` names `sources/heptabase-export-2026-05-18.md`,
which is a file — resolves to the file.

The folder's frontmatter is not lost: an editor and the `kb.write` tool
both take a document with a leading `---` block and file its fields into
the columns, and both hand it back the same way, so a model that learned
Markdown-with-frontmatter writes what it knows and a person editing sees
what the file would have said. `type` is the kind, and a kind the folder
never had — `skill`, `memory`, `inbox` — is spelled the same way.
Frontmatter keys the columns do not know (`captured`, `source`, the inbox's)
are kept in an `extra` JSON column and handed back in the block.

### Files are hashes

A file is a `kb_file` row: its path, the hash of its bytes, the MIME type
read off the bytes, the size, and — for a text file up to 256 KiB — its
text, so an archived article is searchable and readable on every device
without a fetch. The bytes are not in the store.

Their home is the bucket, at `kb/blob/<sha256>`, written with `put_new`:
content-addressed objects are immutable, so the one conditional write the
client has is the right one, *exists* is success, and a retry is free. The
client holds the whole object in memory and has no multipart upload; the
largest file in the folder is 16 MB and the client's timeout allows a
mebibyte a second on top of its minute, so the worker uploads one file at a
time and that is enough. The bucket is the one the
[backup form](../book/src/device-sync.md#the-bucket-form) already names,
opened with the same token; nothing new is configured. The client prefixes
every key with the path in the bucket URL, so two devices share the KB's
objects only when their URLs name the same bucket and path — the form's
URL is the same on each device today, and the book says it must stay so.

Their second home is the kernel's blob cache under `kb:<hash>` — the same
`blobs/` directory beside the store that mail parts, Telegram media and
map tiles share, the same 1 GiB budget and the same LRU. A read that misses
fetches from the bucket into the cache and verifies the hash on the way;
after that the file card, the page's pictures and `kb.file` read it from
disk like any other cached blob. A cache file has no extension, so the
card and the viewer are told the kind by the row, as Telegram's
`playable_beside` does for its player. The `kb:` prefix joins `tg:`,
`mail:` and `agent:` in `picture::KEYS`, the list of keys a tool may name
in `look`, so a picture in the KB is put in front of a model that can see.

Between the two homes is the **outbox**: `kb/outbox/<hash>` beside the
store, where bytes an import or an attach produced wait until the bucket
has them. The cache would evict an original nobody has uploaded; the
outbox is not a cache. One queued [job](../book/src/data-substrate.md#queued-jobs)
per file, `kb.upload {hash}`, safe to repeat, claimed by one `kb-upload`
worker that exists while any is pending; on success the file is
`ingest`ed into the cache and the outbox entry is gone. A device with no
bucket keeps everything working — pages, search, the agent, files it has
in its outbox — and one problem row says *n files wait for a bucket*, with
*backup* as its verb.

Nothing is ever deleted from the bucket. A file's row is soft-deleted like
a page; its blob stays, because a hash is cheap, a revision may still name
it, and garbage collection is a later decision. The bucket encrypts at
rest and the token is the one key; the book says so where it says what
the backups trust.

### Links, and what is derived

Every write of a page re-reads its body and rewrites its `kb_link` rows in
the same transaction: the target slug or path, the kind of link, whether it
resolves. The table is local and derived — a `Step::Derived` walk rebuilds
it from the bodies on any store, and device sync never carries it — so
backlinks, orphans and dangling links are one query each, the page panel
draws *linked from* off it, and the catalogue's `@orphan` and `@dangling`
filters are honest. The FTS5 index is derived the same way, with triggers
in the database as mail's are, so a build that has never heard of it still
maintains it.

A link's target is resolved as written, then with `%20` decoded, never
with `%2F` decoded — the folder has filenames that carry a literal `%2F`
and links that spell a space as `%20`, and one rule that decodes
everything reports files that exist as missing.

### Revisions

The folder's chronology is `git log`; the app's is `kb_revision`: one row
per write of a page — slug, instant, device, author, message, the body it
left — append-only and replicated by its uid, as Fluent's grades are. The
author is `editor`, `import` or `chat:<id>`; the agent's `kb.write` carries
a message the way a commit does; the editor's writes say *edited*. The
page's **history** lists them; a revision opens as a reading with
**restore**, which is a new write.

Revisions are also what makes last-writer-wins bearable: two devices that
edit one page while apart keep one body on the row, and both in the
history. Nothing is lost; the losing edit is a restore away.

### The brief: what every chat knows

The kernel's `App::describe` is prose about the tables, static, copied at
`attach`; the KB's says what a page and a file are and which tool to
prefer. What the brief carries is not static — it is the catalogue as it
stands, and the memory pages as they were last written — so it wants one
new hook:

```rust
trait App {
    /// What this app wants every chat to know *now*, read off the store
    /// when a request is built. Unlike `describe`, it changes; it goes
    /// after every `describe`, so the static prefix stays what it was.
    fn brief(&self, _store: &Store) -> Option<String> { None }
}
```

The agent keeps `Vec<&'static dyn App>` beside the tools it copies in
`attach`, and `Complete::perform` asks each for a brief on the request's
own store reader, before `prompt::request`. The KB answers with three
parts under `## kb`: every memory page whole, newest first, to 8 KiB;
every skill as *slug — summary*; the catalogue as *slug (kind) — summary*,
one line a page, wiki layer only, to 16 KiB with a last line saying *and n
more — kb.search finds them*. A KB with no pages answers nothing, and a
build without the agent app has no brief to give. The caps are small on
purpose: chats have no compaction yet and the brief rides on every request
of every chat.

The prompt's rule that nothing changes between two requests of one chat
bends here, on purpose: the brief changes when a page does, which within a
chat is when the agent itself wrote one, and the block sits last so the
tools, the preamble and the describes still cache. The brief is its own
block, not the panel-context slot, so sending a different panel's chip
does not replace it.

### Skills and memory

A skill is a page of kind `skill` — its summary is the description the
brief carries and its body is what the agent reads with `kb.read` when the
task comes up, which is Claude Code's rule exactly. A skill page's bar wears
**use**: a chat opened beside the page, through the agent's `start`, with
the page as a chip and *follow this skill* as the first turn — the road
Fluent's tutor already takes.

A memory is a page of kind `memory`, in every brief whole. `kb.remember`
appends one dated line to it — the one write a model makes in the middle
of a conversation without reading first — and `kb.write` rewrites it when
it is time to tidy. Both are undoable; neither asks.

## The surface

| Tag | Argument | What it shows |
|---|---|---|
| `kb` | none, or a filter | the catalogue: every page, grouped by kind |
| `page` | a slug | one page as a reading, with its links, backlinks and files |
| `kb-edit` | a slug, or `new` | the page's source, in the shared Markdown editor |
| `kb-history` | a slug | the page's revisions, newest first |
| `kb-revision` | a revision uid | one revision as a reading, with **restore** |
| `kb-file` | a path | one file's card: what it is, where it is, the viewer over it |
| `kb-import` | none | the folder, and the one press that reads it in |

Roots: **kb** (*knowledge base wiki pages*), **ask kb** (a chat with the
catalogue as its chip, through `App::ask`), **skills** (`kb` under
`@kind:skill`), **memory** (`@kind:memory`), **kb import**. The app is
listed after notes, so its roots follow the editor's.

### The catalogue

A [rich table](../book/src/richtable.md) over `kb_page`, grouped under
upper-case captions in the index's order — PROJECTS, CONCEPTS, ENTITIES,
SOURCES, then SKILLS, MEMORY, INBOX — each row the title over its summary,
the slug muted at the right with when it was last written. Free text goes
to the FTS5 index through the table's `TextIndex` arm, as mail's does; the
tags are `@kind:`, `@tag:`, `@orphan`, `@dangling`, `@stale` (no write in
ninety days) and `@date`. The cursor previews a `page` beside the list by
the shell's rule. The bar wears **ask** (`a`) first, then **new page**
(`n`), **lint** (`k`, since `l` is the shell's), which opens a chat with
*run kb.lint and propose fixes*, a link to **import** (`m`), and over
marks **delete n** (`d`), soft and undoable.

### The page

A reading: the title, a muted line — *entity · city, home, germany ·
written 3 Sep 2026 · 4 revisions* — and the body rendered through the
shared `Html` widget. There is no Markdown widget; the app's converter
takes the two that exist — the agent's `text::html` for paragraphs, lists,
code and quotes, the reader's `sanitize` for tables and pictures — and
adds what a wiki page has: wikilinks, links to pages and files, task lists,
and pictures whose source is a file's path, loaded by blob key through
`reader::pictures`. A wikilink and a Markdown link to a page draw as solid
links and open `page(slug)` joined; a link to a file opens `kb-file(path)`;
an external link opens the browser; the reader's `handle_links` learns to
route the first two instead of opening a browser for everything. A dangling
link draws in the muted grey with no underline and says so by its colour
alone.

Under the body, under rules: **links** (what this page names, as solid
links), **linked from** (its backlinks), **files** (the files it names,
each a link to its card). The bar wears **ask** (`a`) first — the
workspace's `shift+cmd+a` for a finger — then **edit** (`e`), **history**
(`h`), **rename** (`r`), which stands a field where the title is and
rewrites every inbound link in the same undoable write, and **delete**
(`d`). A skill's bar wears **use** (`s`) before **ask**. It wishes for five
by six; a phone reads it whole.

`Panel::about` says what a page is and what its slug means;
`context_text_columns` names the body, so a page chip carries the page
whole.

### The editor

`kb-edit(slug)` is the shell's `SourceInput` with the Markdown spans over
the page's document — frontmatter block and body, exactly what `kb.write`
takes — and it behaves as the notes editor does: every change is queued and
coalesced, *saving…* until it commits, text undo the editor's own. The span
parser moves from `notes/markdown.rs` to `shell/widgets/`, where both
editors read it; notes is unchanged. **save** (`s`) files a revision and
keeps the caret; closing the panel files one if the body moved since the
last. `kb-edit(new)` is a blank document with a `---` block for the
frontmatter and nothing else; the first save takes the slug from the title
and replaces the slot with `kb-edit(that)`, as `chat(new)` does.

### The file card

The name, the kind and the size on one line, then a muted line saying
where the bytes are — *in the bucket · cached*, *fetching…*, *in the
outbox, not backed up yet*, *not here: fetch* — and the pages that name
it as solid links. Under a rule, the shared [viewer](../book/src/viewers.md)
over the cached path with the row's kind: text, a picture, a PDF with its
selectable text, a clip through the player. A miss asks the bucket on a
worker and the card redraws when the bytes land. The bar wears **ask**
(`a`), **open** (`o`) and, while the file is not here, **fetch** (`f`).
Its wish follows the viewer's.

### The history

`kb-history(slug)` is a list of revisions, newest first: the date, the
device's name, the author — *editor*, *import*, or the chat's title as a
solid link that opens the chat — and the message. The cursor previews
`kb-revision(uid)`, the body as a reading with **restore** (`r`) on its
bar, which writes that body as the page's next revision, message *restored
from 3 Sep 2026*.

### The import

A form: one path field seeded with `~/cloud/KB`, **browse** (`b`) opening
the files [picker](../book/src/files.md#the-picker) for a folder, and
**import** (`m`). While it runs the status line says *reading…*, then
*101 pages, 96 files · 12 of 96 uploaded*, and the uploads go on after
the panel closes because they are jobs. See [Import](#import).

## The tools

All `kb.`; none asks — a page write is one undo away, a delete is soft, an
attach goes to the person's own bucket.

| Tool | Writes | What it does |
|---|---|---|
| `kb.search` | no | pages and files a query's words reach, best first, out of the FTS5 index: slug or path, kind, title, summary, a snippet; `kind` narrows it |
| `kb.read` | no | one page whole: its document with frontmatter, revision, links, backlinks, files; 64 KiB pages with `next_offset`, as notes read |
| `kb.write` | yes | create or replace one page from a document with frontmatter; `revision` refuses a stale edit; `message` is the revision's; links re-read; one undo |
| `kb.rename` | yes | a new slug, and every inbound link rewritten in the same write |
| `kb.delete` | yes | soft-deletes a page; its links go with it; undo brings both back |
| `kb.remember` | yes | appends one dated line to a memory page, making the page if there is none; one undo |
| `kb.history` | no | a page's revisions, newest first, with author and message; `uid` reads one whole |
| `kb.file` | no | a file's contents, fetched from the bucket into the cache on demand: text and PDF text as `mail.attachment` answers them, a picture described and shown, pages of a scanned PDF as pictures |
| `kb.attach` | yes | puts a file into the KB under a path: `from` is a disk path (`~/Downloads/x.pdf`) or a blob key a tool answered with (`mail:…`, `tg:…`, `agent:…`); hashed, written to the outbox, a row, an upload job |
| `kb.lint` | no | the report the folder's schema asks for, off the derived tables: orphans, dangling links, pages not written in ninety days, files no page names, a memory page over its cap |

`kb.read` on a slug that is an alias answers the page it names. The
`describe` says what the folder's `CLAUDE.md` says — sources are never
edited, a rename rewrites links, expand a page rather than fork one, no
speculative stubs, plain language, keep only what will be useful later,
never fill a gap with a likely answer, distinguish records from inference
— and where the KB's rules differ: there is no `index.md` to update,
because the catalogue is derived, and the summary column is what its line
used to hold.

## Search

One search [provider](../book/src/interaction-grammar.md#search), `kb`,
over the two FTS5 indexes: pages first, then files with text, each hit a
`page(slug)` or a `kb-file(path)` with the summary or the path as its
detail. `shift+cmd+s` finds a passport scan by the word on its page.

## The store

Every table is prefixed `kb_`. Instants are unix seconds.

| Table | What a row is |
|---|---|
| `kb_page` | one page: `slug` (key), `kind`, `title`, `summary`, `aliases` and `tags` as JSON arrays, `extra` (other frontmatter, JSON), `body` without frontmatter, `path` it was imported from or empty, `created`, `updated`, `deleted` |
| `kb_file` | one file: `path` (key), `hash`, `mime`, `size`, `text` for a text file up to 256 KiB else empty, `created`, `updated`, `deleted` |
| `kb_revision` | one write: `uid` (key, random), `slug`, `at`, `device`, `author` (`editor`, `import`, `chat:<id>`), `message`, `body` — the whole document, frontmatter included |
| `kb_link` | local, derived: `slug`, `target`, `kind` (`wiki`, `md`, `file`, `image`), `resolved` |
| `kb_page_fts`, `kb_file_fts` | local, derived: FTS5 over title, aliases, tags, summary, body; over path and text; `content=` the row, triggers in the database, `unicode61 remove_diacritics 2` |

Every column but a key has a default, because a row another device made
arrives one cell at a time. `deleted` is soft on both `kb_page` and
`kb_file`, so undo is an update and a tombstone never races an edit. The
tables are protected from `sql.write`, because a raw write would skip the
links, the revision and the index.

The queue carries one job kind, `kb.upload {hash, path}`, safe to repeat.
Two in-memory effects: `kb.fetch {hash}` — the bucket into the cache — and
`kb.read_tree {path}`, the import's walk of a folder through the world's
`Disk`.

### What replicates

| Table | Key | What travels | What stays local |
|---|---|---|---|
| `kb_page` | `slug` | everything but the key | — |
| `kb_file` | `path` | `hash`, `mime`, `size`, `text`, `created`, `updated`, `deleted` | the bytes, which travel by the bucket |
| `kb_revision` | `uid` | `slug`, `at`, `device`, `author`, `message`, `body` | — |

`kb_link` and both indexes are rebuilt from the rows. The outbox and the
cache are this device's. A revision's `chat:<id>` names a chat that lives
on the device that had it — agent chats do not replicate — so the history's
link opens the chat where it is and says *on another device* where it is
not.

## Import

`kb-import` reads a folder through the world's `Disk` on a worker and
writes it on the UI thread as one undoable action for the rows, plus one
upload job per file. The folder is `~/cloud/KB` at its `origin/main`,
pulled first. The rules, read off the folder:

- **Pages** are every `*.md` outside `sources/` and outside `.git`,
  `.obsidian`, `.conductor` and empty folders. The slug is the file's stem
  where that is unique across the tree, else `dir/stem` for every one that
  is not at the root (`city-safety/cities/berlin`, `dawarich/deploy/readme`),
  and `path` remembers where it came from. Frontmatter becomes the
  columns; `type` becomes the kind and defaults to `concept`; keys the
  columns do not know go to `extra`. The summary is the line `index.md`
  carries for the slug, else the first sentence of the first paragraph —
  the index is curated and most pages have no line.
- **`index.md`** is consumed into summaries and not kept: the catalogue is
  the index now. **`README.md`** becomes the page `readme`. **`CLAUDE.md`**
  becomes the skill `wiki-schema`, verbatim, with a one-line note at its
  top that the folder's layout is now the app's — the agent is the one to
  revise it, and the `describe` already carries the rules that survive.
- **`inbox/*.md`** become pages of kind `inbox`, titled from their
  frontmatter, `captured` and `source` kept in `extra`.
- **Files** are everything else — `sources/**`, `attachments/**`,
  `taxes/**`, `dawarich/deploy/**`, `city-safety/overview.typ` — by their
  path relative to the folder, dot-files and `.DS_Store` skipped. A text
  file keeps its text. Bytes go to the outbox and a job to the queue.
- **`sources/heptabase-2026-05-18/` is not imported.** It is an archive of
  another tool — 743 files, 178 MB, journals and tutorials and cards with
  secrets in them — that the curated pages reach only through the manifest
  `sources/heptabase-export-2026-05-18.md`, which is imported as a file
  like the other sources. The export stays in the folder and its git
  history; `interests.md` still says what is in it.
- **Links** in bodies are kept as written; `[[x]]` resolves by slug, by
  alias, then by a file's stem; `dir/x.md` by `path`; `sources/x.pdf` by a
  file's path, with `%20` decoded and `%2F` not. What does not resolve is
  dangling, and the lint says so — the manifest's links into the export
  will be the first.
- **Revisions**: one per page, author `import`, message *imported from
  ~/cloud/KB at <commit>*, on this device, at the file's modification
  time. Git's own history stays in the repository; 491 of its 593 commits
  are Conductor checkpoints and none of them is a revision anyone wants.
- **Twice** adds nothing: a slug the store has is left as it stands, a
  path whose hash is unchanged is skipped, a changed one becomes a new
  hash and a new upload. The status line says what was added and what
  was already there.

The real run is Andrey's: about a hundred pages, about a hundred files,
some 60 MB, on the Mac that has the folder; the phone gets the pages by
sync and the files by fetch. After it the app is the KB's home; the folder
stays as an archive with its git history, and an export back to a folder
is a later verb. The suite runs the same reader over a fixture folder in
the kernel's demo disk (`~/kb`: six pages, an `index.md`, a `CLAUDE.md`,
one PDF, one picture), which is generic enough to live there.

## The library and the suites

The prototype is drawn first, as phase 0, over a seeded fictional wiki
(eight pages across the seven kinds, three files, links among them — no
row of Andrey's), and it leads with the agent, because that is the door:

- **`agent chat`** gains three nodes: *what does my kb say about berlin*
  runs a `kb.search` through the scripted gateway and answers with page
  links; *file this letter in the kb* over a mail attachment chip runs
  `kb.attach` and `kb.write` and the page appears beside the chat; *remember
  that …* runs `kb.remember`. The recorded request shows the brief.
- **`kb`**: the catalogue with its captions and **ask** first on the bar;
  `@kind:skill`; `@orphan` showing one row.
- **`page`**: an entity page with a picture, links, backlinks and a file;
  a memory page; a skill page with **use** on its bar; a page whose link
  dangles.
- **`kb history`** with a revision *by the agent in "file the tax letter"*,
  and **`kb revision`** with **restore**.
- **`kb edit`**: a page's document with its frontmatter in the editor;
  `new`.
- **`kb file`**: a cached PDF under the viewer; a picture; *fetching…*; *in
  the outbox, not backed up yet*.
- **`kb import`**: idle on `~/cloud/KB`; reading; done with counts and
  uploads pending.

The suites: `e2e/kb/basic.txt` (the catalogue from the launcher, a walk
that previews pages, a wikilink followed, a backlink, a search),
`e2e/kb/edit.txt` (new page, save, the slug taken from the title, a
revision in the history, restore), `e2e/kb/files.txt` (a file card from a
page, the picture in the reading, a miss that fetches from the fake
bucket), `e2e/kb/import.txt` (the demo disk's `~/kb` read in: counts, a
page's summary off its `index.md`, `CLAUDE.md` as a skill, twice adds
nothing), `e2e/agent/kb.txt` (the fake gateway's `kb.search`, `kb.attach`,
`kb.write` and `kb.remember` calls, the brief in the recorded request, the
revision that names the chat). The app's tests: the frontmatter
round-trip, the link parser on every link shape the folder uses including
`%20` and `%2F`, the slug rule on both clashing stems, the brief's caps,
the upload job's idempotence against a fake bucket, every bar's letters.

The fake bucket is a `FakeR2` behind a capability the KB defines in
`App::outside` — memory under a script, the real client on a window's own
run — so no suite reaches the network and the outbox-to-cache walk is
proved inline.

## Phases

0. **The prototype.** The scenes above in the panels library over a seeded
   wiki, headless shots under `e2e/out/kb/`, no store, no tools — the
   surface to judge before anything behind it exists. This phase is
   dispatched with this document.
1. **The store and the surface.** `kb_page`, `kb_revision`, `kb_link`,
   the FTS5 index, the seed; the Markdown converter with wikilinks and
   in-app link routing; the catalogue, the page reading, the editor over
   the moved span parser, history and revision, rename and delete; the
   search provider; `basic`, `edit`.
2. **The agent.** `App::brief` in the kernel and `Complete::perform`;
   `describe`; `kb.search`, `kb.read`, `kb.write`, `kb.rename`,
   `kb.delete`, `kb.remember`, `kb.history`, `kb.lint`; `author` on
   revisions; **ask**, **use** and **lint** on the bars; the fake gateway's
   `kb` entries; `e2e/agent/kb.txt`. After 1.
3. **Files.** `kb_file` with its index, the outbox, the R2 capability with
   its fake, the upload job and worker, `kb.fetch`, the file card over the
   viewer, pictures in a page, `kb.file`, `kb.attach`, the `kb:` prefix
   among the picture keys, the problem row; `files`. After 1, beside 2.
4. **Import.** The tree reader, the slug and summary rules, the form and
   the picker, the demo-disk fixture and `import`; then the real run on
   Andrey's Mac and a look at the lint. After 3.
5. **The book.** `kb.md`, `SUMMARY.md`, the overview's app list,
   `agents.md` (the brief, the tools table), `device-sync.md` (the
   replication table, the one-bucket rule), `data-substrate.md` (the `kb:`
   keys, the outbox beside the store), `dev-x.md` (what a run writes beside
   its store); a review pass over the whole.

Each phase is one implementer in its own worktree with its own PR,
reviewed by a different vendor before the next phase starts; the human
merges.

## Decided in review

Andrey, 2026-09-16, on the first round's questions:

- **Where the KB lives.** The app is the KB's home after the import; the
  folder is the import's source and stays as an archive with its git
  history. Export back to a folder is a later verb.
- **The Heptabase export** is dropped from the import: not pages, not
  files, no text in the store. It stays in the folder.
- **Skills and memory as page kinds**, with `App::brief` and
  `kb.remember`: left to the plan; proposed as written.
- **The brief's caps**: left to the plan; 8 KiB of memory, 16 KiB of
  catalogue, wiki layer only.
- **The bucket**: trust R2's encryption at rest for the passports and
  payslips, and say so in the book. One bucket, one path, the same on every
  device.
- **The notes app** stays as it is; the span parser moves to
  `shell/widgets/` and both editors read it.
- **Agents are the primary interface**, and the surface is designed from
  that: [The agent first](#the-agent-first).

## To decide in review

- **`index.md`** consumed into summaries rather than kept as a page. The
  catalogue is the index, and a kept copy would be the one stale thing.
  Proposed: consume it.
- **Text of the top-level sources in the store** (the archived articles,
  well under a megabyte, replicated) so they are searchable on the phone
  without a fetch. Proposed: yes, to 256 KiB a file.
- **`kb.remember`** beside `kb.write`. Proposed: yes; an append is the one
  write a model makes without reading first, and it cannot clobber.
- **The shared cache budget.** Some 60 MB of KB files in the 1 GiB the
  media shares. Proposed: share it; a fetch is cheap and the outbox holds
  what is not yet uploaded.
- **A revision's chat on another device.** The history's link cannot open
  a chat that is not here. Proposed: show the chat's title from the
  revision's message and say *on another device*; replicating chats is the
  agents app's own question.

## Considered and not chosen

- **A capture root or form.** The chat's composer is the capture form:
  *file this*, *remember that*, a chip and a sentence. A form would be a
  second door beside the one the brief asked for.
- **The catalogue as a chat.** A KB panel that is itself a chat with the
  catalogue as its chip, as the front door. `App::ask` on the catalogue
  and the **ask kb** root give the same chat without a second chat panel.
- **Watching the folder and writing back** (a mirrored vault). Two sync
  systems over one set of pages, and nothing for the phone.
- **Git history into revisions.** Replaying `git log` per page through
  Workshop's git; most of the log is checkpoints. One *import* revision
  each; git keeps its own.

## Not done, on purpose

- **Export to a folder, and two-way sync with git.** A later verb and a
  separate change.
- **Garbage collection in the bucket.** Blobs are content-addressed and
  never removed; a row's soft delete is all a delete does.
- **OCR.** A scanned PDF is shown to a model that can see, as it is
  everywhere else.
- **A text CRDT.** Concurrent edits keep one body and both revisions.
- **Semantic search, embeddings.** FTS5 finds the words; the agent is the
  semantic layer, reading the catalogue. The gateway has no embeddings
  call today and this does not add one.
- **A per-chat switch for the brief**, agent profiles, tool subsets — the
  agents chapter's own not-done list.
- **Obsidian's graph.** The links are rows; a picture of them is a later
  panel.
- **Skills for Workshop's harnesses.** A Claude Code session in a
  worktree reads files, not rows; the export is the bridge, later.
- **The Heptabase export.** Dropped in review; the folder keeps it.
