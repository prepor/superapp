# CR-014 · The trash: a fifth mailbox, and the letter that comes back

Status: **proposed** (Andrey, 2026-09-07: "we need to add trash mailbox with
an ability to untrash").

## Why

`delete` is the one verb every mailbox has, and it is the only one whose
result nothing shows. The mail moves to the account's trash folder, the row
leaves the list, and from that moment the app has no surface that admits the
letter exists: the trash is not a panel, not a root, not a search hit, and the
reader draws an empty card if you reach one. The only way back is `cmd+z`
while the history node is still there — a session-long window, gone on the
next restart.

Every other folder role already has its list. The trash is one more of the
same, and *put back* is what makes deleting a decision you can revisit rather
than one you make once, quietly, forever.

## The model

> **The trash is a mailbox like the other four. What leaves it goes back
> where it came from.**

Two rules, and the second is the whole of the new machinery.

### `Role::Trash` is the fifth role

`Role` gains `Trash`, `ROLES` becomes five, and everything keyed on the enum
follows: the tag `trash`, a panel kind, a launcher root after **spam**, a
template, a scene, a title, an `about`. `Role::named("trash")` starts
answering `Some`, which is what makes `model::role_of` and
`model::thread_siblings` work over a deleted letter instead of treating it as
a mail with no mailbox.

The store needs nothing for this: `folder.role = 'trash'` has existed since
V1, the seed creates the folder, the sync pass mirrors it like any other
special-use folder, and `file_tx`, the push pass and `Filed` are all written
in terms of a role rather than a list.

### `trashed` remembers where a letter was

A restore that always lands in the inbox is a lie the first time you delete
something out of Sent: a letter you wrote appears among letters you received,
and nothing says why. So deleting records where the letter left from, and
putting it back reads that row:

```sql
CREATE TABLE trashed(
  message INTEGER PRIMARY KEY REFERENCES message(id),
  folder  INTEGER NOT NULL REFERENCES folder(id)
);
```

A side table and not a `message` column, because of the rule V1 is built on:
`raw` sits last so that everything a list reads is decoded before the letter's
own bytes, and a column added by `ALTER TABLE` lands *after* it. One row per
letter currently in the trash — written when a letter is filed there, deleted
when it leaves by any road, including undo. Schema step **V4**.

Put back resolves per letter, in this order: the folder `trashed` names; the
account's inbox if that row is missing or its folder is gone. A letter that
arrived in the trash from the server — deleted on the phone, mirrored here —
has no row and no history, and the inbox is the honest answer for it.

## The surface

**The list.** `trash` is a fifth tag over the same panel kind, the same row
and the same filter grammar. Its bar is `sync`, then `put back n` (`cmd+p`)
and `mark all` / `clear` while rows are marked — and **no** `delete`, because
this is where delete goes. `keep_verb` gains a `Trash` arm and the swipe
follows it: leftward puts back, rightward does nothing, since
`swipe_verbs` asks the panel for both verbs rather than assuming `mail.delete`.

**The row.** The four lists count a conversation with the trash left out, so a
trash row would say "0 letters, nobody" — the aggregates must invert for this
one spec: a trash row's participants and count are its *trashed* letters.
`mailbox_spec!` takes the folder test as a second argument (`IS NOT 'trash'`
for the four, `IS 'trash'` for the fifth).

**The reader.** `model::thread` drops the trash, which is why a deleted letter
opens empty today. The rule becomes: the trash is left out **unless the letter
the panel was opened on is itself in it**, and then the conversation is drawn
whole — the deleted letter beside the ones still filed, because that is the
conversation you asked to see. In the reader's bar, a letter read out of the
trash wears `put back` (`cmd+p`) where the others wear `delete`; `archive`,
`reply` and `forward` stay what they are.

**The agent.** One tool, `mail.put_back`, mirroring `mail.not_spam` —
"take a conversation out of the trash and back where it came from". And
`mail.delete`, which scans `ROLES` to find the conversation, skips the trash:
finding it there and answering "there is nowhere to delete that to" is not an
answer.

**The launcher.** Roots become inbox, archive, sent, spam, **trash**, new
mail, settings. Search keeps leaving the trash out — but its reason changes,
and the book has to say the new one: not "a reader opened on one has nothing
to show", which stops being true, but that the trash is a decision, and the
place to go looking through it is its own list, which filters like any other.

## Phases

1. **The fifth mailbox.** `Role::Trash`, the spec with its inverted
   aggregates, the kind, the template in `root.rs` (twice — stage and
   library), the root, `about`/`title`, the scene node, the panel-context
   test's sample id. `delete` off the trash's bar. Deleting still works
   exactly as it does; the trash is merely visible.
2. **Putting back.** `trashed` and schema V4, written in `file_tx` and
   cleared by every other move including `Filed::reverse`; `put_back_tx`;
   the `PutBack` intent (its reverse is "back to the trash, and the row with
   it"); the bar verb, the batch verb over marks, the swipe, the reader's
   verb, `mail.put_back`, and the words — `word_of` and `nothing_said` are
   keyed on the destination role today and have to be told the source.
3. **Reading and telling.** The reader's trash-aware conversation; the demo
   seed grows one deleted conversation so the list is not empty; `mail.md`,
   `interaction-grammar.md`'s swipe paragraph, `agents.md`'s tool table, and
   `MAIL_DESCRIBE`, which currently states "only the first four have panels".
   E2E: `triage.txt` deletes from the inbox, opens the trash, puts it back,
   and finds the row in the inbox again.

## To decide in review

- **The word.** *put back* (`cmd+p`) is what the Finder's trash says, and `p`
  is free in both bars. *restore* wants `cmd+r`, which is free in a list and
  taken by *reply* in the reader; *not trash* would wear `n` and match *not
  spam* exactly, at the cost of reading like nothing anybody says.
- **Where it lands.** The `trashed` table, or always the inbox. Always-inbox
  is a dozen lines and no schema step; it is wrong for Sent and quietly wrong
  for the archive.
- **Search.** Keep the trash out (proposed), or let it answer now that a hit
  is readable.
- **The demo seed.** One deleted conversation makes the panel and the library
  scene real; it also moves the per-role counts the mail tests assert.
- **The root.** Whether the launcher lists the trash at all, or whether it is
  a panel you reach only by name.

## Considered and not chosen

- **The undo tree already knows.** `Filed` carries `from_folder`, so the node
  that deleted a letter can put it back exactly where it was — and that is
  `cmd+z`, which stays what it is. It cannot be what *put back* reads.
  History is in memory and says so (`kernel/src/history.rs`): it is gone on
  restart, it keeps 200 nodes and drops the rest, and a node expires whole
  when one of its intents stops being reversible. A letter deleted on the
  phone and mirrored here never had a node at all — and a day after the
  delete, that is most of what the trash holds. The tree is also a *walk*,
  not an index: putting one conversation back out of the middle of it would
  mean finding the node that moved mail 42, downcasting its intent, and
  reversing that one claim out of order — which is exactly the thing the
  tree refuses to do, because the layout snapshots either side of it would no
  longer describe anything. `trashed` is two columns that survive a restart;
  the alternative to it is always-inbox, not history.
- **`server_msg.folder`.** The server's last word is the old folder only
  until the push lands, and then it is the trash like everything else.

## Not done on purpose

- **Emptying the trash.** No *delete forever*, no expunge, no retention rule.
  The provider's own trash policy is what removes mail, and a server deletion
  already removes the local row.
- **A trash per account.** The list is over the role, as the other four are;
  `@account:` narrows it.
- **Undo across restarts.** `trashed` is where a letter came from, not a
  history entry; putting a letter back is a new action with its own node.
