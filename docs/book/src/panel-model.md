# Panel Model

## Workspaces

There are nine numbered workspaces. Each keeps its own columns, focus, and
camera position. Empty workspaces are always available and need no setup.

Workspaces form a vertical stack. Switching workspace animates up or down by
one screen. Moving a panel to another workspace puts it in a new last column
and follows it there. Joins to panels left behind are removed. Focus in the old
workspace moves to a nearby panel.

## Grid and columns

The screen is divided into grid units with 8 pt gaps:

- desktop: 12×6;
- unfolded phone: 8×4;
- cover screen: 4×3.

The shell switches phone grids when the width crosses about 600 dp. Animation
moves panels to the new layout.

Each panel kind requests a width and height. A request is limited to the active
grid, so an inbox fills the 4×3 cover screen without a separate phone layout.
If a column's requested heights fit, unused space stays empty. If they do not
fit, the panels share the column height evenly. A column is as wide as its
widest panel. Columns continue to the right and the workspace scrolls when they
do not fit on screen.

Some panels request more height when their content needs it. A long message can
grow up to the full column, while a short message stays at its default height.
A conversation counts the full height of open messages and one line for each
closed message. The shell recalculates this after content arrives. The size is
not saved.

## Column actions and tabs

- `cmd+[` and `cmd+]` move a lone panel into the neighboring column, or move a
  stacked panel into a new column on that side.
- `cmd+,` moves the first panel from the right column to the bottom of the
  focused column.
- `cmd+.` moves the bottom panel of the focused column into a new column on the
  right.
- `cmd+t` switches a column between stacked and tabbed display. A tabbed column
  shows one panel at full height. Click a tab or move focus up and down to
  choose the visible panel.

The camera moves just enough to keep the focused panel visible with one gap of
margin. Trackpad movement follows the input directly. Touch movement is free
while fingers are down, then aligns to the nearest column after release.

A [preview](./interaction-grammar.md#preview-the-one-open-that-does-not-go) also
asks to be visible once. Focus wins if both the focused panel and preview
cannot fit.

## Placement

A new panel opens to the right of its parent. It uses the next column when its
requested height fits there; otherwise it gets a new column. A joined child is
always directly to the right of its parent. An unrelated panel never splits an
existing joined pair.

## Joins

- A normal link opens its target joined to the current panel.
- The next normal link from the same parent replaces that joined child.
- Replacing or closing a panel also closes all joined descendants to its
  right. Undo restores the whole closed group.
- A join only exists while its child is in the next column. Moving either panel
  away removes the join. A ═ bridge shows each live join.
- `cmd+click` or `cmd+enter` opens a separate panel with no join.
- A preview is a joined open that leaves focus in the parent. In a tabbed
  column, the preview becomes the visible tab. If the parent and preview cannot
  share the screen, the preview takes focus instead.

## Focus, movement, and closing

Exactly one panel has focus, shown by its inverted header. With the workspace
modifier, up and down move through a column; left and right choose the closest
panel by vertical position in the neighboring column.

Moving a panel can reorder its column, join another column, swap a whole column,
or create a new edge column. Closing removes the panel and its joined
descendants, moves focus to the nearest remaining panel, and removes empty
columns. Moving a panel to another workspace is not a close, but it still
breaks joins with panels that stay behind.

These rules are implemented and unit-tested in `src/core.rs`.
