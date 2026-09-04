# Files

The files app browses this machine's disk: a directory is a list, a file is a
card, five verbs write, and one copies a name. It stores nothing, because the
disk is the state, so it has no schema, no seed, no queued jobs, and no workers.
What it adds is two panel kinds, one launcher root, and a clipboard other apps
may read.

It reaches the disk through the kernel's `Disk` [capability](./apps.md#capabilities),
in the display spelling the panels use, and the kernel translates `~` to a real
path at the boundary. A restored panel therefore points at the current device's
home directory.

## Tags and roots

| Tag | Argument | What it shows |
|---|---|---|
| `files` | a directory, in display spelling (`~/Downloads`) | one directory as a list |
| `file` | a path (`~/Downloads/report.pdf`) | one file as a card |

One root: **files**, which opens `~`.

## The directory list

A `files` panel lists one directory through the [rich table](./richtable.md).
Directories come first, then names case-folded; that order is the kernel's, so
every disk produces the same one. The columns are NAME, SIZE, and MODIFIED. A
directory's name wears a trailing `/` and its size cell is a dash. An empty
directory reads *nothing here*, and one the filter emptied reads *nothing under
this filter*.

The filter tags are `@dir`, `@hidden`, `@kind:`, `@size`, and `@modified`.
`@kind:` offers image, text, pdf, archive, and other. `@hidden` is a switch
rather than a predicate: naming it at all shows dot-files, so `@not:hidden`
shows them too.

Moving the cursor previews a subdirectory as another list or a file as a card,
by the shell's [preview](./interaction-grammar.md#preview-the-one-open-that-does-not-go)
rule. `enter` moves focus to it.

Breadcrumbs such as `~ / Downloads / 2026` sit above the list, each a dotted
link that replaces the panel with that directory. Only the last four ancestors
are drawn.

`go to` (`cmd+g`) replaces the breadcrumbs with a path field, seeded with the
current directory. Completion lists entries for the current path segment; a
directory lands with its slash, so the next offer opens at once. `tab` takes
the offer, `enter` opens what is typed when the disk has it, and `esc` restores
the breadcrumbs. Paths must start with `~/` or `/`; anything relative is
refused with *"…" is not a path*, and a path that is not there says so on the
panel's own status line without navigating. A second root inside the text
restarts the path, so `~/Downloads//tmp` means `/tmp`.

Rows carry the same marks as any other table: `space` toggles, `shift+up/down`
extends a range, `esc` clears. While marks exist the bar's verbs act on the
marked set and say how many — all but `rename`, which no set wears.

## The file card

A card shows the name, the kind and size on one line, the modification date,
the path as a selectable run, and a preview under a rule. A file that has gone
reads *not there any more*.

The preview limits are the kernel's, in `kernel/src/caps/preview.rs`: a text
file's first 64 KiB, or a picture up to 20 MiB. A picture is decoded by what
its bytes say it is, never by its name, so a PNG saved as `.jpg` still draws;
only `png`, `jpg`, and `jpeg` are attempted at all. The card asks for the rows
its content needs, and the picture's size comes off its header alone so the
wish costs no read per frame.

`open` (`cmd+o`) hands the path to the operating system. Superapp does not
execute the file.

`rename` (`cmd+r`) stands a field where the name is drawn, with the name in it
selected, and takes the keyboard: the verb usually arrives through the chord of
the list above, and a caret on an unfocused panel would never see a letter.

