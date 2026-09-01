# Open Questions

Design decisions that are genuinely open. That they are open is part of the
current state; everything else in this book is settled.

1. **Should a joined child align vertically to its parent** instead of
   appending to the bottom of an existing column?
2. **Draft protection**: a joined compose panel is silently closed by the next
   solid link in its parent (cascade-close). Deliberately ignored for now; a
   "pin" concept may return.
3. **In-panel selection vs panel focus**: the inbox row cursor is per-panel
   state today; whether selection should survive replacement or travel with
   joins is unexplored.
4. **The workspace modifier** is Cmd on macOS. Whether that survives contact
   with a real text editor panel (Cmd+W closes a panel, not a tab) is open.
5. **Link hover hints**: the web prototype's tooltips ("opens a joined panel …
   cmd+click: separate") have no native equivalent yet; makepad has no
   tooltip, so it would be our own hover-delay affordance.
6. **Touch gaps**: column ops (consume/expel, tabs toggle) and the fresh
   un-joined open have no touch affordance yet — candidates: long-press on a
   *link* (currently unassigned), a header overflow menu, or drag gestures on
   the tab strip. Un-answered until the phone build gets real use.
7. **Small-grid content density**: panels reuse the desktop content on the
   4×3 cover grid (only the grid clamps). Whether kinds should *adapt* their
   content to tight columns (fewer inbox columns, shorter dates) is open.
8. **Moving a panel between workspaces on touch** has no gesture yet — the
   overlay only switches. Candidates: drag a held panel onto the overlay
   (raise it during a drag near the top edge?), or rows as drop targets.
   Un-answered until the phone build gets real use.

9. **Launcher ranking is positional, not learned**: open panels, then roots,
   contacts, mails, newest first — no recency or frequency boost, because
   nothing records focus history yet. Whether the launcher (and alt-tab-like
   switching generally) wants an MRU model is open until real use hurts.

10. **The semantic component library** (`src/ui.rs`) is a seed: sections,
    field rows, action rows, the tab/enter walk. Its growth path — richer
    field behaviour (click-to-caret, selection), denser tables, a real
    layout pass instead of the char grid's fixed columns — is the same arc
    as the future text editor panel. Grown by need, stelaxis-style: name
    the meaning, never the pixels.

Settled since the last revision: workspaces — nine numbered, vertical slide,
menu bar on macOS, two-finger-swipe-down overlay on touch (see [Panel
Model](./panel-model.md) and [Interaction
Grammar](./interaction-grammar.md)); makepad is now plain upstream at
mosaic's pin (see [Tech Stack](./tech-stack.md) for the one e2e trade-off);
the launcher — double-cmd, one query over open panels + the mail world,
go-to vs open decided per hit, no fresh-copy variant, the search row as its
touch entry (see [Interaction Grammar](./interaction-grammar.md)).
