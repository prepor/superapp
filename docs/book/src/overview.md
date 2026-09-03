# Overview

Superapp is a personal "user space OS": one application that absorbs as much of
daily computing as possible — email, messaging, RSS, calendar, knowledge base,
text editing, agent orchestration — without splitting it into apps and windows.

The bet: applications are the wrong unit. What a person actually works with is
*views of their own data* — a list of messages, one thread, a draft, a note.
Superapp makes that the primitive: every view is a **panel**, specialized on a
single function, and every panel lives on one scrolling tiled workspace. The
seams between "apps" disappear; a mail thread, the sender's contact card and a
reply draft are three panels standing next to each other, joined.

Because panels are small and single-purpose, they compose: the same [rich table](./richtable.md)
that lists mail will list feed items and calendar events; the same link grammar
navigates everything. Consistency is a hard requirement, not a style: from
looking at any element you must know what it does.

## What exists today

A native prototype (Rust + Makepad, macOS) with the full panel mechanics —
scrolling 12×6 workspace, joins with cascade-close, spring animation, the
link/button grammar, keyboard and mouse parity — exercised by fake mail panels:
`email/inbox`, `email/message`, `email/compose`, `contact`, plus `settings`,
`problems`, `help`, `about`, and `effects` with the `job` it previews into —
the queue of everything the app has tried on the outside world, read back as
a list panel and its detail. Mail talks to a real IMAP account; the **file
browser** (`files` and the `file` card) reads the real disk, which is the
first thing here that is not the app's own data at all — and the two meet
at [attachments](./interaction-grammar.md#attachments-a-part-of-a-letter-is-a-card),
where a part of a letter draws on the browser's own card and a file held in
the browser becomes what a draft carries out.

The first, throwaway web prototype that validated the interaction model is kept
under `web/`.
