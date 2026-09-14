# Open Questions

Only unresolved design choices belong here. Each is tagged with the chapter it
belongs to.

1. **Joined-panel alignment.** *(Panel Model)* Should a joined child align
   vertically with its parent instead of using the next available space in the
   child column?

2. **Draft protection.** *(Mail)* A joined compose panel closes when its parent
   opens a different child or closes. Should drafts have a pin or another way
   to stay open?

3. **List cursors.** *(The Rich Table)* A cursor belongs to one panel instance.
   Should it survive panel replacement, or move with joined panels?

4. **Workspace modifier.** *(Interaction Grammar)* Cmd is the workspace
   modifier on macOS. The text editor keeps caret chords; should it also be
   able to override workspace commands such as Cmd+W?

5. **Link hints.** *(Interaction Grammar)* The UI does not explain normal open
   versus `cmd+click` when a pointer rests on a link. Makepad has no built-in
   tooltip, so this would need a custom delay and popup.

6. **A bar is one row.** *(Look & Feel)* An entry that will not fit is dropped,
   so a narrow panel silently shows fewer verbs. It could instead mark the
   overflow, shorten labels, or let the chord still fire for a verb that is not
   drawn.

7. **Small-screen content.** *(Panel Model)* A smaller grid changes panel size
   but not panel content. Some panels may need fewer columns or shorter dates.

8. **Launcher ranking.** *(Interaction Grammar)* Results use a fixed order:
   open panels, then roots and each app's sources in app-list order. The app
   does not record enough focus history for learned recency or frequency
   ranking.

9. **Shared widgets.** *(Architecture)* The widgets under
   `app/src/shell/widgets/` cover current panels. Denser tables and richer
   editor controls may need new shared widgets. Their interface should describe
   meaning rather than fixed pixels.

10. **Panels Library gaps.** *(Developer Experience)* Scenes already live with
    their app. What is still missing is live pointer states, editing a scene
    without rebuilding, saving a state reached by hand, and releasing unused
    render textures.

11. **The shell's workspace scene shows an app.** *(Developer Experience)* The
    shell's own `workspace` scene boots on the first root the app list offers,
    so its picture is whatever app happens to lead. It names no app, which is
    the rule, but the scene is not reproducible across builds. A neutral panel
    for the shell's own scenes would fix it and would be one more thing to
    keep alive.

12. **Gestures the glass has no word for.** *(Interaction Grammar)* Touch
    covers the tap, the scroll, the workspace pan, the launcher and Overview,
    moving panels within and between workspaces through Overview and closing
    them there, and the row's mark and sweep. The header menu offers panel
    context to an agent, copies context, toggles a column's tabs, unjoins the
    panel, and closes it. Opening a link un-joined still has no touch
    equivalent of its own: a link on glass always joins, and the header
    menu's unjoin is the way to the same place after the fact. A long press
    on a link is one possible home.

13. **Android platform storage.** *(Tech Stack)* The Android build includes
    touch, keyboard handling, browser sign-in, file watches and device sync.
    Secrets still use private files. How should Android Keystore, access
    outside the app directory, opening local files in other apps and system
    trash fit the existing capabilities?

14. **`sql.write` at all.** *(Agents)* A model given a writer uses it. It is
    offered because it was asked for, because the apps' tools are preferred in
    the prompt, and because the changeset comes back on `cmd+z`. What is not
    settled is whether the refusals it stands on — the kernel's tables, app
    protections, a table with no primary key, a table the call itself made,
    any change of shape — are the right line, or whether a store this personal
    wants a narrower door.

15. **Where a call runs.** *(Agents)* Background attachment reads can finish
    while a chat is closed, but calls needing a session still wait for its
    panel. Should those calls be driven from the existing `App::poll` hook so
    an unseen chat can continue, while calls requiring approval still wait?

16. **The turn as wire JSON.** *(Agents)* A turn's `body` is the wire's message
    verbatim, so the next request is built from the rows and nothing is lost in
    a mapping. The cost is that a change in the wire is a `Step::Derived` walk
    over every old turn. An app-shaped row per block would cost that mapping
    now instead.

17. **Approvals for a run.** *(Agents)* `Tool::asks` already gates individual
    calls with **allow** and **refuse**; `writes` alone does not ask. Should
    there also be an **allow all for this run** policy, and which calls could
    it cover?

18. **Feed enclosures.** *(RSS, Media)* The reader plays media embedded in
    HTML. Should RSS/Atom enclosures and JSON Feed attachments also be appended
    to the reading, with duplicate URLs omitted?

19. **Paused video frames.** *(Media)* Pausing a reader clip releases its native
    player and restores the poster. Should it retain a still frame while
    keeping the number of prepared decoders bounded?

20. **Device backups and note conflicts.** *(Device Sync)* The backup form only
    stores credentials. Per-device snapshots and restore need a design that
    gives a restored store a fresh identity. Notes currently merge one column
    at a time; simultaneous body edits keep one version. A text CRDT would be
    a separate change if that becomes a problem.
