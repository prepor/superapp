# CR-005 · Mail triage: preview, delete, row swipes

Status: **implemented** (Andrey, 2026-09-01: focusing a row should open the
message immediately without moving focus; the message's shortcuts should work
from the list; delete belongs beside archive; android needs row swipes —
"smooth and nicely animated").

## Why

Reading mail costs more moves than it should, and filing it costs more than
reading it.

- **The cursor walk shows you nothing.** Arrows move a selection wash and
  stop there. To read the mail you press enter, which throws focus into the
  message — so the next mail is `cmd+←`, `↓`, `enter`. Three keys to do what
  one arrow does in every mail client written since 1995.
- **Triage is a mouse errand.** `archive` exists only as an 18 pt button on
  the header of a *focused* message panel. There is no `delete` at all,
  though the trash folder, its IMAP role and the push pass that would sync it
  have all been in the store since CR-001.
- **On glass it is worse.** The only route to archive is tap → read → aim at
  a button in the chrome, and a sideways finger — the gesture every phone
  user already has for exactly this — was defined as meaning nothing.

## The model: preview, and the pair that shares its keys

**Touching a row opens it, without leaving the list.** Walking the inbox with
the arrows — or clicking a row — re-targets a joined message beside it and
keeps the keyboard, so the next arrow keeps walking. `enter` is the one that
hands focus over, which is the solid-link rule the book already had; `cmd+→`
is the other way there, and cmd+click still means a fresh un-joined panel.

> **touching a row previews · `enter` goes**

A preview is a real open — joined, marks read, undoable — minus the one thing
that would end the walk. It is *not* a new kind of panel, and deliberately not
a flag threaded through the general open path: a preview is what a
**master/detail list** does, so the declaration is `ui::preview_kind`, which
answers "what does this panel preview into" and is `None` for everything but
the inbox. Future list kinds (rss, telegram) opt in by answering it.

**A preview needs somewhere to be.** Where the driver and its child cannot
share the screen — a phone grid, where each panel is the whole of it — the
open behaves like any other and goes. That is a geometric test
(`Ws::fit_together`), not a platform one, so a narrow desktop window degrades
the same way: a preview nobody can see is worse than no preview.

Two consequences fall out of the pair being one working surface:

- **it borrows keys** (below);
- **the cursor tracks the preview both ways.** Walking the list moves the
  message; walking the message with `cmd+n`/`cmd+o`, or clicking `older →`
  inside it, moves the cursor. Master and detail can never disagree about
  which mail is open.

### The fifth letter rule

CR-003 fixed four rules for accelerators. Preview needs a fifth, because a
list with a mail beside it should answer to that mail's keys:

> **5. A panel that previews borrows its preview's accelerators.** The
> driver's own keys win first; the preview lends what is left. The borrowed
> mark is never drawn on the borrower — it stays on the previewed panel's own
> chrome, one column over and in plain sight.

That last clause is the whole argument. CR-003's case for putting keys on cmd
rather than bare letters was that *the mark must never lie*. A borrowed key
does not create a hidden binding: it makes a visible control reachable from
the panel standing next to it. Nothing goes bold on the inbox.

**`refresh` became `sync` to pay for this.** Rule 2 says two visible controls
may not share a letter, and the previewed message's `reply` already owned `r`.
`sync` is also the truer word — the button kicks the IMAP workers, it does not
reload a view — and it leaves `cmd+f` unclaimed rather than spending the chord
every hand reaches for on the one thing it does not mean.

**The guard.** Rule 3 gives `c v x a` to a panel whose text can be edited, and
the inbox has a filter — so a naive lend would make `cmd+a` archive instead of
select-all. Borrowed keys therefore **stand down while the driver's own field
holds the keyboard**. Rule 3 survives intact: the list yields the text chords
to its field exactly as before, and lends them only when the field is not
listening. The unit test holds the *union* of a driver's keys and its
preview's to rules 1 and 2, so the pair is checked as the single surface the
user actually faces.

## Delete

`delete` joins `archive` on the message header, on `cmd+d`. Buttons draw right
to left from `×`, so it reads **DELETE ARCHIVE ×** — the destructive one
furthest from the corner, the order compose already uses for discard and send.

Both moves now go through one `file_tx(id, role)`, and both grew an `EXISTS`
guard they should always have had: the old subquery set `folder = NULL` when
an account's server advertised no such folder, and a mail with a null folder
falls out of the inbox query *and* out of the push set's join — vanishing with
nothing left to sync it back. The shell asks before acting, so a missing
folder is a toast rather than silence.

One `Stage::triage` serves every route — header button, borrowed chord, row
swipe — so the undo node, the toast, and the closing of the mail's readers are
the same story every time. Filing also carries the list's cursor to the next
mail **inside the same action**, so one `cmd+z` takes back the filing and the
move together rather than needing two.

## The row swipe: an ink curtain

Sideways on a mail row triages it: **left archives, right deletes**.

The row does not move. A filled panel carrying the action's name **wipes
across** it, entering from the edge that action's button occupies in a message
header — swipe left and `archive` comes in from the right, exactly where the
header draws it. The two surfaces agree about which side means which verb.

