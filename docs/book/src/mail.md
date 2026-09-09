# Mail

The mail app is five mailboxes over one list, conversations, drafts, contacts,
with real IMAP and SMTP behind them. It registers nine panel
kinds, its own schema ladder, a demo seed, four deferred effects, three
capabilities of its own, a search source, two problem sources, and one worker
per account plus the sender.

A window's own run reaches real servers. Every scripted run, every test, and
every panels-library mount gets a fake set of servers instead, which registers
itself under all three capability traits and under its own type, so a test can
plant a letter or take the servers offline. The demo account a fresh store is
seeded with carries the fake servers' hosts only where a sync can reach them,
in a scripted run and in a test; in a real run it has the same letters and no
hosts, so no sync worker runs for a mailbox that is not out there, and in a
library mount — a world with the clock and nothing else — it has none either,
so no pass fails on every panel of the canvas.

## Tags and roots

| Tag | Argument | What it shows |
|---|---|---|
| `inbox`, `archive`, `sent`, `spam`, `trash` | none, or a sender to filter by | one mailbox |
| `message` | a mail id | one conversation |
| `compose` | none, or `reply`/`forward`/`reopen` and a mail id | one draft |
| `contact` | an address | one correspondent |
| `attachment` | a mail id and a part index | one part of a letter, as a card |

The roots the launcher offers, in this order: **inbox**, **archive**, **sent**,
**spam**, **trash**, **new mail**. Shared account settings now belong to
[Accounts](./accounts.md), which also preserves the historical `settings` and
`add_account` tags. Mail is listed first among the
apps, so a store nobody has booted comes up on the inbox.

## Five mailboxes, one list

Inbox, archive, sent, spam and trash are five tags over one panel kind, one row
shape, and one query written out five times so the folder role is a SQL
literal. They share rows, filtering, marks, cursor movement, and message
previews.

What differs is one verb: the verb that *keeps* a conversation. Only the inbox
offers **archive**, because everywhere else the mail is already out of it;
only spam offers **not spam**, which puts a conversation back in the inbox —
the same move in the other direction; and only the trash offers **put back**,
which is that move again, to wherever the delete took the letter from. The
archive and Sent offer none of the three.

**delete** is the move every mailbox has but the trash, and it means moving to
the account's trash folder. The trash wears no delete at all: that is where
delete goes.

Deleting writes down where each letter was, in a `trashed` row per letter, and
*put back* reads it. A letter that arrived in the trash from the server —
deleted on another device, mirrored here — has no such row, and goes to the
inbox. The undo tree knows where a letter came from too, and better, but only
until the process ends: history is in memory, keeps its last two hundred
nodes, and never had a node for a delete that happened elsewhere. `cmd+z` is
the walk back through what you just did; *put back* is a verb over one
conversation, weeks later.

A row's participants and its count leave the trash out — what a conversation
is, is what is left of it — except in the trash itself, where they count the
deleted letters alone, since a row there is what was thrown away.

The filter tags are `@unread`, `@html`, `@from:`, `@subject:`, `@date`, and
`@account:`. Free text matches the sender's name, the sender's address and the
subject as a substring — in Sent, the recipients and the subject, so a list is
searched by what it shows — and the letter's own text through the same FTS5
index the launcher searches, so a word you remember from a body finds the
conversation it was written in; a conversation matches when any of its letters
does. The body goes through the index rather than a scan because a filter runs
on every keystroke and a mailbox of twenty thousand letters is a couple of
hundred megabytes of them. What that costs is the middle of a word: `thermo`
finds *thermos* in a body and `hermos` does not, while `ovac` still finds
`vera@kovac.io`, which is a column and is scanned. `@from:` suggestions come
from the current mailbox: the three ordinary mailboxes offer correspondents
with spam left out, and spam offers its own senders, because a spammer is not a
correspondent.

A mailbox panel may carry a sender as its argument, which is what a contact's
link opens: the panel is filtered from its first draw, and the field shows
`@from:vera@kovac.io` so the next edit is the person's.

The bar is `sync` (`cmd+s`), and while rows are marked, `archive n` (`cmd+a`,
inbox only), `not spam n` (`cmd+n`, spam only), `put back n` (`cmd+p`, trash
only), `delete n` (`cmd+d`, everywhere but the trash), `mark all` (`cmd+m`),
and `clear`. *mark all* wears `m` rather than `l` because the
shell keeps `cmd+l`; *clear* wears no letter because `esc` is the table's.

