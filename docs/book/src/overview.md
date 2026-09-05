# Overview

Superapp is my personal project, built for one user: me. It puts my everyday
work in one application instead of many separate apps and windows. The
long-term scope includes mail, messages, feeds, calendars, notes, editing, and
agent tools.

The main unit is a **panel**: a focused view such as a mail list, a thread, a
draft, or a contact. Panels share one scrolling tiled workspace. Related panels
can stay next to each other, so a mail thread, its sender, and a reply can form
one working group.

Panels reuse the same controls and navigation rules. The
[rich table](./richtable.md) that draws a mailbox draws a directory and the
effect log too, and the same file card draws a file on disk and a part of a
letter. The same kind of control behaves the same everywhere.

## Three layers

- a **kernel** that does not draw: the panel model and navigation, the store,
  effects, undo history, and the interfaces an app implements;
- a **shell** that draws and takes input: the stage, the chrome, the bar, the
  overlays, and the shared widgets;
- the **apps** on top of both.

The kernel and the shell never name an app. [Architecture](./architecture.md)
describes the layers and [Apps](./apps.md) is the contract between them.

## The apps

- [Mail](./mail.md): four mailboxes over one list, conversations,
  attachments, drafts, contacts, accounts, and real IMAP and SMTP.
- [Files](./files.md): a directory as a list, a file as a card, and the disk
  operations that act on both.
- [Agents](./agents.md): a chat over the store, with the apps as its hands —
  a panel as context, tools that are the verbs' own code paths, and `cmd+z`
  over everything the agent does.
- `system`: the shell's own app. Help, about, the effect log and one job, the
  problems list, the device-sync form, and the card a panel gets when no app
  in this build owns its tag. It is listed like any other app, so the shell
  uses its own extension points.

[Device sync](./device-sync.md) is not an app: it replicates the store itself,
every app's tables included, and the shell depends on it.

## What exists today

The current native prototype is written in Rust with Makepad and runs on
macOS. It includes nine scrolling workspaces with tiled panels, joins, tabs,
animation, and keyboard and mouse controls; the four apps above; a
single-writer device sync over a leased bucket; and a panels library that
shows every scene of the catalogue on a zoomable canvas.
