# CR-019 · Walking the chats, picking a file, and coming back where you were

Status: **implemented** (Andrey, 2026-09-14: "workspace: arrows should navigate
chats · add repository: use filebrowser in 'picker' mode, the same for attach
files in mail and telegram · we should save position in chats"; then "review by
Fable then implement"). Three small changes to Workshop, one of which the mail
and Telegram composers share. All four phases have landed and the book says
what they do (`workshop.md`, `files.md`, `mail.md`, `telegram.md`,
`panel-model.md`, `apps.md`). Two things went differently from what is
proposed below, both out of the review, and the book has them: pick mode is
carried by everything the picker opens rather than stopping at the first
listing, and the position is saved by a timer, by `Panel::flush` and by the
panel's `Drop` together rather than by `flush` alone — `flush` is called at
shutdown and before an undo walk, and a closed panel is simply dropped, so
neither covers the other. None of the three waits for the writer, and the
value a chat opens on is the one this process has in hand rather than the
row, which may still be one write behind.

## Why

Three places where the app asks a hand to do something the shell already does
better everywhere else.

**The workspace hub's chat list has no cursor.** It draws one row per chat
through the same `fill_row` every table row uses — and passes it
`(false, false)`, so no row is ever the selected one
(`workshop/widgets.rs`). Arrows do nothing: the panel's only `KeyDown` is the
composer's Return. A chat is reached with the mouse, and a press is an open
with focus rather than the preview every other list gives. Four chats in a
workspace and the one list in the app you cannot walk.

**A clipboard is the only way to hand an app a file.** To attach a letter you
open Files, find the file, press `copy`, come back to the draft, and press
`attach` — the verb only exists while the clipboard holds something.
Telegram's attach panel is the same, with `browse` opening a plain `files(~)`
beside it and `add` taking whatever the clipboard ended up with. That is a
good way in when the file is already under your hand; it is a round trip when
it is not, and it cannot say *what for*. Workshop's *add repository* does not
even have that: it is a bare text field labelled *repository path*, and a
repository is a path you type.

**A chat always opens at its end.** `WorkshopDetail::compose` clears the
widget's state whenever the panel it draws changes, and the transcript's
`follow` is true on the first draw, so every chat opens at the tail. Scroll up
to read what an agent did an hour ago, walk to another chat and back, and the
reading is gone. Telegram already keeps a reading position — its unread divider
and its anchor — and a Workshop chat is a transcript people read the same way.

## The model

> **A list is walked, a file is picked in the file browser, and a chat opens
> where it was left.**

Three rules.

### The hub's chat list is a list

The rows of the workspace hub — and of the *closed chats* panel behind it,
which is the same list — get a cursor, and with it the grammar every other
list follows (`interaction-grammar.md`, *preview: the one open that does not
go*): arrows move the cursor and preview the chat in a joined panel while
focus stays in the hub, `enter` enters it, `cmd+enter` opens it as a panel of
its own, and a click moves the cursor to the row and previews it. The drawing
is already there: `fill_row` takes `(selected, marked)` and `table::line`
draws the selected line; the hub passes `false` today.

No marks. Nothing in Workshop acts on a set of chats, and a mark with no batch
verb behind it is a control that does nothing.

The cursor belongs to the panel instance and goes when the panel closes, as
every list's cursor does. It starts on the chat the panel previews if one is
already joined, else on the first row.

**The terminal keeps the keys it is given.** The hub embeds a live terminal,
and a terminal wants every key there is. It already answers only while it
holds the caret — `terminal/widget.rs` gates its whole `KeyDown` arm on
`cx.has_key_focus(self.area)` for a bound session — so the rule is exactly
that: the chat list has the arrows whenever the embedded terminal does not
hold the caret, and clicking into the terminal hands them over.

### A picker is a files panel with an errand

A panel that needs a file opens the files browser joined to itself, in **pick
mode**, and the browser hands back what was chosen. One panel kind, one
grammar, three callers.

The kernel grows the pair that makes this possible, beside `App::ask`, which
is the same idea one layer up:

```rust
/// What a panel is asking a picker for, while it is asking.
pub struct Want {
    /// The verb the picker wears: *add repository*, *attach 3*.
    pub verb: String,
    /// The line under the picker's header: *choose a local Git repository*.
    pub line: String,
    /// Directories rather than files.
    pub dirs: bool,
}

trait Panel {
    /// What this panel is asking a joined picker for. `None` for a panel
    /// that never asks, which is almost all of them.
    fn wants(&self) -> Option<Want> { None }
    /// The picker's answer, in the order the paths were marked. What it
    /// does with them is the asker's own business.
    fn took(&mut self, _paths: Vec<String>, _s: &mut Session) {}
}
```

A picker is `files` with a second argument, `files("~/code", "pick")`.
`Dir::of` still reads argument 0, so every existing panel and every saved
session is unchanged. It finds its asker through `Session::join_parent_of`,
exactly as Telegram's attach panel finds its chat, and it asks on every draw
and event — `Panel::verbs` is pulled with no session, so the answer is kept on
the instance and refreshed by `Dir::observe`, which already exists for the
chain.

**Pick mode is carried by everything the picker opens.** A crumb, `go to` and
a directory row all build a `files` id, and a file row builds a `file` one; in
a picker every one of them carries the second argument too, so walking into a
directory is another picker and the card a file previews into is a picker's
card. Without that, one step down is an ordinary listing wearing `copy`,
`move`, `rename` and `delete` — with the errand gone and the disk writable
again, one arrow-press from a picker. The asker is then the first panel up the
join chain that is not itself a picker, and *choose* closes the **outermost**
picker, so the whole walk goes with it and focus falls back to the asker.

**A picker chooses rows, and writes nothing.** What it hands back is the
marked set if there is one, else the row under the cursor — not the "object"
rule the writing verbs use, because in a picker the thing being chosen is
always a row in front of you. Its bar is the breadcrumbs, `go to`, the filter,
the marks, and the errand's verb: `new dir`, `copy`, `move`, `rename`,
`delete`, `copy path` and `copy here` are all gone, because a picker is a
question and not a hand on the disk. `cancel` still appears while a run is on,
since that rule is the app's and not the panel's.

What the errand cannot take is refused by name on the panel's own status line,
as every other files refusal is: a file where the errand wants a directory, a
directory where it wants files, a path that has gone since it was marked.

Answering closes the picker. A picker with no chat behind the join says so —
*open this from the panel that asked for it* — as the attach panel already
does when its own join is broken.

**The clipboard stays.** Mail's *attach* and Telegram's *add* keep reading
what the files app holds, and keep coming and going with it. The picker is
the way in when the file is not already in hand; `copy` in a files panel you
have open anyway is fewer presses than opening a second one, and the two are
not two ways into one action — one takes what you have already chosen, the
other is where you choose. What they share is the code behind them: both
arrive at mail's `attach_picked` and at the chat's `carry`.

### A chat opens where it was left

`workshop_chat` remembers a reading position: **the row that was at the top of
the view, and how far into it**. Not a pixel offset alone, which means nothing
once a card opens above it, and not a row index, which is a number about a
draw rather than about the transcript.

The offset travels only where the row it was measured into will be the same
row, the same height, on the way back. Two rows are neither: a line inside an
open card, which is not drawn at all next time, and an open card itself, which
is drawn closed — an offset measured down its output would land past it. Both
come back to the top of the nearest keyed row instead, which is a line or two
early and never past what was being read.

Every transcript row therefore gets a key, namespaced because three tables are
drawn into one list: `msg:` for a person's turn and for a message-level prose
fallback, `item:` for a card and for a turn's prose (one turn is several text
rows, so the message id does not name one of them), `call:` for an app call,
`step:` for a *view changes* line. `Row::Card` already carries an identity
(`CardKey`, which overloads the item namespace with negative message ids —
hence the prefix); `Row::User` and `Row::Text` did not. A row inside an open
card gets none: nothing is open when a chat is opened again, so the anchor is
always the nearest row at depth zero.

