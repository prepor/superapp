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

## Keyboard

**Cmd is the workspace modifier** (niri's Mod; mosaic made the same choice).
Everything below it belongs to the focused panel's content — which is what a
future text-editor panel needs: the whole plain keyboard, no modes.

- `cmd` + `←↓↑→` / `hjkl` — focus panels; `+shift` — move the focused panel
- `cmd+1…9` — switch workspace; `cmd+shift+1…9` — move the focused panel to
  that workspace and follow it
- `cmd+w` — close the focused panel
- `cmd+z` / `cmd+shift+z` — undo / redo (see below)
- `cmd+[` / `cmd+]` — consume into / expel out of a column;
  `cmd+,` / `cmd+.` — pull from the right / push the bottom out
- `cmd+t` — toggle column tabs
- inbox: `j`/`k` row cursor (scrolls the list to keep it visible), `enter`
  opens (`cmd+enter` un-joined), `/` filter
- message: `j`/`k` older/newer in place, `r` reply
- `esc` leaves a text field; arrows scroll a panel that has nothing better to do

Letter keys reach panels as text input, so key repeat and IME behave like
typing; control keys (enter, arrows, backspace) are routed as key events.

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
  row wears its workspace number (`№3`);
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
workspace moves, archive — and `cmd+z` walks them back with their whole
delta: undoing an archive restores the panel *and* the mail's folder;
undoing an open makes the mail unread again exactly if the open unread it.
Undo also puts you back where the action happened (its workspace and focus
revert with it). `cmd+shift+z` re-applies.

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
back out again; the overlay stays up, because browsing is the point. A
delivered send shows as `· sent` and is transparent to the walk — physics,
not data. Under the hood every action's transaction is recorded as an
invertible SQLite changeset; undoing applies the inverse, and rows the
world changed since (a sync, another device someday) are skipped rather
than forced. The menu bar carries *Undo / Redo / History* items; toasts
name what happened.

## Mouse and trackpad

Every action is also reachable by mouse: click focuses, × closes, links and
buttons are hit-tested exactly (hover states and cursor shapes mark them).
Horizontal trackpad scroll pans the strip 1:1; vertical scroll scrolls the
panel body under the pointer. A scrollable body shows a minimal grey thumb on
its right edge; list panels pin their filter and table header above the
scrolling region.

## Touch (android)

The same grammar, re-based on fingers:

- **tap** — exactly a click: follow a link, press a button, focus a panel.
  There is **no touch equivalent of cmd+click**; a solid link always follows
  join semantics on glass.
- **one-finger vertical drag** — scrolls the panel under the finger, 1:1.
  A sideways one-finger drag means nothing (deliberately: it would fight
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
