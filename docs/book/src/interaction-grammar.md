# Interaction Grammar

The same visual signal must have the same meaning everywhere.

## The three interactive signals

| Signal | Meaning |
|---|---|
| Solid underline | Open a panel to the right and join it to this panel |
| Dotted underline | Replace this panel |
| Bordered button | Perform an action without navigating |

A panel's [bar](#the-bar) carries both: its buttons act on what the panel
shows, its links go somewhere from it. Breadcrumbs in the file browser use
dotted links. Batch verbs act on marked rows and open nothing.

`cmd+click` and `cmd+enter` open a separate panel without a join, on a link,
on a row, or on an entry of a bar. Alt remains an alias for this behavior.
Actions report short results in a bottom-right toast. Errors use red; other
toasts do not.

### Preview: the one open that does not go

A list cursor previews the row it lands on in a joined panel, while focus stays
in the list. Arrow keys can therefore continue through the rows. Clicking a row
also moves the cursor there and opens its preview.

Press `enter` to open the row and take focus with it, which is the solid-link
rule. Use `cmd+enter` to open the same target as a separate panel.

A preview is a real open. It can be undone and may claim something of the
world, such as marking mail as read. Consecutive cursor previews combine into
one history node, so one undo closes the whole run. The effect log previews a
job's details; the file browser previews a directory as a list or a file as a
card; a mailbox previews a conversation.

If the list and preview cannot fit together, the preview takes focus and
behaves like a normal open.

A list keeps its cursor and its child in step by reading, on every draw, what
its joined child shows. No panel kind declares what it previews into.

## The bar

Every panel wears a bar at its foot, built on every draw from what the panel
answers with. Left to right: the buttons that act on what the panel shows, the
links that go somewhere from it, and, while the panel's table has marks, the
batch verbs over the marked set with their count in the label.

The header wears nothing but the title and the close button.

A bar is one row and never wraps. An entry that would run past the right edge
is dropped, so a narrow panel shows fewer verbs rather than a taller bar.

Every entry is addressed by its label, whether by a click, by a chord, or by an
end-to-end script. What runs is the verb's stable id, so a bar is a view of the
panel and never a copy of it.

## Accelerators and the bar

A button or link can show one bold letter. `cmd` plus that letter activates the
control. A verb whose letter is not bold still fires on click.

A chord is offered in one order and stops at the first taker:

1. the workspace's **reserved** chords;
2. the **focused widget**, which may take one (a live text field takes
   `cmd+a`) and says so in the same event;
3. the **focused panel's bar**;
4. the bar of the panel it **previews**, if it drives one.

The last step is what lets a list act on the thing under its cursor without
moving focus. Nothing in that order names a panel kind.

A bold letter is a promise that the chord fires that verb *now*, so the bars
draw exactly what this order would reach: the focused panel's bar shows its
letters except those the focused widget is keeping while one of its fields has
the keyboard; the bar of the panel it previews shows only the letters the
focused bar leaves free; every other bar shows no letter at all. There is no
lending rule and nothing to keep in step, because the drawing and the routing
read the same order.

Two more rules hold, and a debug build asserts both on every draw:

1. A verb never wears one of the workspace's reserved letters: `w`, `z`, `u`,
   `t`, `i`, and `l`.
2. Two verbs on one bar never wear the same letter.

Each app tests its own bars besides. A panel with a control per row, such as
the problems list, gives its rows no letters at all: it would have to invent
one per row.

## Keyboard

Cmd is the workspace modifier. The reserved chords are:

- `cmd+arrows`: move focus; add Shift to move the focused panel;
- `cmd+1…9`: switch workspace; add Shift to send the panel there and follow;
- `cmd+w`: close the focused panel and its joined chain;
- `cmd+z` and `cmd+shift+z`: undo and redo;
- `cmd+[` and `cmd+]`: move a panel into or out of a neighboring column;
- `cmd+,` and `cmd+.`: pull from or push to the right column;
- `cmd+t`: toggle tabs for the column;
- `cmd+u`: open the history overlay;
- `cmd+i`: copy the focused panel's context;
- `cmd+enter`: reserved so that no bar may claim it, and handed to the focused
  panel, which reads it as *open un-joined*;
- `shift+cmd+l`: raise or lower the panels library over the workspace;
- `shift+cmd+s`: go to the search panel, wherever it stands;
- a double tap of `cmd`: the launcher.

All other Cmd letters may be used by the focused panel's bar. Only the shifted
`s` is reserved: plain `cmd+s` still belongs to whatever bar wears it, and a
bar is only ever reached without Shift.

In a list, arrows move the cursor and keep it visible, `enter` opens the row
and goes to it, `/` focuses the filter, `space` toggles the current mark unless
a text field owns the keyboard, `shift` with up or down extends the marked
range, `esc` clears the marks, and `tab` returns to the filter.

In completion boxes, arrows choose an item, `enter` or `tab` accepts it, and
`esc` closes it. Without an open completion box, down from a filter enters the
result rows.

In forms, `tab` and `shift+tab` move through fields and buttons. `enter`
advances and submits after the last field. `esc` leaves a text field. There is
no Vim key layer; plain letters remain text input.

## Workspaces

Nine numbered workspaces form a vertical stack. On macOS, the menu bar lists
occupied workspaces and the first empty one, with actions to switch or move the
focused panel. The menu items carry no key equivalents; their labels spell the
chords, and the chords are the keyboard's.

## The launcher

Double-tap Cmd to open the launcher. It is the switcher: it runs over the
panels that are open and every app's roots, and over nothing else. Every word
in the query must match, by prefix, some word of a panel's title, its tag, or a
root's extra words.

