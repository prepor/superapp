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

10. **The semantic component library** lives in `src/panels.rs` as retained
    widgets (`SLabel`, `SField`, `SBtn`, `SLink`, `SKbd`, `SText`, `SBold`,
    and the panel and overlay trees composed from them), so click-to-caret,
    selection, the IME and a real layout pass come from makepad rather than
    from us. What is open is where it grows next: denser tables, and
    whatever the text editor panel turns out to need. Grown by need,
    stelaxis-style: name the meaning, never the pixels. `src/ui.rs` keeps
    the rest — the accelerator rules and the chrome's `Style`/`BtnAct`.

11. **The panels library replays every node from the seed**, one step per
    frame, one node at a time. Starting node *k* from node *k−1*'s world
    (an SQLite backup is milliseconds; the retained widgets' scroll and
    caret state is what would have to be rebuilt) would cut a story's cost
    from a triangle to a line, and is un-answered until the shelf is opened
    often enough to hurt. Also open there: a DSL change is still a rebuild
    and a reopen, a flow continued by hand inside a node cannot yet be
    saved back as a `shot`, and a node's pass texture stays allocated at
    the last size it was seen at.