An **empty** key means the tail, and the tail is what a chat that has never
been scrolled shows: the column's default is the behaviour we have today, and
a chat at its end keeps following what arrives, as it does now. Scroll up and
the anchor is the first visible row; scroll back to the end and it is empty
again.

The position is the **chat's**, not the panel's, and it survives a restart.
Two panels on one chat are last-writer-wins: a chat is one conversation
however many panels show it. It is written through the same command dispatcher
as every other Workshop write, after a pause of 300 ms — Telegram's drafts
wait exactly that long. Two things can happen inside that pause, and each has
its own hook: a quit or an undo walk, which `Panel::flush` is called for, and
a close, which only `Drop` sees. Neither waits for the writer — an undo is a
keystroke and a close happens on the frame of the press, and the serial
writer may be a transcript's worth of commits deep; a quit is safe without
waiting because the shutdown's last act is already a barrier over every
accepted write. What a reading *is* therefore lives in the store's runtime
from the moment it is asked for, and a chat opening reads that rather than
its row: a panel closed and opened again inside the gap would otherwise come
back to where the reading before last left it, and stay there. The command is
bookkeeping,
so it records no history node, and it leaves `last_used` alone — reading a
chat is not using it — and asks nothing of the workspace, so a closed chat and
an archived workspace are read as readily as any other.

A saved position that is not the tail leaves unread alone, because the read
receipt already asks for the focused panel *and* the visible tail
(`ChatReadTracker`); coming back to the middle of a transcript acknowledges
nothing. A key that is no longer in the transcript falls back to the tail.

## The surface

- The hub and *closed chats* draw a selected chat row, and arrows walk it.
- *add repository* keeps its path field and gains **browse** (`b`), a link to
  the picker. Choosing adds the repository and opens Projects.
- Mail's compose sheet gains **browse** (`b`), a link to the picker, appended
  before *attach* (`cmd+h`), which is unchanged and still comes and goes with
  the clipboard.