## Threads: the row is the conversation, the panel is the whole of it

Each mailbox row is a conversation with at least one message in that folder. It
shows the participants newest speaker first with the account's own address as
*me*, the message count, the subject of the oldest message with `Re:` and its
translations stripped, and the date of the latest message in that folder. Sent
is the exception, and for the reason the rest is the rule: its letters all have
the same sender, so a row there names who they *went* to — the addresses of the
conversation's own sent letters, newest first. It is
bold while any message is unread. The same conversation can appear in more than
one mailbox, and the count covers the whole conversation either way.

Thread membership is calculated when mail is received, from `References` and
`In-Reply-To` headers within an account. The code handles a parent that arrives
later and siblings that share a missing parent, and it does not use
subject-line guesses. `message.thread` stores the smallest message id in the
conversation as a stable anchor, and the row groups by it.

Filing a row files every message from that conversation that belongs to the
current mailbox, as one undoable action. A later reply can place the
conversation back in the inbox.

A row opens the folder's oldest unread message, or its newest when everything
has been read.

A message panel shows the whole conversation oldest first, deduplicated by
`Message-ID` so a reply that exists both in Sent and in the list appears once.
The deleted letters are left out of it — a conversation is what is left of it
— unless the letter the panel was opened on is itself in the trash, and then
it is drawn whole: the deleted letters beside the ones still filed, which is
the conversation you asked to see. Where a letter has two copies, the one that
stands for it is never the deleted one, and then it is the copy outside Sent:
what was deleted is the copy that came back through the list, not the letter.
That choice is also what a verb over the conversation acts on.
The TO line of its first letter is at the top: the account's own address for a
conversation that came in, and the person it went to for one this mailbox
started — a letter's recipients are read off its own `To` header and kept on
its row, because the account answers the first case and nothing but the
letter answers the second. Each message is one row that folds open in place:
closed, it shows the sender, the first content line or error, and the date.
It opens unfolded from its first unread message down — the read run above
that message is what folds, so catching up on a conversation is one read from
where you left it, and a message under an unread one stays open whether or not
another client already flagged it. A conversation with nothing unread opens on
the message the panel names alone — from a mailbox row, its newest. Quoted text is folded behind
the reading rules, because in a conversation the quote is the message above.
These open states are the instance's own context and are not part of undo
history; after a restart only the message the panel names starts open.

Opening a conversation marks its unread messages as read. That write is claimed
by the open, so it lands on the same undoable node as the panel appearing and
one undo gives both back. A cursor walk that previews a row at a time coalesces
into one node, so one undo closes the whole walk.

