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
- `cmd+w` — close the focused panel
- `cmd+[` / `cmd+]` — consume into / expel out of a column;
  `cmd+,` / `cmd+.` — pull from the right / push the bottom out
- `cmd+t` — toggle column tabs
- inbox: `j`/`k` row cursor (scrolls the list to keep it visible), `enter`
  opens (`cmd+enter` un-joined), `/` filter
- message: `j`/`k` older/newer in place, `r` reply
- `esc` leaves a text field; arrows scroll a panel that has nothing better to do

Letter keys reach panels as text input, so key repeat and IME behave like
typing; control keys (enter, arrows, backspace) are routed as key events.

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
- **two-finger horizontal drag** — pans the workspace strip, 1:1 while the
  fingers are down; on release the camera **magnetises** to the nearest
  column alignment (a column's left edge one gap in from the viewport's
  left, or its right edge one gap in from the right) and springs there.
- **long-press a panel header** — picks the panel up; it rides the finger
  (spring-following, so it trails with the same physics as everything else).
  The drop point re-places it: inside a column's middle it **stacks** into
  that column at the row under the finger; near a column edge, in a gap, or
  past the strip it becomes a **fresh column** at that boundary.

One finger decides what it is (tap / scroll) after an 8 pt slop; a second
finger anywhere turns the gesture into a pan. Gestures that come to nothing
go inert until every finger lifts — no surprise mode flips mid-gesture.