A sliding row was the obvious alternative and was rejected on look: a card
sliding aside to reveal a layer beneath is a materials metaphor this design
does not otherwise use. A curtain wiping across is the same move as a header
button's hover inversion, which is also how it says *this will fire*: under a
third of the way it is a grey wash with the word in ink; past the threshold
the whole thing inverts. No colour, and nothing to read but the word.

It is drawn by the shell rather than by the row widget, for a reason worth
recording: a `PortalList` item may not carry an `Overlay` — quads under an
overlay ancestor inside a list item never paint (the same trap the selection
wash already works around). The shell draws it inside the panel's own clipped
turtle and against a fresh draw call, or it would merge into the chrome's call
and paint *under* the panel it belongs to.

The gesture is not `cfg`-gated to android: leaving it reachable on desktop is
what gives the e2e harness a door onto it.

## The lag, and what it actually was

Walking the inbox felt laggy against a real account, and the obvious suspect
— "we must be waiting for the panel to commit" — was right about the place
and wrong about the reason. The commit is not the cost. Measured:

| | |
|---|---|
| key handling (`move_sel` → action) | 0.04 ms |
| a whole frame (`draw_walk`, everything) | 0.5–1.6 ms |
| `store.act`, uncontended | 0.10 ms |
| `store.act`, worker holding the write lock | **468 ms** |

`fetch_account` opened `BEGIN IMMEDIATE`, then did the IMAP fetch, then
committed — holding SQLite's single write lock across a network round-trip.
Every preview needs that lock, and `State::act` ended by kicking every
worker, so each arrow press started a sync pass that took the lock across
its next fetch, which blocked the next arrow press. Walking the inbox was a
keystroke-driven denial of your own write lock.

This branch fixed it by restructuring the pass, and **CR-004 fixed the same
thing independently and better** while this work was in flight: *no effect
runs inside a transaction* is now a rule of the world rather than a property
of one function, and the push pass does not talk to the server at all. That
fix is the one that landed; this branch's was dropped in the merge, along
with the unit test that probed for the lock on every transport call — worth
porting to the new shape, but not here (see the PR's follow-ups).

The pre-existing part is worth keeping on the record: the `cmd+n`/`cmd+o`
reading walk had the same shape before any of this, so the hazard predates
preview — preview only made it something you do continuously. And nothing in
the suite could have caught it, because every e2e run uses a fresh seeded
store whose demo account has no `imap_host`: **no sync worker has ever
existed in a test.**

## A bug this uncovered

`act_pid` — "which panel does this hit belong to" — returned `None` for a
hosted widget's own hits. Those hits are pushed *last*, so they sit above the
panel-wide focus hit, and a finger landing on one resolved to no panel at all.

**One-finger vertical drag starting on an inbox row did not scroll the list.**
Nor on a message body, nor on any form field. On a phone, where the inbox is
almost entirely rows, only the filter strip and the padding gutters scrolled.
`e2e/touch.txt` has been photographing an unscrolled list under the caption
`g1-touch-scroll` for as long as row hits have existed — the screenshot was
honest, nobody was reading it.

## Costs, named honestly

- **Previewing marks read, and pushes `\Seen`.** Walking the inbox marks
  every mail it settles on, on the server too. This is the model CR-001
  chose ("undoing an open makes the mail unread again exactly if the open
  unread it"), and a walk coalesces into one node — so one `cmd+z` takes the
  whole run back. It is still a real change made by a navigation key.
- **Previews are not paced at all**, and the reasoning that said they had to
  be was wrong. The worry was that each one is a transaction plus a merge
  into the head node's growing changeset, so a held arrow would be O(n²) on
  the UI thread. Measured on a file-backed store, a preview costs **0.1–0.3
  ms** and gets *faster* across a burst, not slower: forty of them total
  about 8 ms, well inside one frame. The UI tables are a handful of rows, the
  store is WAL with `synchronous=NORMAL` so there is no fsync per commit, and
  coalescing rewrites the head node instead of appending to it.

  Two throttles were built and both thrown away. A trailing debounce made
  *every* walk wait out the full delay. A leading-edge one felt fine but
  still queued mid-burst landings — and a queued preview restores focus when
  it finally fires, so a walk ending in `cmd+→` got snapped back to the list
  a frame later. **The pacing bought nothing and cost a focus bug**; the
  preview now goes straight through, and `Act::Preview` restores whatever
  focus it found rather than assuming the driver still holds it.
- **Triage closes readers on every workspace.** `Wm::showing` deliberately
  looks past the active workspace, so archiving on space 1 closes the same
  mail's panel on space 3. That is one action's delta reaching somewhere the
  user is not looking; `cmd+z` restores it, but it is worth knowing.
- **Preview is a desktop/unfolded behaviour.** On the 4×3 cover grid the
  inbox and a message are each full-screen, so `fit_together` says no and a
  tap goes, exactly as it did before this change. The phone's answer to the
  same problem is the swipe.