- Telegram's attach panel is unchanged except that its *browse* (`b`) now
  opens a picker rather than a plain listing; *add* (`d`) stays.
- A picker looks like a files list with one verb and one line of words under
  the header.
- A Workshop chat opens where it was left.

## Phases

1. **The walk.** A cursor on `Row::Chat`, the arrow/enter/`cmd+enter`/click
   handling in `WorkshopDetail::handle_event`, gated on the embedded
   terminal's caret. `workshop.md`'s hub paragraph, which today claims
   "ordinary panel navigation applies" for a list that has none.
   `e2e/workshop/basic.txt` walks to *chat 1* with arrows instead of clicking
   it — and its `click "chat 1: Codex"` followed by `key cmd+w` has to become
   an `enter` if a click becomes a preview.
2. **The picker.** `Want`, `Panel::wants`, `Panel::took` in
   `kernel/src/panel.rs`; pick mode in `files/panels/dir.rs` — the second
   argument, the observed want, the bar, the choose, the orphan line;
   Workshop's `workshop_add_project` as its first asker. `files.md` gains a
   section, `panel-model.md` a line about the join that carries an errand.
3. **Mail and Telegram ask.** `mail.browse` beside `mail.attach`, both landing
   in `attach_picked`; `telegram.browse` opens the picker and `took` reaches
   the same `carry` as `telegram.add`. `mail.md`'s *carrying a file* and
   `telegram.md`'s attach paragraph gain the second way in. E2E: `e2e/mail`
   attaches through the picker and keeps its clipboard step; `e2e/telegram`
   carries two files through the picker.
4. **Where you were.** Schema V5 — appended after `recover_items`, since the
   ladder only grows at the end — the row keys, `Command::SaveReading` in the
   bookkeeping match, the 300 ms timer with `Panel::flush` and `Drop` behind
   it, the positioning in the draw. Tests: the position is written without a
   history node and without moving `last_used`; a closed chat is read as
   readily as an open one; a reading inside the pause survives both a quit and
   a close; a chat opening reads the newest position rather than the row
   behind it; and an offset is kept only where its row will not have changed.

Phases 1 and 4 touch only Workshop; 2 and 3 are one change split by caller.

## Decided in review

- **A click on a chat row previews**, as the grammar says, and moves the
  cursor there; it no longer opens and takes focus. It also takes the caret
  off the embedded terminal, which is what makes the arrows that follow the
  list's. `e2e/workshop/basic.txt` says `enter` after the click it used to
  rely on.
- **Leaving the embedded terminal** needs no new chord: a bound terminal now
  lets the caret go when its panel loses focus, so `cmd+←/→` away and back, or
  a click on a chat row, gives the keys to the list. `esc` stays the
  terminal's own, because it sends `ESC`.
- **The word is *browse* (`b`)**, in all three bars, which is what Telegram's
  attach panel already called it. Mail's sheet wears *send*, *discard*,
  *browse* and *attach*; a files card wears seven, and bars wrap.
- **A picker keeps no `new dir`.** `add_project` runs `git rev-parse
  --show-toplevel` and an empty folder fails it, so the `git init` evening
  happens in the hub's terminal either way.
- **The picker closes on answering** — the outermost one, so focus falls back
  to the asker.
- **The anchor is two columns**, `anchor_key` and `anchor_scroll`, which read
  better from `sql.query` than one packed one.
- **The position is the chat's**, in the store, because the complaint is about
  coming back after a restart.

## Considered and not chosen

- **The platform's own file dialog.** `NSOpenPanel` is one call and is what
  every other Mac app uses. The shell has no modal surface at all, Android has
  no equivalent, and a dialog cannot be filtered, marked, walked or joined —
  which is most of what the files browser is for.
- **Delivering the pick through the files clipboard** — a *choose* that fills
  it and an asker that reads it back. The clipboard stays as its own way in,
  but it is the wrong wire for an errand: it cannot say *what for*, so the
  verb that takes it is drawn on a guess about what a person meant by `copy`,
  and a picker that filled it would silently throw away whatever was held.
- **A pending-pick row in the store.** An errand that outlives the process is
  an errand nobody remembers asking for. The join is the shell's own word for
  "these two panels are about one thing", and a broken join already has a
  sentence in this app.
- **Making the hub's chat list a rich table.** A `RowSpec` over a `SqlSource`
  brings a filter, marks, batch verbs and a header rule into a panel that also
  holds a PR line, a chat list and a terminal — for four rows that need a
  cursor and nothing else.
- **A pixel scroll offset alone.** It is right until a card opens above it.

## Not done on purpose

- **No picker for a destination.** Nothing in the app saves a file to a place
  a person chooses, so the picker only ever answers with things that exist.
- **No favourites, no recent folders, no second root.** `~`, the breadcrumbs
  and `go to` are how the browser is navigated, and a picker is the browser.
- **No saved position in the review diff, the activity list or the GitHub
  panel.** A diff is read from the top and an activity list is short.
- **No cross-device reading position.** Workshop declares no replication and
  this column is no exception.
- **No cursor in the hub's terminal.** It is a terminal.
