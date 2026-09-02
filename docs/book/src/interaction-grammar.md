# Interaction Grammar

A small vocabulary with sharp semantics. From looking at any element you must
know what it does; nothing may reuse a signal to mean something else.

## The three interactive signals

| Signal | Meaning |
|---|---|
| solid underline | opens a panel to the right, **joined** to this one |
| dotted underline | **replaces** the panel it lives in |
| bordered button | **side effect only** — never navigation |

Nothing draws a dotted link at the moment: the message panel's newer/older
walk was the last one, and the inbox cursor does that job now (see
[Preview](#preview-the-one-open-that-does-not-go)). A list's
[marks bar](./richtable.md#marks) is bordered buttons throughout: `archive`,
`delete`, `all` and `clear` act on the marked set and go nowhere.

**Cmd+click** (or cmd+enter in a list) always opens a fresh, **un-joined**
panel — the workspace modifier means "workspace-level" with the mouse too
(alt is kept as a quiet alias). Side-effect feedback is a transient toast in
the bottom-right corner; errors are the only place colour appears.

### Preview: the one open that does not go

A **list panel's cursor opens what it lands on, without leaving the list.**
Walking the inbox with the arrows — or clicking a row — re-targets a joined
message beside it; focus stays on the rows, so the next arrow keeps walking
rather than scrolling a message body, and reading a list never costs a trip
back.

So the split is: **touching a row previews, `enter` goes.** Enter is the one
that hands focus over, which is the solid-link rule above, unchanged; `cmd+→`
is the other way there. Cmd+click still means what it means everywhere — a
fresh, un-joined panel.

A preview is a real open — joined, undoable, and (for a mail) marking it
read — minus the one thing that would end the walk. It opens **immediately**,
on every step, with nothing queued between the cursor and what it points at;
a whole run of them coalesces into a single history node, so one `cmd+z`
takes it all back.

Only a panel that *has* a cursor over a list can preview, and what it
previews into is **the kind its row names** — the inbox a message, the effect
log a job, a files panel a sub-directory or a card, depending on the row. The
pair reads as one thing, which is also why it borrows keys — see
[Accelerators](#accelerators). What a preview *establishes* is the domain's,
not the grammar's: opening a mail reads it, while looking at a job or a
directory leaves the world exactly as it was.

The three list panels say the same three things about the row under the
cursor — open it, preview it, put the cursor on it — and the shell answers
each the same way whatever the row is, so there is one of each verb rather
than one per table. A click on a row is the one door too: the list takes
focus, its cursor follows, and what the row names previews beside it.

### Threads: the row is the conversation, the panel is the whole of it

An inbox row is a **thread**: every mail of a conversation counts, and it is
a row while at least one of them sits in the inbox. The row names who wrote
in it — newest speaker first, `me` for your own address, first names once
there are two — and the count past one (`Max, me · 3`), the subject with its
`Re:` stripped, and the date of the latest inbox mail; it is bold while any
of them is unread. Filing the row files every inbox mail of the thread in
one undo node. A reply arriving later puts the thread back by itself.

A message panel shows the thread its mail belongs to, oldest first, the
account it came to once at the top and `forward` and `reply` at the foot —
both on the conversation's newest mail. A **reply** answers it: TO and
SUBJECT prefilled, the cursor in the body, and the send threads to it. A
**forward** passes it on: the letter sits in the body under a header block
(who wrote it, about what, when, to whom), SUBJECT is prefilled and TO is
empty, so the cursor lands there. A forward is not a reply — it names no
parent — but it carries the conversation's chain, so it threads for anyone
who already has it, your own Sent folder included. Once it has gone, the
mail it passed on wears a muted `↪` by its date, in every client that
reads the `$Forwarded` keyword. **Each message is one row, open or
closed.** Closed, the row is the
sender, the first line they wrote in grey (or the status line, red when it
is an error) and the date.
Open, the same row is the sender as a contact link and the date, with the
letter unfolded under it; its quoted tail — the message above, usually —
sits folded behind `› quoted`. Touching a closed row opens it in place;
touching an open one's header closes it; touching the fold unfolds it. None
of that is an action: like the inbox cursor it is panel context, no history
node, gone with the process. There is no chord for it — a panel has many
rows, and rule 4 gives chords only to controls a panel has one of.

**Opening a thread reads it.** What starts open is what was new: the mail
the row pointed at (the oldest unread inbox mail, else the newest) and every
mail that was unread — and the open marks all of them read, one intent each,
so one `cmd+z` puts every flag back. After a restart a panel comes back with
only its own mail open; "what was new" dies with the process, as undo does.

A preview needs somewhere to *be*. Where the pair cannot share the screen —
a phone grid, where each panel is the whole of it — the open simply goes, as
any other open would; a preview nobody can see is worse than no preview.

## Problems

A toast is an **event**; a problem is a **condition**. When something in
the background is wrong — an account whose last sync failed, a send the
sender gave up on, a device-sync bucket that cannot be reached — the toast
that announced it fades in three seconds, and what remains is the
**mark**: a small red box in the toast's own corner, bottom-right on both
platforms, that counts what stands (`2 problems`) and stays until the
conditions clear. It is static, deliberately: the shell idles at zero
frames, and red on a monochrome screen is already the alarm — that is the
one job the colour has. It says what it is in words, because a dot would
not.

The mark is the launcher's verb in one click: it goes to the **problems
panel** where that is open, or opens it fresh. The panel is a root like
settings (the launcher finds it as *problems*) and lists every standing
problem as a row — what it concerns, the error in red, a muted line under
it (the last successful sync, the recipient, the frames waiting to
publish) — with what can be done about it. An account offers *sync* and a
link to *settings*. A failed send offers *retry*, a button that files the
send again with its usual window, and *reopen*, a solid link that brings
the draft back as a compose panel joined to the right (`cmd+z` puts the
failed row back). Device sync offers only its count: the network coming
back is what fixes it. With nothing standing, the panel says so. `tab`
walks each row's button and link and `enter` presses; there are no chords,
since a panel with a control per row gives none.

The list is **derived**, never stored: it is read off the account's status
line, the outbox's failed rows and the lease status the sync worker last
reported, so it cannot disagree with them and nothing ever has to be
dismissed — fix the condition and the row is gone. On macOS the menu bar
mirrors it the way it mirrors the workspaces: a `! 2 problems` menu that
exists only while something stands, one item per problem, each opening
the panel. It is plain text; AppKit draws menu titles itself, and the
colour lives in the window.

### Files: a directory is a column, a file is a card

Two rules, and the rest is the grammar already described:

> **A directory is a list panel. A file is a card.**

A `files` panel is [the rich table](./richtable.md) over one directory,
non-recursive: name (a directory wears a trailing `/`), size, modified;
directories first, then names, case folded. Dot-files wait for `@hidden`.
The filter is the inbox's grammar over other tags — `@dir`, `@hidden`,
`@kind:` (image · text · pdf · archive · other, off the extension),
`@size>`, `@modified>`.

The walk is the preview walk, so the browser is Finder's column view for
free: the cursor previews a sub-directory as the next column or a file as
its card, `enter` goes, the next row replaces what was previewed, `cmd+w`
closes a level and `cmd+←` steps back. Above the rows a crumb line —
`~ / Downloads / 2026` — spells the way back, each ancestor a **dotted**
link that replaces the panel in place; they are the first dotted links
since the message walk went, and exactly what that underline is for.

**`go to`** (`cmd+g`) turns the crumbs into a path field, seeded with where
the panel stands and a slash. Each segment completes like a shell's tab —
the entries of the directory the segments before it name, a directory
landing with its own slash so the next offer opens at once, the two roots
`~/` and `/` before the first slash; `tab` takes the offer, `enter` goes to
what is typed even with the offer open, `esc` puts the crumbs back. A
directory replaces the panel, a file opens its card joined, a path that is
not there is refused on the status line. `.` and `..` are read; a relative
spelling is not. A second root typed after the seed **restarts** the path
(find-file's rule): `~/Downloads//tmp` is `/tmp`, so an absolute path wins
without clearing the field. That is how the browser leaves `~`.

The **card** is the file's name, its kind and size, when it changed, and its
path as selectable text. Under a rule it previews what it can: the first 64
KB of a text file, or a PNG or JPEG fit to the column — decoded by the
bytes' own magic rather than the name, so a picture saved under the wrong
extension still draws. Anything else, a PDF included, is the card alone, and
**`open`** (`cmd+o`) is how you read one: the path goes to the OS, which
picks the viewer. Nothing is executed by us. A card measures its preview and
asks for the rows it needs, the way a long letter does — a long file opens
tall rather than scrolled, a short one stays short.

The rest of the verbs are drawn and their grammar settled, but they do not
touch the disk yet (see [Open Questions](./open-questions.md)). What the
grammar says: every verb acts on **the thing the panel shows** — a card's on
its file, a files panel's on its directory — never on a row, and the list
reaches them by borrowing, exactly as the inbox borrows `archive`. So a
files panel wears `copy`, `move` and `delete` only while it is the **end of
a chain**: joined under a parent and driving nothing, which is to say the
thing under someone's cursor. A root, a list opened from the launcher, or a
list that is itself driving a preview wears `new dir` and `go to` alone —
`~` cannot be deleted, and a chord pressed in a list never hits the
directory the list itself shows; it hits what the cursor is on, one column
over, where the mark is.

Where a driver and its preview share a key for the same verb — two files
panels both wearing `new dir` — the driver's wins, and the shell draws the
preview's shadowed mark **plain**, so no bold letter ever promises a chord
the driver would take. Two *different* verbs on one key stay forbidden by
test; the same verb twice is allowed exactly because the mark stays honest.

Destination is named by walking there rather than by a dialog: `copy`
(`cmd+p`) or `move` (`cmd+m`) **holds** the object, and every files panel
then offers `copy here` or `move here` (`cmd+h`), which performs into the
directory that panel shows. A move clears the hold, a copy keeps it. The
hold is context, not history — `cmd+z` never takes it back, and it dies with
the process. `new dir` (`cmd+n`) opens a one-line field above the rows;
`enter` creates, `esc` puts it away, and a name that is taken or holds a
separator is refused on the status line. Note `copy` wears `p`, not `c`: a
card's path is selectable, so rule 3 leaves `cmd+c` to the text. The file
clipboard is not the text clipboard.


## Keyboard

**Cmd carries two namespaces.** The **reserved set** below is global and
fixed — it means the same thing on every panel, forever. Everything else
under cmd belongs to the **focused panel**, which spends it on
[accelerators](#accelerators). Plain letters stay free: no modes, and the
whole keyboard is still there for a future text-editor panel.

- `cmd` + `←↓↑→` — focus panels; `+shift` — move the focused panel
- `cmd+1…9` — switch workspace; `cmd+shift+1…9` — move the focused panel to
  that workspace and follow it
- `cmd+w` — close the focused panel
- `cmd+z` / `cmd+shift+z` — undo / redo (see below)
- `cmd+[` / `cmd+]` — consume into / expel out of a column;
  `cmd+,` / `cmd+.` — pull from the right / push the bottom out
- `cmd+t` — toggle column tabs
- `cmd+u` history · `cmd+i` copy the panel's context

That is the whole reserved set: `1…9`, the arrows, `w z u i t [ ] , .` and
`enter`. A panel may claim any other letter.

Per panel, below the reserved set:

- inbox: `enter` opens *and goes* (`cmd+enter` un-joined), `/` filter, arrows
  walk the rows — threads — (scrolling the list to keep the cursor visible,
  and **previewing** each one beside it). `space` **marks** the cursor's row
  — it arrives as text the way `/` does, so in a live filter it is a space —
  `shift+↓` / `shift+↑` mark a range as they walk, and `esc` clears the
  marks when no field is listening. A click in a row's left 12 pt gutter
  toggles its mark, `shift+click` marks from the cursor's row to the clicked
  one, and the row's body still previews. In a message panel the arrows
  scroll; its rows open and close by pointer only. In the filter, `@` opens the tag
  autocomplete: arrows walk it, `enter`/`tab` take, `esc` puts it away — see
  [The Rich Table](./richtable.md) for the grammar
- compose: in TO, typing offers the senders the store knows, by name or
  address — the same box, the same keys; `enter` and `tab` take the
  address and stay in the field, so a comma starts the next one
- forms: `tab` / `shift+tab` walk the fields **and the buttons** — one ring,
  wrapping; `enter` advances and **submits past the last field**. Read
  panels have no ring: their controls wear chords instead.
- `esc` leaves a text field; arrows scroll a panel that has nothing better to do

There is no vim layer: `hjkl` and the plain `j`/`k` walks are gone. A key is
an arrow, a cmd chord, or typing — nothing in between.

Letter keys reach panels as text input, so key repeat and IME behave like
typing; control keys (enter, arrows, backspace) are routed as key events.

## Accelerators

**A control carries its own key, drawn into its label.** One character of a
button or link is **bold**, and `cmd`+that letter fires it: `archive` is
`cmd+a`, `delete` is `cmd+d`, `reply` is `cmd+r`, `forward` is `cmd+f`, the
inbox's `sync` is `cmd+s`, settings' `add account` is `cmd+d`. Nothing to
memorise and no help panel to consult — the shortcut is a property of the
thing it fires.

This is the one place bold does a second job, so the rule is sharp:

> A bold **run** is emphasis (unread rows, a contact's name). A bold **single
> character inside a bordered button or an underlined link** is that
> control's key.

The two never share a place: emphasis is never applied to a control's label,
and controls are already marked by border or underline.

Accelerators are **on cmd, not on bare letters**, so they work while a text
field owns the keyboard — you can archive mid-sentence in a compose body, and
the mark never has to lie about whether it is live. They are also
**panel-scoped**: `cmd+a` archives on a message, and stays select-all in a
compose body. Five rules keep that honest, enforced by unit test rather than
by discipline:

1. never the reserved set;
2. unique within a panel;
3. a panel whose text can be edited *or selected* yields `c` `v` `x` `a` to
   it — which is why settings' one link is `cmd+d`, not `cmd+a`: the account
   rows are selectable, so select-all stays theirs;
4. only for controls a panel has exactly one of — a list of rows each with a
   *remove* button stays on the Tab ring and the mouse;
5. **a panel that previews borrows its preview's keys** — see below.

### Borrowed keys

A [preview](#preview-the-one-open-that-does-not-go) and the list driving it
are one working surface, so they pool their accelerators: with a thread
previewed, `cmd+a` archives it, `cmd+d` deletes it, `cmd+r` replies and
`cmd+f` forwards — all without leaving the list. The driver's own keys win
first (`cmd+s` still syncs the inbox), and the preview lends what is left.

The mark stays honest because it never moves: it is drawn on the message
panel's own chrome, one column over and in plain sight. Nothing is ever bold
on the borrower — a borrowed key is a property of the panel you can see, not a
hidden binding on the panel you are in. That is also why refresh became
`sync`: two visible controls may not answer to one letter, and `reply`'s `r`
was already spoken for.

Borrowed keys **stand down while the driver's own text field holds the
keyboard**, so `cmd+a` in a live filter is still select-all. Rule 3 survives:
the list yields the text chords to its field exactly as before, and only
lends them when the field is not listening.

They stand down for the [marks bar](./richtable.md#marks) too. While a list
has marked rows the bar wears `a`, `d` and `l` itself — a batch verb is the
same verb on a wider set, so it takes the same letter — and the borrowed
chords go quiet, because two visible controls may not answer to one chord.
The guard is the filter's: with the field holding the keyboard `cmd+a` is
select-all, not an archive of the set.

## Workspaces

Nine numbered spaces on a vertical stack; a switch **slides** the viewport a
workspace-height down or up (see [Panel Model](./panel-model.md)). On macOS
the **menu bar mirrors them**: one menu per workspace worth showing — every
occupied one plus the first empty slot — with the current number bracketed,
and *Switch Here / Move Panel Here* items inside. (The bold app menu itself
is AppKit-mandatory; it holds only Quit.) An empty workspace names itself in
muted text, so switching onto a blank screen reads as a place, not a bug.

On touch, a **two-finger swipe down** raises the workspaces overlay: a
*search* row (the launcher's entry, below), then one row per workspace
(number + its panel titles, the current row inverted, the first empty slot
offered as *new*). A tap on a row switches, a tap outside — or a two-finger
swipe up, or `esc` — dismisses. There is no touch gesture for *moving* a
panel between workspaces yet (see [Open Questions](./open-questions.md)).

## The launcher

**Double-tap cmd** (the workspace key itself — no letter spent) and a modal
query field rises over an ink wash. One query runs over *everything that can
be a panel*: the open panels on every workspace, the root panels, and the
mail world — contacts by name and address, mails by subject and sender, the
same word-by-word substring semantics as the inbox filter. Every token must
match, so `vera q3` narrows to her budget mail. A root also answers to the
word one actually reaches for rather than only to its own name: the effect
log is found by *log* and *queue* as well as by *effects*.

Each hit carries one of two verbs, decided for you:

- already open somewhere → **go to it** — switch workspace, focus it; the
  row wears its workspace number (`#3`);
- not open → **open it** — a fresh un-joined trailing column on the active
  workspace; the row says *new*.

There is never a second copy: an open panel absorbs its would-be duplicate
(no "force a fresh copy" variant — deliberately). The empty query is the
pure **switcher**: every open panel, the active workspace's first, roots
beneath. Arrows pick (the list is a ring: past the last hit is the first),
`enter` goes, `esc` (or another double-cmd, or a tap outside) dismisses;
every row is clickable. The sheet is as tall as its hits; a query nothing
answers says *nothing matches*. A tap only counts as a tap: holding cmd for
a chord, cmd+clicking, or overshooting ~350 ms between taps never summons it.

On desktop the launcher is also in the menu bar (*Launcher — ⌘ ⌘*); on touch
it is the search row atop the workspaces overlay — tapping it flips the
overlay into the launcher and raises the soft keyboard. When real kinds
arrive (telegram, rss, kb), each contributes its entries to the same query —
this surface is where global search lives.

## Undo

**Every action is undoable** — open, close, replace, move, column ops,
workspace moves, archive, delete — and `cmd+z` walks them back with their
whole delta: undoing an archive restores the panel *and* the mail's folder;
filing a thread carries the list's cursor to the next one in the same node,
so one `cmd+z` takes back the filing and the move together;
undoing an open makes every mail it read unread again, and none other.
A batch verb is one node as well: archiving twelve marked conversations
files every inbox mail of every one of them under a single `archive 12
conversations`, and undoing it brings the rows back **marked**.
Undo also puts you back where the action happened (its workspace and focus
revert with it). `cmd+shift+z` re-applies.

History lives **in memory** and dies with the process: quit and your undo
tree is gone, though nothing you did is. The distinction matters and is
worth stating plainly — a send you fired seconds before a crash still goes
out on the next launch, you simply cannot call it back; yesterday's archive
cannot be undone today. The rows every action wrote are durable, and the
background passes read only those, never the tree.

What is deliberately *not* an action: focus walks, workspace switches,
camera pans, row selection, a list's marks — context, not intent. They
persist, but they never become history nodes; undo restores them only as
part of a real action's delta. Rapid bursts of the same gesture on the same
panel (arrow moves, the preview walk) **coalesce** into one node — one
`cmd+z` takes back the whole burst.

History is a **tree, not a line**: acting after an undo starts a branch and
destroys nothing; redo follows the newest branch. **`cmd+u` raises the
history overlay** — the whole tree as rows, newest first, indented by
branch depth, the current position inverted, abandoned branches muted but
alive. Clicking any node **travels** there: undo up to the common
ancestor, re-apply down the other side — including *the beginning*, and
back out again; the overlay stays up, because browsing is the point.

Under the hood a node is a **layout snapshot plus zero or more claims on
the world**. The snapshot is what makes navigation free — open, move,
column and close undo by restoring a small typed value — and only genuine
data mutations owe a claim, of which there are six. Each claim decides its
own reversibility: archiving flips intent back and the next sync pass
re-converges, while a send asks its outbox row and refuses once the sender
has taken it. A node whose claims cannot all be given back goes **expired**
and the walk steps transparently past it — a delivered send shows as
`· sent`, because blocking all history behind one sent mail would be wrong
and pretending to undo it would be a lie. Removing an account is expired
from the start: no snapshot brings its mail back, and saying so beats
half-restoring. The menu bar carries *Undo / Redo / History* items; toasts
name what happened. The tree is bounded (200 actions); past that floor
older ones are simply gone.

## Mouse and trackpad

Every action is also reachable by mouse: click focuses, × closes, links and
buttons are hit-tested exactly (hover states and cursor shapes mark them).
Horizontal trackpad scroll pans the strip 1:1; vertical scroll scrolls the
panel body under the pointer. A scrollable body shows a minimal grey thumb on
its right edge; list panels pin their filter and table header above the
scrolling region.

## Touch (android)

The same grammar, re-based on fingers:

- **tap** — exactly a click: follow a link, press a button, focus a panel,
  preview a mail row. There is **no touch equivalent of cmd+click**; a solid
  link always follows join semantics on glass. (On a phone grid a previewed
  panel is the whole screen, so there the tap goes — see
  [Preview](#preview-the-one-open-that-does-not-go).)
- **one-finger vertical drag** — scrolls the panel under the finger, 1:1.
  Vertical keeps ties, so a diagonal is a scroll and never half a swipe.
- **one-finger sideways drag on a mail row** — triage of the whole thread:
  **left archives, right deletes**. An ink **curtain** wipes across the row carrying the name
  of what will happen, entering from the edge that action's button occupies
  in a message header — so the two surfaces agree about which side means
  which verb. Under a third of the way it is a grey wash with the word in
  ink; past that it **inverts**, the same way a header button inverts under
  the pointer, and letting go there fires it. The curtain finishes covering
  the row before the mail leaves the inbox, and a toast offers the undo.
  Let go short of the threshold and it wipes back out.
  Sideways anywhere *else* still means nothing (deliberately: it would fight
  taps and the workspace pan).
- **two-finger drag** — the first move past the slop locks its axis.
  Horizontal pans the workspace strip, 1:1 while the fingers are down; on
  release the camera **magnetises** to the nearest column alignment (a
  column's left edge one gap in from the viewport's left, or its right edge
  one gap in from the right) and springs there. **Vertical, downward, raises
  the workspaces overlay** (upward dismisses it), then the gesture goes
  inert.
- **long-press a mail row** — marks it: the phone's way into a batch (a
  header's long press picks the panel up; a row's was free). While any mark
  exists a tap **toggles** rather than opens, and the last mark cleared
  gives the tap back. The bar's controls are buttons, so nothing about
  marks is keyboard-only.
- **long-press a panel header** — picks the panel up; it rides the finger
  (spring-following, so it trails with the same physics as everything else).
  While held, an **ink insertion bar previews the drop**, judged by the
  *finger* point: a horizontal bar across a column means *stack at that
  row*; a vertical bar in a gap means *a fresh column here*. Near a screen
  edge the camera **auto-pans**, so a drag can reach columns beyond the
  viewport; the drop lands exactly where the preview said.

One finger decides what it is (tap / scroll) after an 8 pt slop; a second
finger anywhere turns the gesture into a pan. Gestures that come to nothing
go inert until every finger lifts — no surprise mode flips mid-gesture.

### The soft keyboard

The on-screen keyboard belongs to **text fields**, not panels: it rises when
a field is tapped and the whole workspace **lifts above it** (the viewport
shrinks by the keyboard's height — no field ever sits under it). Typing
mirrors android's authoritative IME state, so autocorrect, composition and
word-swipe all behave natively. Dismissing the keyboard (back gesture)
leaves the field — the same meaning as `esc` on desktop — and tapping any
field brings it straight back. The keyboard's action button acts as Enter
for single-line fields (in the filter: select the first row and leave).
