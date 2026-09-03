# Overview

Superapp is my personal project, built for one user: me. It puts my everyday
work in one application instead of many separate apps and windows. The
long-term scope includes mail, messages, feeds, calendars, notes, editing, and
agent tools.

The main unit is a **panel**: a focused view such as a mail list, a thread, a
draft, or a contact. Panels share one scrolling tiled workspace. Related panels
can stay next to each other, so a mail thread, its sender, and a reply can form
one working group.

Panels reuse the same controls and navigation rules. For example, the
[rich table](./richtable.md) used for mail can also support future feeds and
calendar events. The same kind of control should behave the same everywhere.

## What exists today

The current native prototype is written in Rust with Makepad. It runs on macOS
and also has an Android build. It includes:

- nine scrolling workspaces with tiled panels, joins, tabs, animation, and
  keyboard, mouse, and touch controls;
- mailboxes for inbox, archive, sent, and spam; conversation, compose, and
  contact panels; and real IMAP and SMTP support;
- settings, problems, help, and about panels;
- an Effects panel that combines queued jobs with recent in-memory effects,
  plus a panel for one queued job;
- a file browser that reads and writes the real disk; and
- [attachments](./interaction-grammar.md#attachments-a-part-of-a-letter-is-a-card),
  which use the same file card for mail parts and files on disk.
