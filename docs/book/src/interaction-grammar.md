# Interaction Grammar

A small vocabulary with sharp semantics. From looking at any element you must
know what it does; nothing may reuse a signal to mean something else.

## The three interactive signals

| Signal | Meaning |
|---|---|
| solid underline | opens a panel to the right, **joined** to this one |
| dotted underline | **replaces** the panel it lives in |
| bordered button | **side effect only** — never navigation |

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

A preview is a real open — joined, marks the mail read, undoable — minus the
one thing that would end the walk. It opens **immediately**, on every step,
with nothing queued between the cursor and what it points at; a whole run of
them coalesces into a single history node, so one `cmd+z` takes it all back.

Only a panel that *has* a cursor over a list can preview, and it previews into
exactly one kind (the inbox into a message). The pair reads as one thing,
which is also why it borrows keys — see [Accelerators](#accelerators).

A preview needs somewhere to *be*. Where the pair cannot share the screen —
a phone grid, where each panel is the whole of it — the open simply goes, as
any other open would; a preview nobody can see is worse than no preview.

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
  walk the rows (scrolling the list to keep the cursor visible, and
  **previewing** each one beside it). In the filter, `@` opens the tag
  autocomplete: arrows walk it, `enter`/`tab` take, `esc` puts it away — see
  [The Rich Table](./richtable.md) for the grammar
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
`cmd+a`, `delete` is `cmd+d`, `reply` is `cmd+r`, the message walk is
`cmd+n` / `cmd+o` on `← newer` and `older →`, the inbox's `sync` is `cmd+s`,
settings' `add account` is `cmd+d`. Nothing to memorise and no help panel to
consult — the shortcut is a property of the thing it fires.

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
are one working surface, so they pool their accelerators: with a mail
previewed, `cmd+a` archives it, `cmd+d` deletes it, `cmd+r` replies and
`cmd+n` / `cmd+o` walk — all without leaving the list. The driver's own keys
win first (`cmd+s` still syncs the inbox), and the preview lends what is left.

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
match, so `vera q3` narrows to her budget mail.

Each hit carries one of two verbs, decided for you:

- already open somewhere → **go to it** — switch workspace, focus it; the
  row wears its workspace number (`#3`);
- not open → **open it** — a fresh un-joined trailing column on the active
  workspace; the row says *new*.

There is never a second copy: an open panel absorbs its would-be duplicate
(no "force a fresh copy" variant — deliberately). The empty query is the
pure **switcher**: every open panel, the active workspace's first, roots
beneath. Arrows pick, `enter` goes, `esc` (or another double-cmd, or a tap
outside) dismisses; every row is clickable. A tap only counts as a tap:
holding cmd for a chord, cmd+clicking, or overshooting ~350 ms between taps
never summons it.

On desktop the launcher is also in the menu bar (*Launcher — ⌘ ⌘*); on touch
it is the search row atop the workspaces overlay — tapping it flips the
overlay into the launcher and raises the soft keyboard. When real kinds
arrive (telegram, rss, kb), each contributes its entries to the same query —
this surface is where global search lives.

## Undo

**Every action is undoable** — open, close, replace, move, column ops,
workspace moves, archive, delete — and `cmd+z` walks them back with their
whole delta: undoing an archive restores the panel *and* the mail's folder;
filing a mail carries the list's cursor to the next one in the same node, so
one `cmd+z` takes back the filing and the move together;
undoing an open makes the mail unread again exactly if the open unread it.
Undo also puts you back where the action happened (its workspace and focus
revert with it). `cmd+shift+z` re-applies.

History lives **in memory** and dies with the process: quit and your undo
tree is gone, though nothing you did is. The distinction matters and is
worth stating plainly — a send you fired seconds before a crash still goes
out on the next launch, you simply cannot call it back; yesterday's archive
cannot be undone today. The rows every action wrote are durable, and the
background passes read only those, never the tree.

What is deliberately *not* an action: focus walks, workspace switches,
camera pans, row selection — context, not intent. They persist, but they
never become history nodes; undo restores them only as part of a real
action's delta. Rapid bursts of the same gesture on the same panel (arrow
moves, j/k reading walks) **coalesce** into one node — one `cmd+z` takes
back the whole burst.

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
- **one-finger sideways drag on a mail row** — triage: **left archives,
  right deletes**. An ink **curtain** wipes across the row carrying the name
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
