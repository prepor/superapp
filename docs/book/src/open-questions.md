# Open Questions

Only unresolved design choices belong here.

1. **Joined-panel alignment.** Should a joined child align vertically with its
   parent instead of using the next available space in the child column?

2. **Draft protection.** A joined compose panel closes when its parent opens a
   different child or closes. Should drafts have a pin or another way to stay
   open?

3. **List cursors.** A mailbox cursor belongs to one panel. Should it survive
   panel replacement or move with joined panels?

4. **Workspace modifier.** Cmd is the workspace modifier on macOS. A future text
   editor may conflict with familiar commands such as Cmd+W.

5. **Link hints.** The UI does not explain normal open versus `cmd+click` when
   a pointer rests on a link. Makepad has no built-in tooltip, so this would
   need a custom delay and popup.

6. **Touch controls.** Touch has no controls for moving panels between columns,
   toggling tabs, or opening a separate unjoined panel. Possible homes include a
   link long-press or a header menu.

7. **Small-screen content.** The 4×3 cover grid changes panel size but not panel
   content. Some panels may need fewer columns or shorter dates.

8. **Moving panels between workspaces on touch.** The workspace overlay only
   switches workspaces. A held panel could perhaps be dropped on an overlay row.

9. **Launcher ranking.** Results use a fixed order: open panels, roots,
   contacts, then newest mail. The app does not record enough focus history for
   learned recency or frequency ranking.

10. **Shared widgets.** The widgets in `src/panels.rs` cover current
    panels. Denser tables and a future text editor may need new shared widgets.
    Their interface should describe meaning rather than fixed pixels.

11. **Panels Library growth.** The catalogue is one Rust module. It may need to
    move scenes next to their widgets as it grows. Other gaps are live pointer
    states, editing scenes without rebuilding, saving a state reached by hand,
    and releasing unused render textures.

12. **Background file operations.** Creating, copying, moving, and trashing run
    on the UI thread. Large trees can freeze the window. A background runner
    must use the same disk implementation as the panel, report progress, support
    cancellation, and preserve the current undo rules.

13. **Watching the disk.** File panels refresh after Superapp changes the disk
    or enters another directory, but not after another program makes a change.
    macOS could use FSEvents and Android could use inotify, with events grouped
    to avoid repeated refreshes.

14. **Saving mail attachments.** Opening an attachment writes it to a temporary
    directory. Saving it through the file browser needs a held item that can
    refer to a mail part as well as a disk path.

15. **Forwarding mail attachments.** Compose only attaches disk files. A hold
    that can name a mail part could also let forwards include the source
    attachments by default.

16. **Android file access.** The Android browser does not yet support files
    outside the app directory, the system opener, or system trash. These need
    the Storage Access Framework, a `FileProvider`, and `MediaStore` integration.
