# Interaction Grammar

The same visual signal must have the same meaning everywhere.

## The three interactive signals

| Signal | Meaning |
|---|---|
| Solid underline | Open a panel to the right and join it to this panel |
| Dotted underline | Replace this panel |
| Bordered button | Perform an action without navigating |

Breadcrumbs in the file browser currently use dotted links. Buttons in a
[marks bar](./richtable.md#marks) act on rows but do not open panels.

`cmd+click` and `cmd+enter` open a separate panel without a join. Alt remains
an alias for this behavior. Actions report short results in a bottom-right
toast. Errors use red; other toasts do not.

### Preview: the one open that does not go

A list cursor previews the row it lands on in a joined panel, while focus stays
in the list. Arrow keys can therefore continue through the rows. Clicking a row
also focuses it and opens its preview.

Press `enter` to move focus into the preview. Use `cmd+enter` to open the same
target as a separate panel.

A preview is a real open. It can be undone and may have domain effects, such as
marking mail as read. Consecutive cursor previews combine into one history node,
so one undo closes the whole run. The Effects list previews queued-job details;
the file browser previews a directory or file card.

If the list and preview cannot fit together, as on the cover-screen grid, the
preview takes focus and behaves like a normal open.

### Four mailboxes, one list

Inbox, archive, sent, and spam use one mailbox panel with a folder-role
parameter. They share rows, filtering, marks, cursor movement, and message
previews.

Inbox rows can be archived or deleted. Rows in archive, sent, and spam can only
be deleted. The marks bar, swipe action, and borrowed shortcut all follow this
rule. `@from:` suggestions come from the current mailbox; senders found only in
spam are not offered elsewhere.

Trash is not another mailbox role. Deleting removes a message from these thread
lists; a future Trash panel would need to list individual messages.

### Threads: the row is the conversation, the panel is the whole of it

Each mailbox row represents a conversation with at least one message in that
folder. It shows participants, message count, subject, and the date of the
latest message in that folder. It is bold while any message is unread. The same
conversation can appear in more than one mailbox.

Filing a row files every message from that conversation that belongs to the
current mailbox, as one undoable action. A later reply can place the conversation
back in Inbox.

A message panel shows the full conversation in oldest-first order. The account
appears at the top. Reply and forward actions apply to the newest message.

A reply fills the recipient and subject, quotes the source message, and sends
`In-Reply-To` and `References` headers. A forward starts with an empty recipient,
adds a forwarded-message header block, and keeps the reference chain without
naming a reply parent. Sent mail can then join the same conversation. Forwarded
source mail shows a muted `↪` when the server supports `$Forwarded`.

Each message is one expandable row. A closed row shows the sender, first content
line or error, and date. Click its header to open or close it. Quoted text is
collapsed behind `› quoted`. These open states are temporary panel context and
are not part of undo history.

Opening a conversation marks its initially open unread messages as read. One
undo restores exactly those unread flags. After restart, only the message named
by the panel starts open.

### Attachments: a part of a letter is a card

An open message lists attachments not already shown in the body as `name · size`, with at most five
links followed by a remaining count. Inline images already shown in the body are
not repeated.

An attachment opens in the same card widget used by a disk file. It shows the
name, type, size, source message, and a preview for text, PNG, or JPEG content.
A mail attachment has no disk path, so its only action is `open` (`cmd+o`). This
writes it to an app-owned temporary directory and asks the operating system to
open it.

Attachment bytes remain in the raw message. Derived rows store metadata and a
part number. A worker reads the bytes so opening a panel does not parse a large
message during drawing.

To add a disk file to a draft, open its card and choose `copy` (`cmd+p`). The
compose panel then offers `attach` (`cmd+h`). The draft shows a `CARRIES` line
with links to the selected file cards. Attaching is undoable and adding the same
path twice does nothing.

The draft stores the path, not a copy of the bytes. Sending fails clearly if the
file moved, is larger than 25 MB, or was selected on another device. Directories
cannot be attached.

## Problems

A toast reports an event and fades. A problem represents a condition that still
exists, such as a failed account sync, failed send, or unreachable device-sync
bucket. A small red box in the bottom-right corner shows the current count.

Clicking the count opens or focuses the Problems panel. Each row shows what
failed, the error, supporting details, and available actions. Account failures
offer sync and a Settings link. Failed sends offer retry and reopen. Device-sync
failures clear when the connection recovers.

Problems are derived from account, outbox, and replication state. They are not
stored or dismissed separately. Fixing the source condition removes the row.
The macOS menu shows the same current problems.

### Files: a directory is a column, a file is a card

A `files` panel lists one directory. Directories come first, followed by files,
with case-insensitive name ordering. Columns show name, size, and modification
time. Hidden files appear with `@hidden`. Other filters include `@dir`, `@kind:`,
`@size`, and `@modified`.

Moving the cursor previews a subdirectory as another list or a file as a card.
`enter` moves focus to it. Breadcrumbs such as `~ / Downloads / 2026` replace
the current panel when clicked.

`go to` (`cmd+g`) replaces the breadcrumbs with a path field. Completion lists
entries for the current path segment. `tab` accepts a suggestion, `enter` opens
the typed path, and `esc` restores the breadcrumbs. Paths may start with `~/` or
`/`; relative paths are refused. A second root after the seed restarts the path,
so `~/Downloads//tmp` means `/tmp`.

A file card shows the file name, kind, size, modification time, and selectable
path. It previews up to 64 KiB of text or a PNG/JPEG image detected from its
bytes. `open` (`cmd+o`) gives the path to the operating system; Superapp does not
execute the file.

File operations act on the object represented by a panel, not an arbitrary list
row. A card can open, copy, move, or delete its file. A directory panel at the
end of a joined chain can copy, move, or delete that directory. Root and parent
directory panels offer only `new dir` and `go to`.

`copy` (`cmd+p`) and `move` (`cmd+m`) hold one or more paths. Directory panels
then offer `copy here` or `move here` (`cmd+h`). A move clears the hold; a copy
keeps it. The hold is temporary and is not part of undo history.

Operations check the current disk when they run. Existing destinations,
missing sources, moves to the same place, and copying a directory into itself
are refused per path. Copying a file into its own directory creates names such
as `notes copy.txt` and `notes copy 2.txt`.

`new dir` (`cmd+n`) opens a name field. `delete` (`cmd+d`) always moves items to
the system trash. Copy, move, new directory, and delete are each one undoable
action, including when they act on marked rows. Undo first checks that paths
still name the same objects. If the disk has changed, the history node expires
instead of overwriting or deleting unrelated data.

The browser refreshes visible file panels after its own changes. It does not
watch changes made by other programs.

Directory rows support the same marks as mail: `space` toggles a mark,
`shift+up/down` extends a range, and `esc` clears all marks. A long press starts
marking on touch. While marks exist, copy, move, and delete apply to the marked
set. Each path can succeed or fail independently.

## Keyboard

Cmd shortcuts are split into global workspace commands and focused-panel
commands.

Global commands:

- `cmd+arrows`: move focus; add Shift to move the focused panel;
- `cmd+1…9`: switch workspace; add Shift to move the panel there;
- `cmd+w`: close the focused panel;
- `cmd+z` and `cmd+shift+z`: undo and redo;
- `cmd+[` and `cmd+]`: move a panel into or out of a neighboring column;
- `cmd+,` and `cmd+.`: pull from or push to the right column;
- `cmd+t`: toggle tabs for the column;
- `cmd+u`: open history;
- `cmd+i`: copy panel context;
- `cmd+enter`: open the current target as a separate panel.

All other Cmd letters may be used by the focused panel.

In a list, arrows move the cursor and keep it visible. `enter` enters the
preview. `/` focuses the filter. `space` toggles the current mark unless a text
field owns the keyboard. Shift with up or down extends the marked range.

In completion boxes, arrows choose an item, `enter` or `tab` accepts it, and
`esc` closes it. Without an open completion box, down from a filter enters the
result rows. Compose's recipient field uses the same controls.

In forms, `tab` and `shift+tab` move through fields and buttons. `enter` advances
and submits after the last field. `esc` leaves a text field. There is no Vim key
layer; plain letters remain text input.

## Accelerators

A button or link can show one bold letter. `cmd` plus that letter activates the
control. For example, archive uses `cmd+a`, delete uses `cmd+d`, reply uses
`cmd+r`, and sync uses `cmd+s`.

Accelerators follow these rules:

1. They cannot use a global shortcut.
2. They are unique within a panel.
3. A panel with editable or selectable text leaves `c`, `v`, `x`, and `a` to
   normal text operations.
4. Only a control that appears once in a panel gets an accelerator.
5. A list may borrow available accelerators from its visible preview.

These rules are checked by unit tests.

### Borrowed keys

A list and its preview act as one working area. With a message preview open,
the list can borrow archive, delete, reply, and forward shortcuts. The list's
own shortcut wins if there is a conflict.

Borrowing stops while a text field has focus, so `cmd+a` still means Select All
in a filter. It also stops while marks exist. The marks bar then owns the batch
action shortcuts. Mailboxes that do not offer archive do not borrow archive.

## Workspaces

Nine numbered workspaces form a vertical stack. On macOS, the menu bar lists
occupied workspaces and the first empty one, with actions to switch or move the
focused panel.

On touch, a two-finger swipe down opens the workspace overlay. It shows a search
row and one row per workspace. Tap a workspace to switch. Tap outside, swipe up,
or press `esc` to close it. Moving a panel between workspaces has no touch
gesture yet.

## The launcher

Double-tap Cmd to open the launcher. It searches open panels, root panels,
contacts, and mail. Every word in the query must match.

An open result focuses its existing panel and switches workspace if needed. A
new result opens as a separate last column in the current workspace. The
launcher does not create a duplicate of a panel that is already open.

With an empty query, open panels appear first, followed by roots. Arrow keys
wrap through results. `enter` opens the selected result; `esc`, another
double-Cmd, or a click outside closes the launcher. It is also available in the
macOS menu and from the search row in the touch workspace overlay.

## Undo

User actions such as open, close, replace, panel movement, filing mail, and file
operations create history nodes. Undo restores both layout and data changed by
the action. A batch operation creates one node and restores its marks on undo.

Focus movement, workspace switching, camera movement, row cursors, and marks do
not create history nodes. Rapid repeated layout or preview changes can combine
into one node.

History is kept in memory and is lost when the process ends. The database work
remains durable, so pending sends and sync continue after restart even though
they can no longer be undone.

History is a tree. Performing an action after undo creates another branch
without deleting the old one. `cmd+u` opens an overlay that can travel to any
node by undoing to the shared parent and replaying the selected branch.

Some changes cannot be reversed after the outside world changes. A delivered
send, emptied trash, or reused file path makes its node expire. History skips
expired nodes instead of pretending the action was reversed. Removing an
account is expired immediately because restoring the panel cannot restore its
mail. The tree keeps at most 200 actions.

## Mouse and trackpad

Clicking focuses panels and activates controls. Hover states and cursor shapes
show hit areas. Horizontal trackpad movement pans the workspace directly;
vertical movement scrolls the panel under the pointer. Scrollable content shows
a small grey thumb.

## Touch (Android)

- **Tap** behaves like click. There is no touch version of `cmd+click`.
- **One-finger vertical drag** scrolls the panel under the finger.
- **Sideways drag on a mail row** archives left or deletes right. A labelled
  curtain shows the pending action. Release after one third of the row to run
  it; release earlier to cancel.
- **Two-finger horizontal drag** pans the workspace. On release it aligns to the
  nearest column edge.
- **Two-finger vertical drag** opens the workspace overlay when moving down and
  closes it when moving up.
- **Long-press a list row** starts marking. While marks exist, taps toggle them.
- **Long-press a panel header** starts a panel drag. A horizontal insertion bar
  means stack in that column; a vertical bar means create a column. Holding near
  a screen edge scrolls the workspace.

Movement starts after an 8 pt threshold. Once a gesture chooses a mode, it does
not switch modes until all fingers lift.

### The soft keyboard

The on-screen keyboard belongs to the focused text field. The workspace uses
the remaining height, so the field stays above the keyboard. Android's full
text state supplies autocorrect, composition, and swipe typing. Dismissing the
keyboard leaves the field, like `esc` on desktop. Tapping a field shows it
again. The keyboard action button acts as Enter for single-line fields.
