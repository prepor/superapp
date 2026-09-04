# Files

The files app browses this machine's disk: a directory is a list, a file is a
card, five verbs write, and one copies a name. It stores nothing, because the
disk is the state, so it has no schema, no seed and no queued jobs. What it
adds is two panel kinds, one launcher root, a clipboard other apps may read,
and one worker: the pass that performs the verbs that write, off the thread
that draws — see [Runs](#runs).

It reaches the disk through the kernel's `Disk` [capability](./apps.md#capabilities),
in the display spelling the panels use, and the kernel translates `~` to a real
path at the boundary. A restored panel therefore points at the current device's
home directory. It hears about somebody else's writes through `Watcher`, the
capability beside it.

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
A caret in one of the card's two selectable runs keeps the four text chords and
nothing more, so while a path is selected `cmd+c` is the selection's and `cmd+d`
is still *delete*. The `rename` field keeps every letter, as any field with a
caret in it does.

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

Each writing verb is an [effect](./data-substrate.md#effects). None of them is
performed on the thread that draws: `new dir`, `copy here`, `move here` and
`delete` hand their paths to a **run**, and a background pass performs them one
at a time. See [Runs](#runs) below for what that costs and what it buys.

`copy here` and `move here` plan against the disk as it is when the run starts,
then perform one path at a time. A path is refused, by name, when:

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

## Runs

A directory of forty thousand files is a copy that takes minutes. Performed on
the frame of the click it would be minutes with nothing drawn, nothing
scrolled, and no way to press the one button a person wants — so the four verbs
that write queue a run instead, and a background pass performs it a path at a
time.

Four things hold, and they are what the design is for:

- **The same disk.** A run performs the same effects the panel did, through the
  same `Disk` capability. The shell installs the machine's filesystem on the
  *environment* rather than on the window's world, so every world built from it
  — the window's and the runner's thread alike — is handed the same
  implementation, with the same refusals, the same exclusive claim on a
  destination, and the same trash. There is no second copier to drift from the
  first.
- **It says where it is.** While a run is on, every files panel draws
  *copying 12 of 340 — "report.pdf"* under its header, in place of the line a
  refusal leaves. The line is the app's, not the panel's: it is one disk, and a
  run started in one panel writes into directories others are showing.
- **It can be stopped.** *cancel* is the first verb on every files bar while a
  run is on, and stops it from wherever anybody is looking. It carries no
  letter: it is the only verb here that undoes nothing, and no chord should be a
  keystroke away from stopping a copy — which is also why it goes first, since a
  bar is a row and never a wrap, and a verb past the right edge is not drawn at
  all. The path in hand is finished — a half-copied file is nobody's — the runs
  waiting behind it are dropped, and the toast says how far it got and what that
  cost — even a run that managed nothing of its own still answers for what it
  took with it. A stop that finds nothing started yet drops what was queued and
  says so itself: work is never dropped in silence. And *cancel* is about the
  run whose line was on screen when it was pressed: the line and the run it is
  about are one sample, taken as the panel **draws**, so a run that finished in
  the intervening frame is not its successor's to answer for. A *cancel* drawn
  when nothing had started yet is about the queue instead — what is still
  waiting never starts, and one that came out of that queue since is stopped
  where it is.
- **Undo is unchanged.** The run records nothing. It collects what it performed
  and hands it back; the history node, its intents, the lease check, the marks a
  delete consumed, the panel a delete closes and the toast are all the UI
  thread's, one frame later. A run that was stopped halfway lands what it
  managed, because a change with no node behind it is a change nobody can undo.

A run carries the panel that asked for it — the slot *and* what stood in it —
because a slot is a place and not a panel: a crumb and `go to` both replace
what a slot shows, in place, without closing anything. A run that lands after
that writes no line on the stranger standing there, takes none of its marks,
and above all does not close it. The same holds for everything else it may
have outlived: a move lets go of the clipboard it *carried*, and a set held
since stands; `new dir` closes its field on the name it made and on no other,
so a name typed while the run was out survives; and the marks a delete
consumed are worked out from what *went*, not from what is still marked when
it lands, since the rows disappear one at a time and each draw takes their
marks with them. Where the lease turns over and the trash is given back, the
marks go back on with the rows: the node that would have carried them is never
recorded, so nothing else would.

Every session performs its own runs, one at a time — the window's and each
mount's are separate hands, not one between them, or the session whose entry
was lost would read as idle and have its worker retired mid-run. The worker
exists exactly while its session has something to perform: an action retires
it as it retires any pass, and a run that ends with no action to record — one
refused outright, one given back to the lease, one cancelled before it started
— kicks the workers itself rather than leaving a thread on a store reader.

Panels keep up with a run through the same write count as ever, so a listing
fills as the copy lands in it. A card, though, asks whether the file it is on
actually moved before it reads again: every path a run performs bumps that
count, and a card that re-read on each of them would hand its widget the same
picture to decode once a frame for the length of the run.

Runs are performed one at a time in the order they were asked for, and each is
planned when it reaches the front — the disk may have moved on while it waited,
and nothing watches a disk. Every run is stamped with the session that asked
for it, by the address of the one database that session and its workers share:
a process may be running several sessions — the window's, and one per mounted
scene in the [panels library](./panel-model.md) — and a run performed by
another session's pass would write another session's disk, while a node
recorded there would be a node in the wrong history, on a slot number that
means nothing. A run is *not* a queued
[job](./data-substrate.md#effects): the store's queue is for work that is
retried and outlives the process, and a copy is neither. Nobody may replay a
trash on the next boot; if the process goes away mid-run, what it had already
written stands, exactly as a force-quit mid-copy always did.

Where the background passes run inline — under virtual time, which is every
scripted run, and in every test — one pass is the whole run, so a scripted
`wait` is followed by the run's consequences in the same tick, as everything
else inline is.

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

A run that wrote refreshes every open files panel when it lands: each list
relists keeping its filter, cursor, and marks, and each card restats. A run
still going is covered by the same count the app keeps of its own writes — each
panel compares it on every draw and event — so a long copy fills the listing it
lands in as it goes, and so does a reversal that no verb ran.

Another program's write is the other half, and the kernel's `Watcher`
[capability](./apps.md#capabilities) is how it arrives. A panel watches the one
directory it shows for as long as it shows it — a card watches the directory
its file is in — and lets go when it closes; the watcher counts rounds of
change per directory. What a panel compares on every draw and event is
therefore a pair: this app's writes, and the rounds counted for its own
directory. A change in one directory never costs another a reading.

The machine behind the capability is `app/src/platform/watch/`: FSEvents on
macOS, inotify on android, and on any other platform nothing at all, which
leaves a panel refreshing on its own writes alone. Watching is per directory
and never recursive, so a build running three levels down is not a listing's
business. A directory taken out from under a panel — moved, renamed or deleted
— is a change to that directory and reported as one, so a listing never goes on
drawing a path that is not there. So is a path that leads somewhere else than
it did: both instruments watch what a path resolved to, so what each path leads
to is asked again on every turn, and a repointed link is a change to the panel
showing it. When the platform says only that events were lost, every watched
directory is reported and every watch retaken: a listing that may be stale and
one known to be stale are worth the same reading. Events are otherwise grouped
twice: the platform coalesces a burst into one delivery, and a delivery bumps a
directory once however many paths it carried — a thousand-file copy is one
reading, not a thousand. The watching thread rings the UI thread's bell, and
the shell redraws; each panel then works out for itself whether the round was
its own directory's.

A scripted run is never watched, real disk or demo tree: a suite's frames may
not depend on what the machine it runs on happens to be doing.
