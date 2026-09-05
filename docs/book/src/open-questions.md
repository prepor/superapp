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
   modifier on macOS. A future text editor may conflict with familiar commands
   such as Cmd+W.

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
   `app/src/shell/widgets/` cover current panels. Denser tables and a future
   text editor may need new shared widgets. Their interface should describe
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

12. **Background file operations.** *(Files)* Creating, copying, moving, and
    trashing run on the UI thread. Large trees can freeze the window. A
    background runner must use the same disk capability as the panel, report
    progress, support cancellation, and preserve the current undo rules.

13. **Gestures the glass has no word for.** *(Interaction Grammar)* Touch
    covers the tap, the scroll, the workspace pan, the workspaces overlay, the
    panel drag and the row's mark and sweep. Five moves still have no gesture:
    sending a panel to another workspace, moving it between columns, toggling
    tabs for a column, opening a link un-joined, which on glass always joins,
    and offering a panel to an agent as context, since a long press on a header
    is already *pick the panel up*. Possible homes include a long press on a
    link and a menu on the header.

14. **Android.** *(Tech Stack)* The crate is shaped for an Android build and
    the shell's own half of it is written: touch, the grid the screen picks,
    the safe-area insets, the soft keyboard's occlusion and full text state,
    and the inotify watch the file panels refresh on. There is no SDK in this
    tree, so none of it has run on a device.
    What is not written is what needs one: a secrets backend that is not a
    private file, and, for the file browser, access outside the app directory,
    the system opener and the system trash, which mean the Storage Access
    Framework, a `FileProvider`, and `MediaStore`.

16. **The gateway's account.** *(Agents)* It is read off the bucket's host, so
    a device with no R2 bucket has no gateway at all. The store an agent reads
    is the synced one anyway, which is the argument for it; the alternative is
    one more const beside the gateway's name, or a field of its own, and a
    device that wants a model without wanting to sync.

17. **`sql.write` at all.** *(Agents)* A model given a writer uses it. It is
    offered because it was asked for, because the apps' tools are preferred in
    the prompt, and because the changeset comes back on `cmd+z`. What is not
    settled is whether the refusals it stands on — the kernel's tables, a table
    with no primary key, a table the call itself made, any change of shape —
    are the right line, or whether a store this personal wants a narrower door.

18. **Where a call runs.** *(Agents)* The chat panel runs a run's calls on the
    UI thread, because a tool is the verb's own code path and a verb needs the
    session. The cost is that a chat shown nowhere pauses its run at the next
    call. The alternative is a kernel hook — `App::tick(&mut Session)`, run
    every frame the store changed — which is generic and small, and would be
    the first thing the kernel scheduled for an app.

19. **The turn as wire JSON.** *(Agents)* A turn's `body` is the wire's message
    verbatim, so the next request is built from the rows and nothing is lost in
    a mapping. The cost is that a change in the wire is a `Step::Derived` walk
    over every old turn. An app-shaped row per block would cost that mapping
    now instead.

20. **Asking before a call.** *(Agents)* Every tool call runs on arrival and
    undo is the net. A gate — a card that waits, with *allow*, *refuse* and
    *allow all for this run* — is the obvious next step, and the schema has
    room for it: `agent_call.status` can take the words and `Tool::writes` is
    what it would key on. What is not clear is which tools would want one,
    since a gate on all of them is a chat nobody finishes.
