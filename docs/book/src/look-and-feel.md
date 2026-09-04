# Look & Feel

The interface is almost entirely black, white, and grey. Red is reserved for
errors: status lines, error toasts, and the problems count. Focused panel
headers are inverted. Hovered and selected items use a grey background.

The palette is one short list: ink `#141414`, background `#ffffff`, a second
text grey `#5a5a5a`, a muted grey `#909090`, rules `#dcdcdc`, hover `#efefef`,
selection `#e7e7e7`, and error `#a01500`.

## Type

Body text is monospace at about 14 px. Everything is drawn in bundled Geist
Mono on every platform, so a screenshot on one machine matches another, with
Liberation Mono and Noto as fallbacks for what Geist does not cover. Titles,
table headers, and buttons use smaller uppercase text with extra letter
spacing.

A label in a box is centred by its line box, which is where Geist Mono puts a
capital's middle exactly. The close button's `×` is the one glyph the chrome
draws that is not a letter: it rides the maths axis, lower, and so is centred
on its own ink instead.

Bold text has two uses: emphasis in prose, and the one bold letter that marks a
control's [keyboard shortcut](./interaction-grammar.md#accelerators-and-the-bar).
On the bar that letter is drawn by painting its glyph three times a third of a
pixel apart, so the character grid never shifts under a bold mark.

## Panels

Panels have sharp corners, 1 pt dark borders, and 8 pt gaps. They have no
shadows.

A panel is a header, a body, and a bar.

- The **header** is 26 pt tall and wears nothing but the title and the close
  button. The title is truncated to what is left beside that one box. A focused
  header is filled with ink and its title drawn in the background colour; an
  unfocused one is a single rule under the title.
- The **body** is whatever the panel's own widget draws, clipped to what the
  header and the bar leave.
- The **bar** is a 26 pt strip at the foot, under a hairline rule, holding the
  panel's verbs left to right with 6 pt between them. A button is a bordered
  box that inverts on hover; a link is its text with a 1 pt underline. A panel
  with no verbs has no bar at all, and its body gets the space. A bar is one
  row: an entry that will not fit is dropped rather than wrapped.

## Controls

Tables use a strong rule below the header and light rules between rows. A
marked row has a 3 pt bar inside its left padding, so marking it does not move
the text. The cursor's row takes a grey wash. Marked rows hidden by a filter
appear in a separate band above the visible rows, under an upper-case caption
and a dark rule.

A keyboard-focused link has a double underline. A focused button has a grey
background. Containers own spacing; text controls do not add their own padding.

## Motion

Panels and the camera use the same spring (`k=800`, `ζ=1`), which settles in
about 330 ms. Changing a target keeps the current speed. New panels fade and
grow into place; closed panels leave a short fading image. Small alpha changes
use a snappier spring (`k=1600`), so a tab switch is a crossfade in place
rather than an open and a close.

Launcher, workspace, and history overlays use `k=1200`, which settles in about
200 ms: quick enough that a palette feels summoned rather than animated. Their
wash, sheet, and contents ride one spring together. Trackpad movement follows
the input directly and does not use a spring.

The application requests another frame only while something is moving. The
problems count therefore stays still instead of pulsing indefinitely.
