# Architecture

The application has two main parts:

- a core that decides what the workspace contains and where panels belong;
- a Makepad shell that draws the result, handles input, and animates changes.

Most modules do not depend on Makepad and can be tested without opening a
window.

| Module | Purpose | Uses Makepad |
|---|---|---|
| `core.rs` | Panels, columns, joins, workspaces, and target layout | No |
| `store.rs` | SQLite storage and cached queries | No |
| `effect.rs` | Work outside the database, queued jobs, and the Effects source | No |
| `history.rs` | Undo and redo history | No |
| `filter.rs` | Filter parsing and completion context | No |
| `richtable.rs` | Table data sources, SQL building, pages, and marks | No |
| `mail.rs` | Mail queries, data, actions, and attachments | No |
| `files.rs` | File listings, file cards, path completion, and file actions | No |
| `html.rs` | Safe, limited HTML used by message bodies | No |
| `sync.rs` | Receiving and updating mail through IMAP | No |
| `send.rs` | Drafts, the outbox, SMTP, and the undo delay | No |
| `repl.rs` | Device-sync log, lease, and sync passes | No |
| `object.rs` | In-memory and HTTP object storage used by device sync | No |
| `r2.rs` | Signed requests to an R2 bucket | No |
| `secret.rs` | Keychain or private-file storage for passwords and keys | No |
| `oauth.rs` | Gmail browser sign-in and token handling | No |
| `launcher.rs` | Launcher search | No |
| `problems.rs` | Current sync, send, and device-sync problems | No |
| `spring.rs` | Animation calculations | No |
| `ui.rs` | Shared content types such as lines, fields, and buttons | No |
| `theme.rs` | Sizes and colours | No |
| `e2e.rs` | End-to-end script parser and runner | No |
| `scene.rs` | Panel-library scenes and their layout | No |
| `mac.rs` | macOS screen, window, and screenshot helpers | No |
| `panels.rs` | Panel widgets | Yes |
| `app.rs` | The Makepad shell | Yes |
| `catalog.rs` | Scenes shown in the Panels Library | Yes |
| `library.rs` | The Panels Library canvas | Yes |

## World

`World` holds the database, access to the operating system and network, and the
effect registry. It is passed into the code instead of stored globally. The UI
thread and each worker have their own handle. Tests can replace the real world
with an isolated in-memory one. See [Data and Effects](./data-substrate.md).

## Stages and mounts

`Stage` is the main workspace widget. It owns the workspace, animation, hit
areas, and end-to-end runner. A `Boot` value supplies its database, grid,
send-delay setting, clock, and access to the outside world.

The application window has one stage. The Panels Library creates smaller
stages, called mounts, for its examples. A mount can show one panel or a whole
workspace. It draws into its own surface and receives only its own events.
Simple component examples mount a widget without a stage. See
[Developer Experience](./dev-x.md#panels-library).

## Frame loop

Input changes the workspace model. The model then produces target panel and
camera positions. Animation moves the current positions toward those targets
and requests frames only while something is moving. Trackpad movement updates
the camera directly.

The model is read again after a change, not on every animation frame. This lets
the application stop drawing while idle.

## Drawing and input

- The shell measures the monospace font once for each display scale. Panel
  content uses this character grid, while scrolling remains pixel-smooth.
- Drawing records a rectangle and action for each control. A click uses the
  topmost matching rectangle from the latest frame.
- Focused fields use the system input method. Text arrives as text input;
  shortcuts arrive as key events.
- `State` holds the workspace and UI state for a stage. Drawing temporarily
  takes this state out of the widget so both can be borrowed safely.

## Keep expensive work out of drawing

Drawing runs on the UI thread and may run many times during an animation. It
must not perform file or network access, parse large messages, or decode large
images. Database reads are allowed through `Store::rows` because results are
cached and invalidated only when their source tables change.

Longer work runs on a worker. The panel keeps a stable placeholder, then
updates and redraws when the result arrives. Message images follow this rule:
a reader thread loads mail parts, Makepad's decode pool decodes them, and the
image header provides enough information to reserve their final size.

Some Makepad work, such as turning SVG into drawing commands, must stay on the
UI thread. Such input is limited instead. Inline SVG is capped by
`MAX_INLINE_SVG` at 64 KiB; larger images use their alternative text.
