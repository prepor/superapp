# CR-003 · Keyboard-centric: declared shortcuts, selectable text

Status: **accepted direction** (Andrey, 2026-09-01: the app must be fully
keyboard-centric; shortcuts should be *visible on the control they fire*;
most content should be selectable; vim navigation goes).

## Why

Three complaints, one root cause: **the keyboard grammar is invisible and
incomplete.**

- Chrome buttons — archive, refresh, send, discard — have **no keyboard path
  at all**. They are mouse-only, and nothing on screen suggests otherwise.
- The keys that *do* exist are undiscoverable. `r` replies, `/` filters,
  `j`/`k` walk — you learn them from the help panel or not at all. A panel
  that can archive shows you a button and hides the key.
- Read-only content is `Label`s, so a message body, an address, an account
  row cannot be selected or copied. The one thing every mail client does.

The fix for the first two is the same idea, and it is Andrey's: **a control
carries its own key, drawn into its label.** `archive` with a bold `a` tells
you what to press by existing. No help panel, no memorisation — the grammar
becomes self-documenting, which is the same argument the underline grammar
already won.

## The model: cmd splits into two namespaces

The book's rule was "cmd is the workspace modifier; everything below it
belongs to the focused panel's content". Accelerators change that, and the
change must be stated rather than smuggled:

- **the reserved set** — global, fixed, panel-independent: digits `1…9`,
  arrows, `w` `z` `u` `i` `t` `[` `]` `,` `.` `enter`, and the system's `q`.
  These mean the same thing on every panel forever.
- **the panel's accelerators** — declared per kind, each drawn onto its
  control. `cmd+a` archives *on a message*; on another panel the same chord
  is free to mean something else, or nothing.

Accelerators are on **cmd, not plain letters** (Andrey's call). The
alternative — bare `a` for archive — dies on the first text field: it can
only fire when nothing is focused, so the mark would have to lie or flicker.
On cmd it **always works**, including mid-sentence in a compose body, and
the mark is honest at all times. That is worth the modifier.

### Letter rules

1. **Never the reserved set.** A panel may not claim a global chord.
2. **Unique within a panel.** Two controls on one panel may not share a
   letter.
3. **Yield the text chords where text is edited.** `c` `v` `x` `a` belong to
   an editable field (copy/cut/paste/select-all). A panel that *contains an
   editable field* may not claim them — so compose takes `s`/`d` for
   send/discard and leaves `cmd+a` to mean select-all in its body. The
   message panel has no editable field, so `cmd+a` = archive there is safe.
4. **Panel-unique controls only.** A list of account rows each with a
   *remove* button cannot have one letter; those stay on the Tab ring and
   the mouse. Accelerators are for controls a panel has exactly one of.

All four are enforced by a unit test over the declaration table, not by
discipline.

## The mark

The accelerator letter is **bold**, inside the control's label. Bold already
means emphasis (unread rows, the contact name), so the grammar gains a
disambiguating rule:

> A bold **run** is emphasis. A bold **single character inside a bordered
> button or an underlined link** is that control's key.

The two never occupy the same place — controls are already marked by border
or underline, and emphasis is never applied to a control's label — so
context separates them without ambiguity.

Drawing is nearly free in both renderers, because both already have the
trick:

- **chrome** (`draw_label`) walks the label character by character to apply
  letter-tracking; the accelerator index simply draws its glyph twice,
  nudged 0.4 — the same fake-bold the char grid has always used.
- **widgets** (`SLink`, `SBtn`) overlay a twin label holding
  `" ".repeat(i) + ch`. The face is monospace, so the padding *is* the
  offset: no measurement, no per-glyph styling, no new shader. `SBold`
  already established the pattern.

## Vim navigation goes

`hjkl` leaves (Andrey: not a fan). Concretely:

- `cmd+h/j/k/l` focus-walk is removed; `cmd+←↓↑→` stays and is now the only
  way to walk panels.
- plain `j`/`k` in the inbox is removed — the arrows already mirror it with
  scroll-follow, so nothing is lost.
- plain `j`/`k` in the message panel (older/newer) is removed and **replaced
  by marked accelerators**: `← ⁠newer` takes `cmd+n`, `older →` takes
  `cmd+o`. This is a straight upgrade — the hidden binding becomes a visible
  one, which is the whole point of the CR.
- plain `r` (reply) becomes `cmd+r`, marked on the link.

Two consequences worth naming. It **frees `h` `j` `k` `l`** for the
accelerator alphabet, which was otherwise tight. And it **retires the plain
letter grammar entirely** — after this pass a key is an arrow, a cmd chord,
or typing, with nothing in between. `/` survives as the filter's focus key
(universal, not a vimism), and plain letters are left open for
find-as-you-type later.

## Selection

Read-only content becomes a **borderless read-only `SField`** (`SText`) —
makepad's `TextInput` with `is_read_only`, themed to look like the `SLabel`
it replaces: no well, no border, no caret.

This buys click-to-caret, drag-select, double-click-word and cmd+c from the
framework rather than from us — the same argument CR-002 made for fields.
Two facts make it safe:

- a read-only input gates off `Hit::TextInput`, so it **cannot swallow the
  panel's letters**; and
- copy arrives as a platform `TextCopy` hit, ungated by read-only, so cmd+c
  works while editing is impossible.

Applied to: the message body, the from/to/date headers, and the settings
account rows.

## Costs, named honestly

- **Cmd is busier.** The reserved set is now something a panel author must
  know, not just a list in the book. The unit test is what keeps that from
  being folklore.
- **A read-only field takes key focus** when clicked, where a `Label` never
  did. On the message panel that means arrows move a caret instead of
  scrolling the body once you have clicked into it — browser behaviour, but
  a change. Escape returns focus to the panel.
- **`cmd+a` is not select-all on a message.** Rule 3 keeps this confined to
  panels with nothing to edit, but it is a real asymmetry across panels.
- **The mark costs a draw per accelerator.** Negligible, but it is a second
  label per control in the widget path.

## Phases

Each lands green (unit + all e2e suites), book updated, committed.

A. **Vim removal + the accelerator core.** The declaration table and its
   four rules under test; the shell resolves `cmd+letter` against the
   focused panel; chrome buttons (archive/refresh/send/discard) get keys and
   marks. `hjkl` and plain `j`/`k`/`r` go in the same commit, since the
   replacements land with it.
B. **Link accelerators.** `SLink` carries an accel char and draws the twin;
   the message panel's reply/newer/older resolve from forwarded chords.
C. **Selection.** `SText` and its three applications.
D. **Complete the reach.** Tab rings for message/inbox/contact, links as
   focusable stops, enter activates a focused link — so every control is
   reachable by walk *and* by key, not one or the other.
