# CR-008 · Files: a directory as a column, a file as a card

Status: **proposed** — requirements only, 2026-09-02. Andrey's calls on the
first draft: the disk is **read during draw**, not mirrored into the store;
mail attachments are a **follow-up CR**, not this one; a **watcher** is in
v1, so a directory is always current without a button; and the browser
**writes** — new directory, copy, move, delete — from the start.

## Why

The user space has no notion of a file: nothing to browse, no panel to look
at one, nowhere for other kinds to point. Attachments are the first thing
waiting on it (a mail carries files the app cannot see), then the editor,
the knowledge base and agents — a file is the one object every future kind
will want to name.

## The model

Two rules, one per surface:

> **A directory is a list panel. A file is a card.**

The rest is the existing grammar doing its job: a solid link opens joined,
the cursor previews and `enter` goes, a join is the only relation, buttons
only side-effect, a list borrows its preview's keys, every action is
undoable.

### Kinds

| kind | shows | grid |
|---|---|---|
| `Files { dir }` | one directory, non-recursive, as a rich table | 4×6 |
| `File { path }` | one file: card + preview | 4×3, wishes taller for a preview |

A path in `p_txt`. Titles: the directory's name, the file's name. Four
wide, not three: the header carries five verbs, and three columns of
`Files → Files → File` still fill a 12-grid.

### The files panel: a column

- **Rows**: name (a directory wears a trailing `/`), size, modified.
  Directories first, then files, by name. Dot-files are out unless
  `@hidden`.
- **Filter**: free text over the name; `@dir`, `@hidden`, `@kind:` (image ·
  text · pdf · archive · other, off the extension), `@size>`,
  `@modified>`. The rich table's engine, filter grammar, autocomplete and
  paging as they are; only the datasource is new (below).
- **The walk**: the cursor **previews** — a directory row previews the
  sub-directory joined beside it, a file row previews its card — and
  **`enter` goes**: focus moves to that previewed panel, the solid-link
  rule unchanged. That is Finder's column view for free: `Files → Files →
  File`, each next step replacing the joined child, `cmd+w` closing a
  level, `cmd+←` back to the list. The preview rule reads one word wider
  than today — *a row previews into the kind its target names* — instead
  of "into exactly one kind".
- **Up**: a crumb line above the rows, `~ / Downloads / 2026`, each segment
  a **dotted** link — it replaces the panel with that ancestor in place.
  The first dotted links since the message walk went, and exactly the case
  the underline exists for.
- **Roots**: the launcher's `files` root opens `~`; on android, the app's
  data dir, where the crumbs also stop. A root cannot be copied, moved or
  deleted: those buttons are absent on it.
- **Go to** (`go to`, ⌘g): the crumbs become a path field, seeded with
  where the panel stands and a slash. Each segment completes like a
  shell's tab — the entries of the directory the segments before it name,
  a directory landing with its slash so the next offer opens at once, the
  two roots `~/` and `/` before the first slash; `tab` takes the offer.
  `enter` goes to what is typed: a directory replaces the panel in place
  (the crumbs' own semantics), a file opens its card joined, a path that
  does not exist is refused on the status line. `esc` puts the crumbs
  back. This is how the browser leaves `~` — `/tmp`, `/etc` — without a
  second root panel; `..` and `.` are read, and a relative spelling is
  not. A second root typed after the seed **restarts** the path, Emacs'
  find-file rule: `~/Downloads//tmp` is `/tmp` and `~/Downloads/~/x` is
  `~/x`, so an absolute path wins over the seed without clearing it.

### The card

Name (big), kind and size, modified, the path — selectable, so `cmd+c`
takes it.

