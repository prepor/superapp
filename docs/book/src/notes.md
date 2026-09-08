# Notes and the text editor

**notes** in the launcher opens a rich table of notes, most recently edited
first. The filter searches titles and bodies. **new note** creates a note
and opens its editor. The first nonempty line supplies the title, with a
leading Markdown heading marker removed. Delete works on the cursor or
marked notes and can be undone.

Notes are database records, independent of files and directories. Every text
change is written through the store, including local text undo and redo.
There is no Save action for a note. Closing a panel retains its text.

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
on text changes; styled layout is reused until the text or width changes.
Tabs draw at four-column stops while remaining literal tabs in the source.
The small styling hook currently requires a local copy of Makepad's input,
with its license beside it, until that hook is available upstream.

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
compare-and-swap. UTF-8 BOMs and CRLF line endings are retained. Files over
2 MiB, invalid UTF-8 and binary data are refused rather than truncated.

| Tag | Arguments | Meaning |
|---|---|---|
| `notes` | optional filter | the notes table |
| `editor` | `note`, note ID | autosaved note |
| `editor` | `file`, display path | file with an autosaved draft |

`notes_note` holds the title, body, timestamps and soft deletion flag.
`notes_draft` holds a path, the original source, edited source and timestamp.
Neither typing nor saving a file adds a workspace undo action. Text undo is
the editor's own; creation and deletion participate in workspace history.
