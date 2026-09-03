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

12. **A copy waits for its own bytes.** The browser writes now — `new
    dir`, `copy here`, `move here` and `delete` are effects through the
    outside, each an undoable action, `delete` the trash and never `rm`
    — but all four are performed **inline**, on the frame of the click.
    A file or a small tree is the same wait as the listing that follows
    it; a large tree is a stall. The shape that was drafted for it is a
    deferred job with its status on the panel, and what is in the way is
    real: the effect executor runs on the sender and sync threads, each
    of which builds its own `Real` outside — and under `--demo-disk`
    that is a *different disk* from the one the panel is looking at, so
    a filed copy would run somewhere else. A runner that shares the
    panel's outside comes first; progress on the panel comes with it,
    and with progress, a cancel. Until then the honest reading is that a
    copy is as fast as the disk and the window is still.

13. **The effect log is a queue, not a log.** The `effects` panel is the
    `effect` table read back, and only a *deferred* effect writes a row
    there — so `clip`, `open` and the file browser's four writing verbs
    leave no trace at all. For the clipboard that is proportionate; for a
    delete it is not, and the panel's own promise — everything the app has
    tried on the outside world — was quietly the queue's promise instead.
    Two ways to close it, and the choice is the question: file the file
    verbs as deferred jobs, which is question 12 with all of its problems,
    or let an inline effect write its row **after** it is performed,
    `done` or `failed`, never `pending`. The second is the smaller change
    and the truer one: the queue keeps its meaning, the log becomes the
    log, and undo never races anything. What it needs is a serializable
    payload per logged effect — the panel's one-line description is
    decoded through the registry — and a judgement about which inline
    effects opt in: the file verbs, `clip` and `open` yes; `now`,
    `connect`, `fetch` and `secret_get` would bury the panel.

14. **Nothing watches the disk.** A files panel lists when it lands on a
    directory and again when one of our own verbs writes — that much a
    verb can say for itself — so a file *another* program wrote appears
    when you walk back to it, not when it lands. FSEvents on macOS and
    inotify on android, one watch per directory a panel shows, coalesced
    into one invalidation per burst, is the shape — until then the
    listing is a snapshot with no generation to bump, which is also why
    the panel has no `sync` button: a button for it would be an
    admission rather than a feature. It is the same absence that makes a
    `… here` plan against the disk at the click rather than at the hold,
    and an undo ask the disk before it reverses.

15. **Saving an attachment to the disk** is the one direction
    [attachments](./interaction-grammar.md#attachments-a-part-of-a-letter-is-a-card)
    do not go. `open` writes a part into the app's scratch directory and
    hands it to the OS, which is enough to read one; putting it somewhere
    you chose means `copy` on a part *holding* it, and a files panel's
    `copy here` — which now performs — would carry it the rest of the way.
    What is missing is the hold: it is a set of **paths**, and a part is
    not one until it has been written out. The same gap as question 16,
    from the other side.

16. **A letter cannot carry a letter's part.** Compose attaches files off
    the disk; forwarding a mail that carried something drops what it
    carried, and `attach` cannot be pointed at an attachment card. The
    hold is a set of *paths*, so the fix is a hold that can name a part
    too — at which point a forward can carry the source's parts by
    default, which is what every other client does.

17. **Files beyond the app's own directory on android**, and `open` and
    the trash there: scoped storage puts the rest of the disk behind SAF,
    the OS opener behind a FileProvider and a delete behind
    `MediaStore`'s own trash, all JNI the shell does not have — a
    `delete` on android refuses rather than falling back to a `rm`. The
    browser is macOS-shaped today.

