# Mail

The mail app is four mailboxes over one list, conversations, drafts, contacts,
and accounts, with real IMAP and SMTP behind them. It registers ten panel
kinds, its own schema ladder, a demo seed, four deferred effects, three
capabilities of its own, a search source, two problem sources, and one worker
per account plus the sender.

A window's own run reaches real servers. Every scripted run, every test, and
every panels-library mount gets a fake set of servers instead, which registers
itself under all three capability traits and under its own type, so a test can
plant a letter or take the servers offline. The demo account a fresh store is
seeded with carries the fake servers' hosts only in those runs; in a real one
it has the same letters and no hosts, so no sync worker runs for a mailbox
that is not out there.

## Tags and roots

| Tag | Argument | What it shows |
|---|---|---|
| `inbox`, `archive`, `sent`, `spam` | none, or a sender to filter by | one mailbox |
| `message` | a mail id | one conversation |
| `compose` | none, or `reply`/`forward`/`reopen` and a mail id | one draft |
| `contact` | an address | one correspondent |
| `attachment` | a mail id and a part index | one part of a letter, as a card |
| `settings` | none | the accounts |
| `add_account` | none | the add-account form |

The roots the launcher offers, in this order: **inbox**, **archive**, **sent**,
**spam**, **new mail**, **settings**. Mail is listed first among the apps, so a
store nobody has booted comes up on the inbox.

## Four mailboxes, one list

Inbox, archive, sent, and spam are four tags over one panel kind, one row
shape, and one query written out four times so the folder role is a SQL
literal. They share rows, filtering, marks, cursor movement, and message
previews.

What differs is one verb: only the inbox offers **archive**, because everywhere
else the mail is already out of it. **delete** is the move every mailbox has,
and it means moving to the account's trash folder. Trash is a role a folder
plays, not a mailbox panel; nothing lists it.

The filter tags are `@unread`, `@html`, `@from:`, `@subject:`, `@date`, and
`@account:`. Free text matches the sender's name, the sender's address, and the
subject. `@from:` suggestions come from the current mailbox: the three ordinary
mailboxes offer correspondents with spam left out, and spam offers its own
senders, because a spammer is not a correspondent.

A mailbox panel may carry a sender as its argument, which is what a contact's
link opens: the panel is filtered from its first draw, and the field shows
`@from:vera@kovac.io` so the next edit is the person's.

The bar is `sync` (`cmd+s`), and while rows are marked, `archive n` (`cmd+a`,
inbox only), `delete n` (`cmd+d`), `mark all` (`cmd+m`), and `clear`. *mark
all* wears `m` rather than `l` because the shell keeps `cmd+l`; *clear* wears
no letter because `esc` is the table's.

## Threads: the row is the conversation, the panel is the whole of it

Each mailbox row is a conversation with at least one message in that folder. It
shows the participants newest speaker first with the account's own address as
*me*, the message count, the subject of the oldest message with `Re:` and its
translations stripped, and the date of the latest message in that folder. It is
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
The account's address is at the top. Each message is one row that folds open in
place: closed, it shows the sender, the first content line or error, and the
date. Quoted text is folded behind the reading rules, because in a conversation
the quote is the message above. These open states are the instance's own
context and are not part of undo history; after a restart only the message the
panel names starts open.

Opening a conversation marks its unread messages as read. That write is claimed
by the open, so it lands on the same undoable node as the panel appearing and
one undo gives both back. A cursor walk that previews a row at a time coalesces
into one node, so one undo closes the whole walk.

The reader's bar is `archive` (`cmd+a`), `delete` (`cmd+d`), `reply` (`cmd+r`),
and `forward` (`cmd+f`). The first two are buttons; reply and forward are
links, so they follow the [solid-link rule](./interaction-grammar.md#the-three-interactive-signals)
and open a draft joined to the reader. Filing closes the reader's own slot and
nothing else: another panel reading the same conversation stays where it is and
says what it shows. The list driving that reader then walks on to the row that
took its place, and that walk is part of the filing rather than a second
action — one undo puts the mail back, reopens the reader on it, and takes the
walk's own read mark back with it.

A reply fills the recipient and subject, quotes the source message, and sends
`In-Reply-To` and `References` headers. A forward starts with an empty
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
size, the letter it came with, and a preview of text or a PNG or JPEG. A part
has no disk path, so its one verb is `open` (`cmd+o`), which writes the part to
a per-part directory under the system temporary directory, keeping the sender's
filename, and asks the operating system to open it.

The bytes are never stored twice. A letter's raw MIME already holds every part;
an `attachment` row is the description a list and a card need, plus the part
index the bytes are read back by. Those rows are derived and versioned by the
walk that made them, and the version is recorded per letter, so a letter that
arrives through device sync gets its parts scanned by the sender worker. The
bytes themselves are read on a worker, not while drawing.

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

The launcher's mail source answers with the people who wrote first and then the
letters a query's words reach, best match first, out of an FTS5 index. Spam is
left out of the sender side, so nothing a launcher or a compose field offers
came out of the junk.

## Accounts

Settings is mail's own panel, not the shell's: what a person configures belongs
to the app it configures. It lists one row per account with the address, the
host, and the last pass's status, all three selectable so a sync error can be
carried somewhere else, plus a *remove* button per row. Its one bar verb is
*add account* (`cmd+d`, because `a` is archive and `s` is sync everywhere
else).

What an account stores is a label, an address, an IMAP host, an SMTP host, and
one word saying how it authenticates, plus the status and time the sync pass
writes. The secret is never in the store: an app password lives in the keychain
under the address and a Google refresh token under a key of its own, while an
access token is never written down at all.

Removing an account cannot be undone, and history says so rather than
half-restoring it: putting the panel back cannot put its mail back.

The add-account form has four fields, with the two host fields prefilled,
because a form with two empty host fields is a quiz. It refuses a blank address
and an address already present, files the password to the keychain, and adds
the row.

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

The app checks that the granted scopes include full mail access before creating
the account, and reads Google's XOAUTH2 error response so it can tell a missing
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
It pushes local changes every turn and pulls about once a minute or on
**sync**. A pass discovers special-use folders, receives new mail, and
reconciles flags and deletions. Each folder keeps its newest 200 messages, and
a UIDVALIDITY reset re-ingests that folder from scratch.

Folder roles come from IMAP special-use attributes: inbox, archive, sent, spam,
and trash. Only the first four have mailbox panels. Folders without one of
these roles are not mirrored.

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

A letter arrives as text, or as text and HTML. A reader draws the HTML when
there is one; a reply quotes the text.

Outside HTML is narrowed at ingest into the limited markup the reader draws,
and the result is stored. Scripts, frames, hidden content, unsafe link schemes,
and unsupported styling are removed; `javascript:`, `data:`, and `cid:` hrefs
lose their link and keep their text. Tables become a small grid. Input and
output limits keep a large or hostile letter from blocking layout: 4 MiB in,
100 KiB out, and the cut says so in the body rather than truncating silently.
An image whose area is 25 square pixels or less is treated as a tracking pixel
and removed with its alternative text.

Because the narrowing is stored, its version is a derived schema step: raising
it re-narrows every letter with raw MIME on the next store open.

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
