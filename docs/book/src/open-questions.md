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

7. **Two undos after filing from a reader.** *(Mail)* Filing closes the reader
   on one node, and the driving list then previews the next row, which is a
   node of its own. One `cmd+z` therefore un-previews the successor and a
   second un-files. Whether the successor's preview should join the filing
   action is not settled.

8. **Small-screen content.** *(Panel Model)* A smaller grid changes panel size
   but not panel content. Some panels may need fewer columns or shorter dates.

9. **Launcher ranking.** *(Interaction Grammar)* Results use a fixed order:
   open panels, then roots and each app's sources in app-list order. The app
   does not record enough focus history for learned recency or frequency
   ranking.

10. **Shared widgets.** *(Architecture)* The widgets under
    `app/src/shell/widgets/` cover current panels. Denser tables and a future
    text editor may need new shared widgets. Their interface should describe
    meaning rather than fixed pixels.

11. **Panels Library gaps.** *(Developer Experience)* Scenes already live with
    their app. What is still missing is live pointer states, editing a scene
    without rebuilding, saving a state reached by hand, and releasing unused
    render textures.

12. **The shell's workspace scene shows an app.** *(Developer Experience)* The
    shell's own `workspace` scene boots on the first root the app list offers,
    so its picture is whatever app happens to lead. It names no app, which is
    the rule, but the scene is not reproducible across builds. A neutral panel
    for the shell's own scenes would fix it and would be one more thing to
    keep alive.

13. **Background file operations.** *(Files)* Creating, copying, moving, and
    trashing run on the UI thread. Large trees can freeze the window. A
    background runner must use the same disk capability as the panel, report
    progress, support cancellation, and preserve the current undo rules.

14. **Watching the disk.** *(Files)* File panels refresh after Superapp changes
    the disk or enters another directory, but not after another program makes a
    change. macOS could use FSEvents, with events grouped to avoid repeated
    refreshes.

15. **Touch.** *(Interaction Grammar)* Touch is not in this build. The
    interfaces leave room for it (the table widget's rows, the panel drag, the
    two-finger workspace pan, and the workspaces overlay are all there) and
    the script grammar still parses the touch steps. A port has to answer what
    it did before: there is no touch gesture for moving a panel between columns
    or workspaces, for toggling tabs, or for opening a separate un-joined
    panel. Possible homes include a link long-press or a header menu.

16. **Android.** *(Tech Stack)* The crate is shaped for an Android build:
    a library with a JNI entry point, its own launcher icons, and a grid the
    layout can switch. The platform work is not ported: touch input, fold
    and unfold as grid changes, safe-area insets, the on-screen keyboard and
    its full text state, and a secrets backend that is not a private file.
    Files on Android needs more still: access outside the app directory, the
    system opener, and the system trash, which mean the Storage Access
    Framework, a `FileProvider`, and `MediaStore`.