An open result focuses its existing panel and switches workspace if needed. A
new result opens as a separate last column in the current workspace. The
launcher does not create a duplicate of a panel that is already open.

With an empty query, open panels appear first, followed by roots in app-list
order. Arrow keys wrap through results. `enter` opens the selected result;
`esc`, another double-Cmd, or a click outside closes the launcher. It is also
available in the macOS menu.

## Search

`shift+cmd+s` goes to the search panel — focused wherever it already stands,
opened beside the focused panel where it does not. It is a panel and not an
overlay, so an answer stays on screen while it is read, is walked and marked
like any other list, and can be sent to a workspace of its own.

The line above its rows is read twice over. Its **words** are the question, put
to every app's own search source at once; each source answers on its own
thread, and its rows land as they arrive, under the rows already on screen.
Its **`@` tags** narrow what came back: `@app:mail` keeps one source's rows.
The words are never a second sieve over the answer — a letter found by a word
deep in its body has none of that word in the line the row draws.

The rows are a rich table: arrows walk and preview, `enter` opens and goes,
`space` marks, and *open n* opens every marked row at once, as one undoable
step. An empty list says which kind of empty it is: nothing asked, nobody
answered yet, or an answer of nothing.

## Undo

User actions such as open, close, replace, panel movement, filing mail, and
file operations create history nodes. Undo restores both the layout and the
data an action changed, because both halves are one node. A batch operation
creates one node and restores its marks on undo.

Focus movement, workspace switching, camera movement, row cursors, and marks do
not create history nodes. Rapid repeated layout or preview changes coalesce
into one node, per originating slot.

History is kept in memory and is lost when the process ends. The database work
remains durable, so pending sends and sync continue after restart even though
they can no longer be undone.

History is a tree. Performing an action after undo creates another branch
without deleting the old one. `cmd+u` opens an overlay that can travel to any
node by undoing to the shared parent and replaying the selected branch.

Some changes cannot be reversed after the outside world changes. A delivered
send, emptied trash, or reused file path makes its node expire. History skips
expired nodes instead of pretending the action was reversed. Removing an
account expires immediately, because restoring the panel cannot restore its
mail. The tree keeps at most 200 actions.

## Problems

A toast reports an event and fades. A problem is a condition that still exists,
derived from the rows that carry it and never stored. A small red box in the
bottom-right corner shows the current count, and the macOS menu shows the same
problems as items.

Clicking the count opens or focuses the Problems panel. Each row shows what it
concerns, what is wrong, a muted line of supporting detail, and the controls
that source gave it. The controls arrive as data, so a source this build has
never heard of still draws. A problem is announced once as a toast when it
first appears, and it clears when the source stops listing it: fixing the
condition removes the row.

The apps supply their own sources; device sync is the kernel's own, and it is
listed first.

## Mouse and trackpad

Clicking focuses panels and activates controls. Hover states and cursor shapes
show hit areas. Horizontal trackpad movement pans the workspace directly;
vertical movement scrolls the panel under the pointer. Scrollable content shows
a small grey thumb.

## Touch

A finger lands on the same rectangles a click resolves against, so a gesture
and a click never disagree about what is under them. One state machine
arbitrates, and it locks at the first move past eight points and holds until
every finger lifts, so nothing changes its mind mid-gesture.

| Gesture | Meaning |
|---|---|
| Tap | A click where it went down |
| One finger, vertically | The panel under it scrolls, 1:1 |
| One finger, sideways on a row | The curtain, and a verb past a third of it |
| Two fingers, horizontally | The workspace pans, and aligns on release |
| Two fingers, down | The workspaces overlay |
| Two fingers, up | Whichever overlay is up goes away |
| Long press on a header | The panel is picked up |
| Long press on a row | Its mark, toggled |

A tap is a press and a release at one point, so what it means is what a click
means. There is no touch equivalent of `cmd+click`: a link on glass always
follows the join rule.

Two fingers moving sideways pan the strip 1:1 and magnetise to the nearest
column edge when they lift. Two fingers moving down raise the workspaces
overlay, and its *search panels* row raises the launcher; two moving up put
whichever overlay is up away.

A long press on a panel's header picks the panel up. It then rides the finger,
an ink insertion bar previews where a drop would land: vertical in a gap for
a fresh column, horizontal across a column to stack at that row. A finger held
near an edge pans the strip, so the far columns are reachable. The drop is
judged by the finger, not by the panel.

A long press on a row toggles its mark, which is the phone's way to a set:
space and shift belong to a keyboard. A sideways drag on a row draws a curtain
in from the edge the finger travels away from, carrying the word of the verb a
lift would run. Below a third of the row it is the selection grey with the word
in ink and a hairline at its leading edge; past it the whole thing inverts, the
way a control under the pointer does. On release the curtain finishes covering
the row and only then does the verb run, so the row is gone from view before it
is gone from the list.

The verb is the panel's own, run over the swept row alone: the row is marked,
the batch verb fires, and whatever was marked before goes back on. A list that
offers no verb that way draws no curtain, and the lift does nothing. In a
mailbox, a leftward sweep archives and a rightward one deletes.

## The soft keyboard

The workspace lives inside the safe area, so a cutout or a rounded corner takes
nothing from a panel. Android additionally swallows touches in the
notification-shade strip at the top of the window, and the workspace stays
clear of that too.

The soft keyboard shortens the workspace by as much as it occludes, and the
panels spring up to fit the smaller board: the app makes its own room rather
than letting the system slide the whole window. The keyboard's own action
button is this grammar's enter: a form advances, a filter runs, a list opens
its row.

A field owns the whole input protocol, its authoritative full text state
included, so the shell hands one over whole rather than reading characters out
of it. That is what keeps a composition from being typed twice.