The same card widget draws a mail [attachment](./mail.md#attachments): what a
file *is* is the panel instance's to work out, and the card is handed the
answer.

## The bar

Every files verb is a button; the links in this app are the breadcrumbs and the
rows.

A card wears `open` (`o`), `copy` (`p`), `move` (`m`), `rename` (`r`), `delete`
(`d`), and `copy path` (`c`). `copy` wears `p` rather than `c` because the
card's path line is selectable and `cmd+c` copies the text there — which is why
`c` is exactly the letter `copy path` takes: the chord copies a path either way.

A directory wears `new dir` (`n`) and `go to` (`g`) always. It wears `copy`,
`move`, `rename`, `delete`, and `copy path` when it has an object to act on:
either a marked set, or the directory itself when the panel hangs under a list,
drives no preview of its own, and is not a root. That is why a root or a parent
directory offers only `new dir` and `go to`, and why `cmd+d` in a walk means the
directory under the cursor. While marks exist it also wears `mark all` (`a`) and
`clear`, which has no letter because `esc` is the table's — and drops `rename`,
since a name is a name and two things cannot both wear it.

While the clipboard holds something, a directory wears `copy here` or `move
here` (`h`).

## The clipboard

`copy` and `move` fill a clipboard the app owns: one verb and a list of paths,
either the marked set or the single object the panel names. Filling it toasts
*copy "notes.txt": choose where, then copy here* and asks for a redraw, which
is the whole of the signal, since bars are pulled on every draw. Marks are not
consumed.

A move clears the clipboard once something has landed; a copy keeps it, so the
same set can be laid down in another directory too. Deleting a held path prunes
it, and clears the clipboard when nothing is left.

The clipboard is ordinary public API on the app, found through
`Apps::get_as::<Files>()`. Mail's compose panel reads it to offer
[attach](./mail.md#carrying-a-file); take files out of the build and that verb
never appears.

## Copying the path

`copy path` (`cmd+c`) is the one verb that leaves the app without touching a
disk: it puts what the object is called on *this* machine — the real spelling,
not the `~/` one the panels draw — on the system clipboard, through the kernel's
`Clip` [effect](./data-substrate.md#effects). Over a marked set it is one path
to a line, and the label counts them: `copy 3 paths`.

Nothing of ours changes, so there is no history node, no listing goes stale, and
the write gate is not asked: a clipboard is not a device's to lease. What
happened is the toast, and the
[effect log](./data-substrate.md#effects-and-job-panels) has the row. It is the
system clipboard and not the app's own — a `copy path` never fills a
`copy here`.

## Writing to the disk

Each writing verb is an [effect](./data-substrate.md#effects), performed where
the click is rather than queued: the wait for a copy is the wait for the
listing that follows it.

`copy here` and `move here` plan against the disk as it is at that moment, then
perform one path at a time. A path is refused, by name, when:

- it is a root;
- it is no longer there, because the source may have gone while the clipboard
  waited;
- the destination is inside it, so nothing can be copied into itself;
- something is already at that name, which covers both a collision and a move
  that goes nowhere.

A copy into the source's own directory is allowed and takes a free name:
`notes.txt` becomes `notes copy.txt`, then `notes copy 2.txt`. Two files of one
name held from two directories clash with each other, not only with the disk.

`new dir` (`cmd+n`) opens a name field beside the crumbs and creates exactly
one directory; a typo is a refusal, not a tree. `delete` (`cmd+d`) always moves
items to the system trash, and refuses a root before any disk is asked.

`rename` (`cmd+r`) is the same disk verb as a move, with the destination in the
directory the thing is already in. It never takes a set. It refuses a root, a
name containing `/` (*a name is not a path*), `.` and `..`, a name the
directory already has, and a path that has gone since the field went up —
each before or instead of writing, and each on the panel's own status line. The
name it already has closes the field and does nothing at all: no disk is asked
and no history node is made, and the text is compared as typed as well as
trimmed, so a name that itself ends in a space is not shortened by a submit
that changed nothing.

Changing only the case of a name is a rename and not a clash. On the
case-insensitive volume macOS formats by default, `notes.md` to `Notes.md` finds
the destination already stats — it is the same file — so the check is of the
**object** and not the path, at each of the three places that ask: the verb's
own refusal, the exclusive claim the real disk makes on a destination, and the
undo that asks whether something else has taken the old name. Two paths must
differ only in case *and* be one object for any of them to relax; two hard links
to one inode are still two names, and moving one onto the other is refused.

The panel that ran it is pointed at the new name in the layout half of the same
action, because a panel is on the thing and not on the spelling: the card
becomes the card on the renamed file, the listing the listing of the renamed
directory. Every *other* panel on the old name keeps it and says so, exactly as
one does after a delete.

Every path in a batch is attempted. The toast says what went and appends what
did not, and what a verb leaves marked is exactly what it could not do. If
nothing succeeded, the panel's status line carries the refusal and no history
node is created.

The real disk never overwrites. A copy and a move claim their destination
exclusively, because `std::fs::rename` and `std::fs::copy` both replace
silently; the one destination exempt from the claim is the source itself under
another case, which is not an overwrite at all. A copy that fails removes only the destination it created itself, and
an existing destination is never touched. A move across filesystems copies
first and trashes the source second, and takes its own copy back off the disk
if the source cannot be trashed. A symbolic link is copied as a link; a FIFO,
socket, or device node is refused by name.

Every writing verb and `open` are refused outright on a scripted run
against a real disk, in one sentence, because a suite must no more delete a
human's files than write to their keychain. `--demo-disk` gives a run the
kernel's writable demo tree instead, which is what the file suites use.

A verb checks that the device may write before it touches the disk, because the
disk would take the write even where the store will not. If the lease turned
over in between, the write is reversed and the panel says so. See
[Device Sync](./device-sync.md#the-lease).

## Undo

Copy, move, rename, new directory, and delete are each one undoable action,
including when they act on marked rows. Undo restores the marks the batch
consumed.

Every reversal asks the disk before it acts rather than trusting what it
remembers, and it asks about the **object**, not the path: a name is cheap to
reuse and a reversal removes what it finds. Each intent records the device and
inode the write landed on, and refuses when nothing is there, when the disk
will not say, or when the id has changed. A node that cannot be reversed
expires and history moves past it instead of pretending.

Undoing a copy trashes what the copy made; undoing a delete moves the item back
out of the trash; undoing a rename moves the old name back, and refuses when
something else has taken it; undoing a new directory refuses while the
directory is no longer empty. Nothing here ever removes a path a person had: the only true
removal in the app is sweeping away a half-made copy nobody has seen.

A reversal over a batch attempts every path and then names the ones that would
not go, rather than stopping at the first.

## Refreshing

A verb that wrote refreshes every open files panel: each list relists keeping
its filter, cursor, and marks, and each card restats. Undo and redo are covered
differently: the app keeps a count of its own writes, and each panel compares
it on every draw and event, so a reversal that no verb ran still lands.

The app does not watch the disk. A change another program makes is not noticed
until something else refreshes the panel; see
[Open Questions](./open-questions.md).
