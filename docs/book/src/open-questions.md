# Open Questions

Design decisions that are genuinely open. That they are open is part of the
current state; everything else in this book is settled.

1. **Should a joined child align vertically to its parent** instead of
   appending to the bottom of an existing column?
2. **Draft protection**: a joined compose panel is silently closed by the next
   solid link in its parent, and now by closing that parent too
   (cascade-close). Deliberately ignored for now; a "pin" concept may
   return.
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

11. **The panels library's catalogue is Rust**, one function per subject,
    and every panel or workspace node boots its own world and replays its
    steps from the seed. Open there: whether scenes want to live beside
    their widgets once the catalogue outgrows one file; pointer states
    (hover, press) are fixtures today, not events the mount receives; a
    DSL change is still a rebuild and a reopen; a state reached by hand
    inside a node cannot be saved back as a node; and a node's pass
    texture stays allocated at the last size it was seen at.

12. **The file browser reads; it does not write yet.** `new dir`, `copy`,
    `move` and `delete` are drawn with their chords and their whole
    grammar — the hold and its `… here`, the refusals, the end-of-chain
    rule — and each of them toasts what it *would* have done rather than
    touching the disk. What is settled and waiting: the verbs are effects
    (the store cannot reproduce them), the rename-class ones performing
    inline like `clip` while a copy is a deferred job with its status on
    the panel; `delete` is the trash, never `rm`, so undo can restore;
    every one is an undoable action whose reversal expires honestly when
    the disk has moved on (a new directory that is no longer empty, a
    trash that was emptied); a clash refuses on the status line, with the
    one exception of a copy into the file's own directory.

13. **Nothing watches the disk.** A files panel lists when it lands on a
    directory, so a file another program wrote appears when you walk back
    to it, not when it lands. FSEvents on macOS and inotify on android,
    one watch per directory a panel shows, coalesced into one invalidation
    per burst, is the shape — until then the listing is a snapshot with no
    generation to bump, which is also why the panel has no `sync` button:
    a button for it would be an admission rather than a feature.

14. **Files beyond the app's own directory on android**, and `open` there:
    scoped storage puts the rest of the disk behind SAF and the OS opener
    behind a FileProvider, both JNI the shell does not have. The browser
    is macOS-shaped today.

15. **Mail attachments** are the reason the browser was built first and
    are not built: `parse_mail` still drops every part that is not the
    text or the HTML. The shape that fits: an `attachment` row per part
    with the bytes staying in the `raw` the store already keeps, a card
    over that instead of a path, and compose gaining `attach` bound to a
    files panel by the join, the way `copy here` is bound to a directory.

16. **A files selection spans nothing**: the unit is the row under the
    cursor, because a multi-row selection would multiply every verb, and
    the same question stands for the inbox (open question 3). Nothing has
    hurt yet.