The reader's bar is `archive` (`cmd+a`), `delete` (`cmd+d`), `reply` (`cmd+r`),
and `forward` (`cmd+f`), plus `not spam` (`cmd+n`) over a letter read out of
the spam folder and nowhere else: archiving and deleting are moves any letter
has, while a letter is junk or it is not. Over a letter read out of the trash,
`put back` (`cmd+p`) takes the place `delete` has everywhere else, because
deleting a deleted letter is the one filing with nothing to do. The filing verbs are buttons; reply
and forward are links, so they follow the [solid-link rule](./interaction-grammar.md#the-three-interactive-signals)
and open a draft joined to the reader. Filing closes the reader's own slot and
nothing else: another panel reading the same conversation stays where it is and
says what it shows. The list driving that reader then walks on to the row that
took its place, and that walk is part of the filing rather than a second
action — one undo puts the mail back, reopens the reader on it, and takes the
walk's own read mark back with it.

A reply fills the recipient and subject, quotes the source message, and sends
`In-Reply-To` and `References` headers. The recipient is the letter's sender —
or, over a letter this account itself sent, the people that letter went to,
because answering one's own letter means writing to them again and not to
oneself. The field is a comma-separated list either way, and a send addresses
every name in it. A forward starts with an empty
recipient, adds a forwarded-message header block, and keeps the reference chain
without naming a reply parent. Sent mail then joins the same conversation. A
forwarded source shows a muted mark once the letter has actually left, and only
where the server keeps the `$Forwarded` keyword.

## Attachments

An open message lists the parts not already drawn in its body as `name · size`,
with at most five links followed by a remaining count. An inline image already
in the body is not listed under a picture of itself.

An attachment is `("attachment", ["42", "3"])`: the letter and the part's place
in it, because a derived row's own id is local to a device. It opens in the
same card widget a disk file uses, and shows the name, the media type, the
size, the letter it came with, and the shared [viewer](./viewers.md) for text, PNG/JPEG images, or PDF pages. A part
offers shared fit and zoom verbs beside `open` (`cmd+o`), which writes the part to
a per-part directory under the system temporary directory, keeping the sender's
filename, and asks the operating system to open it.

Attachment bytes stay out of SQLite and device sync. `message.raw` holds a
versioned content snapshot: the MIME reading with file bodies removed, plus
each file's description, decoding headers, and IMAP section number. Messages
and their attachment metadata are committed and replicated together, so the
list is available with the message. Part indices stay stable for existing cards.

Previews, inline images, and `open` download the requested section over IMAP
on a worker; cards show `loading preview…` while awaiting content. Files use
the same bounded, least-recently-used local cache as Telegram, so a cached
file works offline and an evicted file downloads again when needed.
Cache identity includes the account, server folder, UIDVALIDITY,
UID, and section; a stale UID generation is refused. The cache's SQLite index
holds filenames and sizes only; file bytes stay on disk. File sizes shown from
the server's MIME structure are estimates.

Agents use `mail.attachment` with the letter's `mail` id and MIME `part`
index to read the file directly. `mail.thread` includes each attachment's
name, media type, size and those ids. The read runs on the agent worker and
uses the same IMAP/cache path as a preview. PDFs with text layers and
UTF-8/UTF-16 text files up to 32 MiB are supported; longer text is read in
64 KiB chunks using `next_offset`. Scanned PDFs need OCR. No panel, manual
export, or mark-as-read action is needed.

## Carrying a file

A draft carries **paths**, not bytes. Attaching costs one `stat`; the file
stays where it is and the send is what reads it, so a draft that sits for a day
carries the file as it is when it leaves, and a file that has moved fails the
send honestly instead of going out stale.

To carry a file, open its card in [Files](./files.md) and choose `copy`. The
compose panel then offers `attach` (`cmd+h`), and shows a `CARRIES` line with a
link per file. The compose instance asks the registry for the files app on
every draw and reads its clipboard; a build without files never shows the verb.
The clipboard is not consumed, because a move cannot mean "and take it off the
disk" when the letter carries a copy.

A directory is passed over. A file past 25 MB is refused with its size named. A
path the draft already carries is ignored. Attaching is one undoable action
that adds only what it added.

Each row also records which install picked the file, because the same path is a
different file on another machine and these rows replicate. Sending refuses a
path attached elsewhere, and refuses a file that has since grown past the limit,
each by name.

## Contacts

A contact panel shows the name as of the latest letter, the address, and how
many messages are in mail. Its one verb, *messages from …* (`cmd+m`), opens the
inbox filtered to that address. An address nobody has written from still opens
and says so.

Mail's search source — what the [search panel](./interaction-grammar.md#search)
puts a question to — answers with the people who wrote first and then the
letters a query's words reach, best match first, out of an FTS5 index. Spam is
left out of the sender side, so nothing a search or a compose field offers came
out of the junk, and the trash is left out of the letters: what you deleted is
what you decided you were not looking for. The trash has its own list, and it
filters like any other mailbox, which is where to go looking through it.

## Accounts

[Accounts](./accounts.md) owns the shared account list and sign-in form. Mail
uses its existing account IDs, hosts, keychain entries and Google grants;
Google Calendar uses the same identities with separate service permissions.
A disabled Mail service keeps cached messages but stops its workers. The
shared list shows the address, hosts, status, service switches and removal
confirmation. Password/IMAP accounts continue to work as before.

## Gmail sign-in

**sign in with google** (`cmd+g`) starts the installed-application flow: the app
binds a temporary loopback listener and mints its PKCE pair before opening
Google's consent page in the system browser, because a redirect to a closed
port is lost. It never asks for the Google password. A scripted run refuses the
flow in one line, and a second press while one is waiting is refused too.

| Value | Lifetime | Storage |
|---|---|---|
| Authorization code | seconds | only inside the OAuth module |
| Refresh token | until revoked | platform secret store |
| Access token | about one hour | process memory |

When Mail is selected, the app checks that the granted scopes include full
mail access before enabling the service, and reads Google's XOAUTH2 error response so it can tell a missing
scope from disabled IMAP. Signing in again as an existing Google account
renews the grant rather than adding a duplicate; an address already present as
a password account is refused, because its hosts are another provider's.

Gmail uses All Mail as the archive target but never as an ingest source: a MOVE
into All Mail is what archiving means there, and importing it as well would
file every inbox message a second time. The cost is stated rather than hidden:
mail archived on another device may not appear locally. Gmail also files its
own Sent copy, so the usual IMAP append is skipped for it.

Superapp needs the developer's Google Desktop-app registration. Set
`SUPERAPP_GOOGLE_CLIENT_ID` and `SUPERAPP_GOOGLE_CLIENT_SECRET`, or place the
downloaded configuration at `google-oauth.json` beside the database. A Web-app
registration is refused by name, because it cannot accept the temporary
loopback port.

The browser consent step is not part of end-to-end tests. The URL, PKCE, token,
scope, XOAUTH2, sync, and send behaviour are covered by unit tests and fake
services.

## Sync

Each account with an IMAP host gets one [worker](./apps.md#workers), named
`sync-<account>`, kicked at `account:<id>`, claiming only that account's jobs.
It pushes local changes every turn and pulls when the watch below says so, on
**sync**, or about once a minute anyway. A pass discovers special-use folders,
receives new mail, and reconciles flags and deletions. A folder is mirrored
**whole**: after the new mail lands, the pass compares the server's uid list
against what the store holds and reaches back for the missing ones 200 at a
time, newest first — grouped fetches and one commit a batch, over the session it
already holds. Nothing is dropped for being old; the batches only keep a whole
mailbox out of memory. A pass reaches back for at most twenty seconds, so this
account's own jobs are not left waiting behind a first sync, and comes back
five seconds later for the rest. A UIDVALIDITY reset re-ingests that folder
from scratch, the same way.

Sync fetches headers and `BODYSTRUCTURE`, followed by `BODY.PEEK[section]`
for reading text only. UIDs needing the same reading sections share one
command. Unusable messages are skipped for the current pass and retried
later, so older mail and other folders can keep syncing. File sections are
fetched on demand with `PEEK` too, which leaves the server's `\Seen` flag
alone. A session is kept between passes
and checked with a `NOOP` rather than signed in again, because providers count
logins.

Folder roles come from IMAP special-use attributes: inbox, archive, sent, spam,
and trash. Each of the five has a mailbox panel. Folders without one of these
roles are not mirrored.

`message` rows store the desired state. `server_msg` rows store the last state
seen on the server. A difference between them becomes a queued job: the folder
a mail sits in, whether it has been read, whether it has been passed on. Each
job checks again before it acts, so undo costs no server traffic. A sync pass
never keeps a database write transaction open during a network request, and it
writes the account's status only when the text changed, so a quiet pass does
not stale every cached query once a minute.

Server deletions remove local rows. For other differences, the user's desired
state wins and is sent to the server. Undo changes the desired state again, so
the next pass reverses a change with no compensation logic.

An account whose last sync failed is a [problem](./apps.md#problems) with two
controls: *sync*, and a link to settings.

## The watch

Beside each sync pass runs `watch-<account>`: a second session, sitting in
RFC 2177 `IDLE` on the inbox so the server can say that a letter arrived
instead of being asked once a minute. It fetches nothing — the sync pass holds
the session that may write, and two threads ingesting one mailbox would race
for the same uids — so a watch that hears something sets what that pass reads
and [wakes it](./apps.md#workers).

The second connection is what buys the first one's manners: a wait cannot be
cut short, and a pass that spent five minutes inside `IDLE` could not push a
mark the moment a verb made one. Five minutes is well under the 29 the RFC
allows, because that window is also how long a watch takes to notice that it
has been retired, that the machine woke with a dead socket, or that the
account is gone.

`IDLE` reports on the selected mailbox and no other, and the inbox is the one
whose latency anybody feels; the interval carries the rest. What ends a wait is
mail arriving or going, never a flag: a `STORE` this app just pushed comes back
on the watch's own connection, and a pull for each would be one per mark. A
server that offers no `IDLE` parks its watch for good — handing the session
back, because a connection nobody waits on is one the account cannot spend
elsewhere — and the minute is the cadence it had before.

A fake cannot block, so a wait that came back before its window was up is
treated as one that did not wait: the watch holds the remainder itself rather
than ask again on the next tick. That is what keeps a scripted run — where
every pass runs sixty times a second — from turning the log into a torrent.

## Sending

Drafts are saved as the user types, straight through the store rather than as
actions. A draft belongs to its compose **slot**: slot ids are stable and
persisted, so half-written text survives a restart, and the outbox row shares
that id, which means one pending send per compose.

Sending creates an outbox row with a default 10-second window and closes the
compose panel, both on one node, so one undo takes the letter back and the
panel with it. `SUPERAPP_SEND_DELAY` sets the window in seconds, which is what
a suite turns down to one.

At the deadline the sender worker claims the row and files a submit job. It
sends through SMTP, appends the message to Sent over IMAP when the provider
requires it, and stores the result. Replies and forwards carry the headers that
join the Sent copy to its conversation, and a forward's source is marked passed
on once the letter has left. Filing to Sent is best effort: a failure there is
reported on the row and never fails the send.

The submit job is the one deferred effect that is **not** safe to repeat, so a
crash mid-send fails with `interrupted; outcome unknown` and asks a human. A
delivered message cannot be undone; a send that never left still can. A failed
send is a problem with *retry*, which refiles it with a fresh window, and
*reopen*, which opens the draft again as a compose panel and takes the failed
row away. Because the outbox and the job are durable, a restart delays pending
work rather than losing it.

## HTML and pictures

The HTML cleanup, typography and image cache live in `app/src/reader/`,
shared with [RSS](./rss.md). Mail supplies the inline MIME file adapter.

A letter arrives as text, or as text and HTML. A reader draws the HTML when
there is one; a reply quotes the text.

Plain and HTML letters use the shared proportional prose face, IBM Plex Sans
at 15 px, with semibold emphasis and space between lines and paragraphs.
Headings stay compact: h1–h2 are semibold at 1.17× body size, and h3–h6 are
semibold at body size, separated by spacing. Code stays monospace, links are
grey and underlined, and quotes and dividers use light rules. The same prose
face is used when writing a letter. Stored HTML keeps its heading levels.

Outside HTML is narrowed at ingest into the limited markup the reader draws,
and the result is stored. Scripts, frames, hidden content, unsafe link schemes,
and unsupported styling are removed; `javascript:`, `data:`, and `cid:` hrefs
lose their link and keep their text. Sender font choices are discarded;
semantic code tags determine what reads as code. Compact data tables become a
grid. Labeled tables with four or more columns and longer descriptions read
as one paragraph per record, each value beside its column label; two- and
three-column tables can keep longer cells in their grid. Presentation tables
become lines. Input and output limits keep a large or hostile letter from
blocking layout: 4 MiB in, 100 KiB out, and the cut says so in the body rather
than truncating silently.
An image whose area is 25 square pixels or less is treated as a tracking pixel
and removed with its alternative text.

Because the narrowing is stored, its version is a derived schema step: raising
it re-narrows every letter with raw MIME on the next store open.

Images fit a 360 × 320 px preview box and the reading column without cropping
or enlarging small originals. The width limit gives wide banners a consistent
measure; the height limit keeps portrait artwork from filling the reader.

Images load off the drawing thread. An inline `cid:` part and a `data:` image
are read by a reader thread, a remote image is an ordinary HTTP request, and
Makepad's decode pool decodes what arrives. Each image reserves its final size
from its header, so nothing reflows when the bytes land, and a failure is
remembered rather than re-asked. Remote images are fetched with no prompt; the
tracking-pixel rule above is the only defence.

## Environment knobs

An app's own knobs are environment variables it reads itself; argv belongs to
the shell.

| Variable | Meaning |
|---|---|
| `SUPERAPP_SEND_DELAY` | the send window in seconds; 10 by default |
| `SUPERAPP_MAIL_DOWN=<reason>` | takes the fake servers offline with that reason, so a suite can watch a send fail |
| `SUPERAPP_GOOGLE_CLIENT_ID`, `SUPERAPP_GOOGLE_CLIENT_SECRET` | the Google desktop client, in place of `google-oauth.json` |
