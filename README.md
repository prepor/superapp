# superapp — ui prototype

A "user space OS": no apps, no windows — specialized panels on one scrolling
workspace. This is a throwaway web prototype to find the right interaction
model; the real implementation will not be web.

Run: `open index.html` (no build, no deps). `smoke.html` is a headless
regression check of the panel mechanics:

```
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless=new --dump-dom file://$PWD/smoke.html | grep -o '<title>[^<]*'
```

## Model (iteration 2)

- **Panel** = kind + params (`email/inbox`, `email/message {id}`, `contact
  {email}`, `email/compose {re}`, `help`, `about`). Header: title, optional
  panel-global buttons, close.
- **Workspace** = columns on a 12×6 grid, scrolling horizontally
  (niri-style). Panels request grid units (w×h) and get exactly that —
  unused rows at the bottom of a column stay empty.
- **Interactive grammar** (everything must be readable from its look):
  - `button` — side effect only, never navigation
  - solid underline — opens a new panel to the right, **joined** to this one
  - dotted underline (round dots) — replaces the panel it lives in
  - alt+click / alt+enter — always a fresh, un-joined panel
- **Joins**: the next solid link in the parent replaces the joined child.
  A join is alive only while the child sits in the column immediately
  right of its parent; any move or insert that breaks adjacency breaks the
  join. The ═ bridge between the two panels is the (only) indicator and is
  always drawn for a live join. **Replacing a panel closes its joined
  chain** (the chain is context derived from content that just changed).
- **Placement**: open into the column right of the source if the rows fit,
  else insert a new column right of the source. A joined child always
  lands immediately right; an un-joined open goes after an existing pair.
- **Keyboard**: `alt` is the workspace modifier (niri's Mod) — alt+←↓↑→ /
  alt+hjkl focus, alt+shift+same moves, alt+x closes. Plain keys belong to
  the focused panel: inbox `j k enter /`, message `j k r`, esc leaves a
  text field. Every action also works by mouse.
- **Look**: black on white, monochrome; color only for errors. Mono
  13px, uppercase 11px labels, 1px borders, focused header inverted.
  Side-effect feedback is a transient toast (bottom right).

## Decided

- Heights are honored literally (no niri-style fill).
- Replace cascade-closes the joined chain to the right.
- Draft-protection ("pin") — deliberately ignored for now.
- Workspace-modifier keyboard model over modal in-panel navigation
  (a text editor panel needs the whole plain keyboard).

## Open questions

1. Column tabs (niri) — likely next iteration.
2. Moving a panel into a full column currently overflows it (allowed,
   ugly). Clamp? Scroll within column?
3. Should a joined child align vertically to its parent instead of
   appending to the bottom of an existing column?
4. Real-app Mod key (Super? Cmd?) — alt is just the web-prototype stand-in.

## Files

- `index.html` / `style.css` / `app.js` — the prototype
- `smoke.html` — scripted interaction check (headless)
