# Open Questions

Design decisions that are genuinely open. That they are open is part of the
current state; everything else in this book is settled.

1. **Column tabs** (niri's tabbed columns) — agreed direction, not designed.
2. **Moving a panel into a full column** currently overflows the grid
   (allowed, ugly). Clamp? Scroll within the column?
3. **Should a joined child align vertically to its parent** instead of
   appending to the bottom of an existing column?
4. **Draft protection**: a joined compose panel is silently closed by the next
   solid link in its parent (cascade-close). Deliberately ignored for now; a
   "pin" concept may return.
5. **In-panel selection vs panel focus**: the inbox row cursor is per-panel
   state today; whether selection should survive replacement or travel with
   joins is unexplored.
6. **The workspace modifier** is Cmd on macOS. Whether that survives contact
   with a real text editor panel (Cmd+W closes a panel, not a tab) is open.
