# Look & Feel

Black on white, almost fully monochrome. Colour appears in exactly one role:
errors (`#a01500`-ish ink red) — a status line, an error toast, and the
problems mark that stays in the toast's corner while something in the
background is wrong. There is no second accent; hover and selection are grey
backings; focus is the inverted panel header.

## Type

Everything is monospace (Menlo fronts the family; makepad's bundled fonts fill
in symbols), body ≈14 px. Labels — panel titles, table headers, buttons — are
uppercase, smaller, and letter-tracked (the stelaxis register style). Bold is
a nudged double-draw (unread rows, the contact name). Panel content is laid
out on the character grid the face is measured to, which is what keeps tables,
fields and buttons aligned by construction.

Bold does exactly two jobs, and they never share a place: a bold **run** is
emphasis, a bold **single character inside a button or a link** is that
control's [accelerator](./interaction-grammar.md#accelerators).

## Chrome

1 pt near-black borders, sharp corners, 8 pt gaps, no shadows. Panel header:
26 pt, tracked title, side-effect buttons right, × last. Focused header
inverts; unfocused headers carry a 1 pt rule. Tables: strong rule under the
header row, hairline rules between rows.

A link that holds keyboard focus wears a doubled underline, the way a
focused button wears the grey wash. Text carries no padding of its own: a label, a section label, a link sits
exactly where its row puts it, so every line a panel writes shares the
panel's inset, and the spacing between lines belongs to the rows and rules
rather than to the words. Nothing zeroes or pads around a label at the
site; the vocabulary is bare and the containers carry the rhythm.

## Motion

niri's spring (`k=800, ζ=1`, closed-form, ~330 ms) drives every rect and the
camera; retargeting preserves velocity, so chained motions keep momentum.
Panels are born slightly inset and fade in; closed panels fade out as ghosts.
The modal overlays — launcher, workspaces, history — ride one *presence*
spring (`k=1200`, ~200 ms): the ink wash, the sheet and its rows fade in
together while the sheet rises its last few points into place, and a close
runs the same spring back. Their widget trees render to a texture and are
composited at the spring's alpha, which is how a real text field, caret and
all, fades as one surface. Trackpad pans are 1:1 and deliberately not
springed. The shell idles at zero frames — springs, not timers, request the
next frame — which is also why the problems mark does not pulse: a pulse
would keep the loop turning for as long as a server is down, and the colour
is signal enough.