**Preview**, under the card: UTF-8 text (the first 64 KB, mono, scrolls)
and PNG/JPEG (fit to the panel's width; makepad decodes both from bytes).
Anything else is the card alone — PDF included; `open` is how you read one.

### The verbs

Every verb is a bordered header button with its chord, and every one acts
on **the thing the panel shows** — a card's on its file, a files panel's on
its directory — never on a row. The list reaches them the way the inbox
reaches `archive`: the cursor previews the object beside it, and **the
list borrows the preview's keys** (rule 5). Cursor on `2026/`, `cmd+d`:
the previewed directory's own `delete` fires, its mark in plain sight one
column over. A panel has exactly one of each, so rule 4 holds.

A files panel wears `copy`, `move`, `delete` for the directory it shows
**only while it is joined under a parent** — while it is the object under
someone's cursor. A root, or a list opened un-joined from the launcher, is
nobody's object and wears `new dir` alone: `~` cannot be deleted, and a
chord pressed in a list never hits the directory the list itself shows.
The bridge is the whole explanation, as with the attach binding.

| panel | header |
|---|---|
| `File` | `open` · `copy` · `move` · `delete` · `×` |
| `Files` | `new dir` · `×`; **at the end of a chain** — joined under a parent, driving nothing — also `copy` · `move` · `delete` for the directory it shows; and, while something is held, `copy here` or `move here` |

A files panel is both a list and, when a row of its parent previews it,
an object. It wears the object verbs only while it is the **end of a
chain**: the thing under someone's cursor. A root, a list opened from the
launcher, or a list that is itself driving a preview wears `new dir`
alone — so `~` cannot be deleted, and a chord pressed in a list never
hits the directory the list shows; it hits what the cursor is on, one
column over, where the mark is. Where a driver and its preview share a
key for the same verb (`new dir` on both), the driver's wins and the
preview's mark is drawn plain while it is shadowed, so no bold letter
promises a chord it will not get.

Where a joined list drives a preview of its own — the middle of a chain —
its verbs and its preview's share letters (`copy` on both). The driver's
own key wins, as the fifth rule says, and **the shell draws the preview's
shadowed mark plain**, so no bold letter ever promises a chord the driver
would take. Two *different* verbs on one key stay forbidden by test; the
same verb twice is allowed exactly because the mark stays honest.

- **`open`** — hands the path to the OS (`NSWorkspace` through makepad's
  `open_url`). Nothing is executed by us; Gatekeeper does its job. macOS
  only in v1: android needs a FileProvider, so the button is absent there.
- **`new dir`** — opens a one-line field above the rows, `NEW DIR ___`;
  `enter` creates it and the cursor lands on it, `esc` puts the field
  away. A name that exists, or holds a separator, is refused on the
  field's error line. While the field holds the keyboard the text chords
  are its, and borrowed keys stand down, as with the filter.
- **`copy` / `move`** — **hold** the object: nothing touches the disk. The
  app keeps one held item, `(copy | move, path)`, in memory, process-wide;
  the toast says `copy report.pdf: choose where`. Then every `Files`
  panel's header shows **`copy here`** or **`move here`** — the label
  names what will happen — and pressing it performs into the directory
  that panel shows. Walking to the destination is the ordinary walk, so
  there is no pick mode, no dialog, and it works the same by mouse, by
  chord and on glass. A `move here` clears the hold; a `copy here` keeps
  it, so one file can be copied to three places. The hold is context, not
  history: `cmd+z` never takes it back, and it dies with the process.
- **`delete`** — to the **trash**, never `rm`: `NSFileManager
  trashItemAtURL` on macOS (it answers where the item went, which is what
  restoring needs); on android, an app-private `.trash/<n>/<name>` with the
  original path beside it. The cursor stays on the row it stood on, like
  the inbox.

Name clashes **refuse**, on the status line in the one colour errors get:
`report.pdf is already here`. No overwrite, no silent renaming — with one
exception, a copy into the file's own directory, which is the one case the
duplicate is the point: it lands as `report copy.pdf`. Moving a directory
into itself or below itself refuses. Across volumes, a move is a copy and
a trash. Symlinks are copied as links, never followed.

The file clipboard is **not** the text clipboard. A card's path is
selectable, so rule 3 yields `c` `v` `x` `a` to the text — `copy` and
`move` wear other letters, and `cmd+c` on a card copies the path, never
the file. Finder muddles the two; the grammar does not.

Nothing selects more than one thing. The unit is the row under the cursor
— the object the panel beside it shows — and a selection would multiply
every verb (open question 3 in the book). Later, if it hurts.

### The disk is read during draw

A files panel does not go through the store. Its datasource (`DirSource`,
a second `Datasource` beside `SqlSource`) holds the **listing of one
directory in memory** — one `read_dir` and a `stat` per entry — and
answers the table's three questions off it: the count under the filter,
a page, a row's rank. The filter's AST is evaluated in Rust over the
entries (name substring, the typed tags), the same grammar and the same
error line as the inbox. The cursor's identity is the name.

The listing is a **cache stamped with a generation**, exactly the shape of
the store's query cache: taken on the first draw that needs it, kept until
the watcher bumps the directory's generation, re-read lazily on the next
draw. Nothing polls, nothing re-lists per frame.

The reads go through the outside — `list_dir` and `read_file` join `now`
and `clip` as verbs — so `Fake` serves a tree out of the `files` map it
already keeps and the whole panel tests in a fake world, and a `Deny`
world says *this world has no outside* on the status line, which is what
a catalogue node draws unless it is given a fake tree.

`cmd+i` on a files panel records what a draw actually read: the path, the
filter, the entry count, and the listing's age — no SQL, honestly.

### The disk is written through effects

A filesystem verb is an effect by the substrate's line — the store cannot
reproduce it — so `Outside` gains `mkdir`, `rename`, `copy`, `trash` and
`restore`, and `Fake` performs them on its tree, which is what the tests
assert on. Two speeds:

- **Rename-class ops perform inline** — `mkdir`, `rename`, `trash`,
  `restore` are milliseconds on one volume — as `clip` does: an
  [`Effect`], performed at the call, its error straight to the status
  line.
- **A copy is deferred**: a row in the `effect` table, performed by the
  executor off the UI thread, idempotent by construction (into a temp
  name, then one rename), so a crash retries it. The panel reads its own
  job through the reactive layer (`entity = 'panel:N'`) and its status
  line says `copying…` until the row settles — progress is invalidation,
  no new machinery. A cross-volume move is that copy and then a trash.

The watcher sees our own writes like anyone's: the row appears when the
directory says so, not when the button was pressed. The action knows the
name it made, so the cursor is put on it.

### Undo

`new dir`, `copy here`, `move here` and `delete` are **actions**, each one
history node with one claim; `copy`/`move` (the hold) and `open` are not.
Every reversal is a rename-class op, so it performs inline and answers
honestly:

| action | reversal | expires when |
|---|---|---|
| `new dir` | remove the directory | it is no longer empty |
| `copy here` | trash the copy | the copy is gone |
| `move here` | move it back | its old place is taken, or it is gone |
| `delete` | restore from the trash | the trash was emptied |

An expired node reads as `· deleted` (or `· copied`, `· moved`) in the
history overlay and the walk steps past it, as a delivered send does.
Undo replaces confirmation: nothing asks *are you sure*, because every one
of these can be taken back until the disk itself says otherwise.

### The watcher

One watch per directory an open `Files` panel shows — taken when the panel
opens or is replaced onto a directory, shared by panels on the same one,
dropped when the last of them closes. Panels on other workspaces keep
theirs: a switch back must be instant. Nothing is watched recursively and
nothing is watched that no panel shows.

- **macOS: FSEvents** with file-level events, latency 0.1 s. A stream
  reports a subtree, so events below the watched directory's own level are
  discarded on arrival. **android: inotify** on the directory (`IN_CREATE
  | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO | IN_CLOSE_WRITE | IN_ATTRIB`),
  one level by construction. Both are a handful of `extern "C"` lines — no
  crate, as the sync transport and the keychain already are.
- **Coalescing**: a watcher thread folds every event for a directory in a
  ~100 ms window into **one** invalidation — a build writing ten thousand
  files is one re-list, not ten thousand. It bumps the directory's
  generation and wakes the UI (`SignalToUI`, as a sync worker's commit
  does); the next draw re-reads the listing, and a row that moved under
  the cursor is found again by name.
- **Failure is on the status line**: a watch that cannot be taken (the
  limit, permissions) leaves the panel readable and says so in the one
  colour errors get; a directory removed under a panel shows empty rows
  and *gone*, and the crumbs still climb out.

### The launcher

A `files` root. No file-name hits: nothing is indexed, and the launcher
says nothing it does not know.

### Safety

- `open` never runs anything itself.
- Nothing is ever unlinked: `delete` is the trash, and undo of a copy is
  the trash. Roots wear no destructive buttons.
- Previews read at most 64 KB of text and 20 MB of image; beyond that, the
  card alone. An image is downsampled to the panel's width on decode.

## Verification

- Unit: `DirSource` over a `Fake` tree — order, the filter's tags and free
  text, paging, rank, the hidden rule; the coalescer (a burst is one
  invalidation); a stale generation re-lists and a fresh one does not;
  every verb against `Fake` — the clash rule, the own-directory copy, the
  into-itself refusal, a root's missing buttons — and every reversal,
  including each way it expires; the accelerator table's uniqueness with
  the text chords yielded.
- `e2e/files.txt` against a temp tree the harness seeds (as it seeds the
  store): the launcher's `files` root, a walk down two levels with the
  cursor previewing, `enter` moving focus, a crumb replacing, `@kind:image`
  and the error line; `new dir` by field, `copy` on a card and `copy here`
  two columns over, `move here` once, `delete` from the list by borrowed
  key, `cmd+z` through all four — the row appearing or leaving is the
  assertion, because the watcher is the only way a row changes. A new
  step, `fs add "name"` / `fs rm "name"`, writes into the seeded tree from
  outside, so the watcher's row appears without a keypress. Every run has
  a real directory, so the suite runs `Real` for the disk while the mail
  world stays fake.
- Catalogue: a files row (directory, file, hidden), the card in its three
  readings (text, image, other), the `new dir` field, a header holding
  `move here`, the column walk as a workspace scene, the phone grid.

## The draft in the panels library

The UI is drafted as live scenes before anything touches a disk:

```sh
mise exec -- cargo run -- --library file      # the four scenes below
```

- **`files row`** — a file, a directory, the cursor's wash, a dot-file, a
  long name.
- **`files`** — `~`, `Downloads`, the cursor walk, `@kind:image`, the
  `new dir` field, a refused name on the status line, the header holding
  `move here`, a directory that is *gone*, the phone grid.
- **`file card`** — a text preview, an image, a bare card, and the card
  with a copy held.
- **`files walk`** — a workspace that starts from the files root alone
  (no help, no inbox: a stage may now boot on one panel as the strip's
  first column, not only solo), walked by keys: `↓` previews Downloads
  beside `~`, enter goes, `~ → Downloads → 2026 → a card` with the list
  still driving, `↑` replacing the card in place; then `⌘p` on the card,
  `⌘←` back to Downloads whose header now offers `copy here`, and `⌘h`.

What is real in the draft: the two kinds and their chrome, the header
verbs with their chords, the cursor **previewing** joined beside the list
(the inbox's preview for any kind: `Act::PreviewKind`), the borrowed keys
with shadowed marks, the hold and the `… here` button it raises, the
`new dir` field and its refusals, the filter grammar over the listing, the
crumbs as dotted links, rows opening joined, `enter` going. What is not:
the listing is a **demo tree** in `src/files.rs`, and every verb that
would write **toasts** what it would have done — nothing leaves the
process. The datasource (`files::DirSource`) is the shape the
implementation fills in with the disk and the watcher.

## Considered and not chosen

- **Mirroring the directory into the store** (`dir_entry` rows, a scan
  pass, the disk as an ingest source like IMAP). It would have kept panels
  on registered queries, made files launcher-searchable and `cmd+i`-traced
  as SQL, and drawn on `Deny`. Andrey's call: read it during draw. The
  costs of that are listed below, and none of them bites for a browser.
- **Verbs on the row** (a `delete` per row, or header buttons acting on
  the cursor). Rule 4 forbids the first and the second hides what a chord
  will hit; the preview-and-borrow pattern shows it, one column over.
- **A pick mode for the destination** (`move` opens a browser that knows
  what it is picking for). A hidden state the panel would have to explain;
  the hold and `move here` are two visible buttons and the ordinary walk.
- **`cut` / `paste`.** `cut` says *remove* and removes nothing; `paste`
  says nothing about which. `copy here` / `move here` name the outcome.
- **Auto-renaming on clash** everywhere (`report (2).pdf`). A silent
  second copy is how directories fill with near-duplicates; refusing is
  one red line and one keystroke to undo the walk.
- **`rm`.** Nothing needs it; the trash is what makes undo honest.
- **The native file dialog.** makepad's macOS select-file dialog is a stub
  (only the directory picker is real); a dialog is a window, and the
  product has none; android has no equivalent without JNI.
- **One kind for directory and file** (`Files` showing a card when the
  path is a file). Two functions in one kind; widening the preview rule by
  one word is the smaller change.
- **Polling** (re-list on a timer, or on focus). Wrong twice: work while
  nothing changes, and stale between ticks.

## Costs, named honestly

- **A first listing runs on the UI thread.** A directory of 50 000 entries
  is a visible pause on first draw, then cached and paged like any table.
  If a frame is blown in practice, the read moves to the watcher's thread
  and the panel draws *listing…* until it lands — the cache's shape does
  not change.
- **A delete on a chord.** `cmd+d` with the cursor on a directory trashes
  the whole directory, one keystroke, no question — as `cmd+d` on a thread
  deletes the mail. The trash and `cmd+z` are the answer; the mark on the
  previewed panel's chrome is the warning.
- **In the middle of a chain the list's own verbs win.** Focused in
  `2026` (joined under Downloads) with a file previewed, `cmd+d` trashes
  `2026`, not the file: the list wears `delete` for itself, the card's
  mark is drawn plain. The file is one `enter` away. Whether a joined
  list should instead yield its own verbs while it drives a preview is a
  question for real use.
- **A big copy is a job, not a gesture.** Gigabytes take their time; the
  status line says so, and there is no cancel in v1.
- **Not searchable.** The launcher opens `files` and nothing finer.
- **`Deny` draws nothing.** A catalogue node needs a fake tree handed in.
- **Android v1 is the app's own directory**, with its own trash. Scoped
  storage puts the rest behind SAF.
- **FSEvents watches a subtree** and the filter is ours; a panel on `~`
  hears every write under it and discards nearly all of them — cheap, but
  not nothing. inotify has a per-user watch limit; one per open panel is
  far inside it.
- **Two preview formats.** PDF is the obvious gap; `open` covers it.

## Follow-ups

1. **Attachments** — the next CR: `parse_mail` lists parts; an
   `Attachment { mail, idx }` kind on the same card widget, bytes out of
   `raw`; `save` into a directory; compose gains `attach`, bound by the
   join to a files panel; multipart send; a Finder drop onto a compose.
2. **Rename** — the `new dir` field's mechanics on an existing name; one
   more verb and one more reversal.
3. A **selection** that spans rows, if one-at-a-time hurts.
4. A **row swipe** to the trash on glass, the inbox curtain's physics.
5. Android: SAF for the rest of the disk, the share sheet, FileProvider
   for `open`; a cancel for a running copy.
6. Files in the launcher, once something indexes them.
7. PDF preview.

## Decisions to take first

1. **Verbs act on the shown object, the list borrows them** — not on the
   cursor row directly; a files panel wears its object verbs only while
   joined, and a driver's own key shadows its preview's mark.
2. **Hold, then `copy here` / `move here`** — not cut/paste, not a pick
   mode.
3. **Clashes refuse**, except a copy into its own directory.
4. **Trash on both platforms**, our own on android; nothing unlinks.
5. **Roots**: `~` on macOS, the app's data dir on android.
6. **Crumbs as dotted links** for *up*.
