# Notes and the text editor

**notes** in the launcher opens a rich table of notes, most recently edited
first. The filter searches titles and bodies. **new note** creates a note
and opens its editor. The first nonempty line supplies the title, with a
leading Markdown heading marker removed. Delete works on the cursor or
marked notes and can be undone.
A selected note stays highlighted through background refreshes;
keyboard navigation follows its updated position in the list.

Notes are database records, independent of files and directories. Every text
change is queued for the store, including local text undo and redo. Pending
edits coalesce before a write; the editor shows *saving…* until its latest
text commits. There is no Save action for a note. Closing a panel lets its
accepted writes finish, and application shutdown waits for open and closed
editors to finish their queued writes.

The editor shows plain Markdown source. Delimiters stay visible: `**bold**`
is bold, `*emphasis*` is italic, and headings are bold at the normal font
size. Links, quotations, strikethrough spans and code use a quieter grey.
Fenced code has no language highlighting. The reference is
[CodeMirror's Markdown source mode](https://codemirror.net/5/mode/markdown/);
CommonMark parsing uses the existing pulldown-cmark dependency and its
source offsets. Escapes, nested emphasis and code boundaries follow the
parser, rather than independent regular expressions.

The shell's `SourceInput` specializes the pinned Makepad native input with
cached style spans. It retains native undo, IME, selection, clipboard,
wrapping, caret navigation and scrolling. Only glyph fonts and colours
change; source positions and line geometry stay the same. Parsing happens
on a blocking worker after text changes; the UI receives completed style
spans, and styled layout is reused until the text or width changes.
Tabs draw at four-column stops while remaining literal tabs in the source.
Shift-click extends or shrinks the selection from its existing anchor, and
a following drag keeps that anchor. This behavior is shared by editable
fields, read-only text, HTML/Markdown readers and PDF text selections.
The small styling hook currently requires a local copy of Makepad's input,
with its license beside it, until that hook is available upstream. The shell
registers the shared input's template before any app UI is loaded.

## Editing files

A text file card in the file browser offers **edit** when the notes app is
installed. This opens the same editor in file mode. A changed file wears
`*` in its title and says *draft saved · save to update file*. Typing writes
only `notes_draft`; **save**, or `cmd+s` with the caret in the editor, is the
only action that changes the filesystem. Closing or restarting retains the
draft, recovered by opening the same path again.

Save compares the file with the original that the draft was based on. A
conflict or failure keeps the draft and reports the error. On a real disk,
new bytes are staged beside the target, flushed, and renamed into place;
permissions and symbolic links are preserved. External writers are not
locked, so the last comparison and replacement are not a filesystem
compare-and-swap. UTF-8 BOMs and individual LF/CRLF endings are retained,
including in mixed-ending files: line alignment keeps unchanged lines'
endings when lines are inserted or deleted, and replacement lines reuse
their prior endings. Additional new lines follow the surrounding style.
Files over 2 MiB, invalid UTF-8 and binary data are refused rather than truncated.

Opening, line-ending alignment, Markdown parsing, autosaves and file saves run
outside the UI thread. Save is an ordering barrier: it persists that text as a
draft before touching the file. Typing accepted after Save retains a new draft
against the saved file's contents. A failed save leaves the draft intact and
records the error in the effect log even if the editor has closed. Concurrent
draft changes are not removed by another editor's save cleanup.

| Tag | Arguments | Meaning |
|---|---|---|
| `notes` | optional filter | the notes table |
| `editor` | `note`, note ID | autosaved note |
| `editor` | `file`, display path | file with an autosaved draft |

`notes_note` holds the title, body, timestamps and soft deletion flag.
`notes_draft` holds a path, the original source, edited source and timestamp.
Neither typing nor saving a file adds a workspace undo action. Text undo is
the editor's own; creation and deletion participate in workspace history.

## Agent tools

Agents can discover note IDs and draft paths with `sql.query`, then use the
notes tools to read and change their contents. Titles, timestamps and file
originals are maintained by the same storage code as the editor.

| Tool | Input | Result |
|---|---|---|
| `notes.create` | `body` | Create an autosaved note with its initial text |
| `notes.read` | `id`, optional `offset` | Read a note's source and revision |
| `notes.update` | `id`, `body`, `revision` | Replace a note's complete source |
| `notes.read_draft` | `path`, optional `offset` | Read the current draft, or the file when no draft exists |
| `notes.create_draft` | `path`, `body`, `revision` | Create a draft for an existing text file |
| `notes.update_draft` | `path`, `body`, `revision` | Replace an existing draft's complete source |

Read tools run on the agent's background worker, including file reads and
revision hashing. Write preparation also runs in the background: validation,
original-file reads, revision checks and line-ending conversion finish before
the session receives a transaction. That transaction rechecks the stored text
and commits the edit and agent result together. Stopping a run before the
transaction starts prevents the edit from being applied.
Read results include `body`, `revision`, `total_bytes` and `next_offset`.
Each page contains at most 64 KiB and ends on a UTF-8 boundary. Start at
offset zero, follow `next_offset` until it is null, and check that every
page has the same revision before replacing the whole body. Updates must
include the revision returned by the last read or write of that note or
path. A stale revision fails without changing the text or recording an
undo action; read again and incorporate the newer text before retrying.
Writes accept up to 2 MiB and retain at most 2 MiB of prior text per field
for undo. Missing or deleted notes are refused.

For a file, call `notes.read_draft` first: `exists=false` calls for
`notes.create_draft`, while `exists=true` calls for `notes.update_draft`.
Reading alone creates no draft. Paths can be absolute or start with `~/`;
both spellings use the editor's same draft key. File bodies use LF text
without a BOM, while storage retains the original BOM and individual line
endings. The tools capture the file's original contents themselves and
never accept an `original` argument. Changing a draft back to its original
removes it. File changes on disk still trigger the editor's Save conflict
check, even after an agent has updated a draft.

Each changed write is one workspace undo step and runs without an approval
card. Undo and redo check the stored text before changing it, preserving
subsequent edits; note creation uses the same undoable soft deletion as
the **new note** action. Open editors observe tool changes automatically.
Every result includes a `panel` object with `tag` and `args`, suitable for
`panels.open`. Draft tools, including undo and redo, only change the
database: committing a draft to its file still requires explicit **Save**
in the editor.
