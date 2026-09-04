# Architecture

Three layers, in two crates.

- The **kernel** is everything generic that does not draw: the panel model and
  navigation, the store and its cached queries, effects and the queue, undo
  history, the filter and the rich table's state, the search sources and the
  switcher's list, problems, springs, the e2e grammar, and the interfaces an
  app implements. It never names Makepad, so it is what `cargo test` runs
  without a window.
- The **shell** is everything generic that draws or takes input: the stage, the
  chrome, the bar, animation, the overlays, the shared widgets, the panels
  library, and the hosting of panel widgets. It uses Makepad and depends on the
  kernel.
- The **apps** implement the kernel's interfaces and supply their widgets to
  the shell.

Dependencies run one way: apps on the shell and the kernel, the shell on the
kernel, the kernel on nothing. The kernel and the shell never name an app.
That is the whole rule; there is no rule between apps, which reach each other
through the registry and work when the answer is `None`.

[Device sync](./device-sync.md) is not an app. It replicates the store itself,
every app's tables included, and the shell depends on it: the write gate in
`Session::act`, the locked screen, and the lease driver. The test for an app is
not "does it have a panel and a worker" but "does the shell work without it".

## The kernel: `kernel/src/`

| Module | Purpose |
|---|---|
| `panel.rs` | `Tag`, `PanelId`, `PanelKind`, `Panel`, `Verb`, and the `Missing` panel |
| `app.rs` | `App`, the `Apps` registry, `Root`, `Schema`, `Capabilities`, `Env`, `Mode`, `Worker`, `Workers` |
| `session.rs` | `Session` and `Action`: the one surface a verb, an instance, or a widget acts on; `session/repl_mount.rs` mounts the lease driver |
| `nav.rs` | `Nav` and its application to the layout, with the history kind and coalescing |
| `layout.rs` | Slots, columns, joins, workspaces, and the target scene |
| `store.rs` | SQLite, the one writer, cached queries and their dependencies; `store/repl.rs` is the replication half |
| `effect.rs` | Effects, the queue, the in-memory ring, and `World` |
| `caps/` | The capabilities the kernel owns, the demo disk, and what a file is |
| `repl/` | Device sync: the log, the lease and its passes, the object store, and R2 |
| `history.rs` | The undo and redo tree |
| `richtable.rs` | Table sources, SQL building, pages, cursors, and marks |
| `filter.rs` | Filter parsing and completion context |
| `search.rs` | Search providers, and one question put to all of them |
| `launcher.rs` | Open panels and roots as one switcher list |
| `problems.rs` | `ProblemSource` and `Problem` |
| `scene.rs` | The panels library's scene graph and its layout |
| `e2e.rs` | The end-to-end script grammar and its parser |
| `spring.rs` | Spring animation |
| `theme.rs` | Colours, spacing, and type sizes |
| `time.rs` | Civil dates and the two spellings the panels use |

## The shell: `app/src/shell/`

| Module | Purpose |
|---|---|
| `mod.rs` | The app list handed in at boot, and the shell's own script block |
| `boot.rs` | Argv, the world, and the session a stage comes up on |
| `stage.rs` | The widget that owns the session and draws it, and the frame loop |
| `draw.rs` | The chrome: the strip of workspaces, the panels on it, and the sheet over them |
| `hosted.rs` | One widget per slot, instantiated from its tag's template |
| `bar.rs` | The bar at a panel's foot: its geometry, its chords, and its labels |
| `keys.rs` | The reserved chords, and the routing order after them |
| `pointer.rs` | What is under the pointer, and what a press on it means |
| `hits.rs` | The labelled rectangles a frame drew |
| `anim.rs` | Springs towards the scene's targets |
| `overlays.rs` | The launcher, the workspaces list, and the history tree |
| `lock.rs` | The locked screen a device that may not write shows |
| `menu.rs` | The macOS menu bar |
| `context.rs` | `cmd+i`: the focused panel's context, to the clipboard and to a file |
| `dsl.rs` | The theme and the base widgets every panel is built from |
| `widgets/` | The shared components a panel embeds: the rich table and the file card |
| `app_ui.rs` | `AppUi`: what an app adds to the screen |
| `catalog.rs` | What a panels-library node comes up as, and the shell's own scenes |
| `library/` | The zoomable canvas of live scenes |
| `e2e.rs` | The bridge between a script and the shell's own input paths |
| `system/` | The shell's own app: help, about, the effect log, one job, problems, the device-sync form, and the missing card |

## The apps: `app/src/`

| Module | Purpose |
|---|---|
| `lib.rs` | The two app lists, `APPS` and `UIS`. The only place in the build that names an app |
| `main.rs` | The desktop binary: one line |
| `root.rs` | The window, and the app root that hangs every app's templates on the stage |
| `apps/mail/` | [Mail](./mail.md): ten panel kinds, a schema, a seed, four deferred effects, three capabilities, a search source, two problem sources, and its workers |
| `apps/files/` | [Files](./files.md): two panel kinds, one root, and a clipboard other apps may read |
| `platform/` | What this machine gives the shell that Makepad does not: the disk and the watch over it, the keychain, the trash, and a window-layer screenshot |
| `bin/` | `bucketd`, `sync-demo`, and `reseed-edit`, the programs the device-sync walks are driven with |

`app/src/platform/` is below the shell rather than beside it, and the same rule
holds: it names no app.

## The rules

1. Code under `kernel/` names no Makepad type and no app.
2. Code under `app/src/shell/` and `app/src/platform/` names no app.
3. Apps reach each other only through `Apps::get` and `Apps::get_as`, and work
   when the answer is `None`.
4. A tag, a verb id, an effect kind, and a table name never change once written
   to a store.
5. Every deferred effect says whether it writes. Every bar has no reserved
   chord and no duplicate letter.
6. An app's e2e suites live under `e2e/<app>/` and name only labels its own
   panels draw, plus shell chrome.
7. Tests in CI enforce rules 1 and 2 by reading the source.

`app/src/shell/system/` is the one directory inside the shell that is an app, on
purpose: the shell uses its own extension points rather than a private door,
and the boundary test skips it and the list itself.

## The contract, in short

The binary lists the apps once. The kernel builds one registry from that list
and asks the list for everything; it never asks an app by name.

An app registers panel kinds by tag, a schema ladder, a demo seed, deferred
effect kinds, capabilities per mode, search providers, problem sources, the
workers it wants running, and its roots. Its Makepad half registers a template
per tag, its own script block, and its panels-library scenes.

A `PanelKind` opens instances and nothing else. A `Panel` instance owns its own
state, answers with a title, a size wish, and a bar of verbs, and runs a verb
by its id. `Session` is the one object a verb, an instance, or a widget acts
on; `act` is one undoable action and `nav` is where a click goes. Neither
touches an instance: `settle` places and drops them after the event, which is
what lets a verb close its own slot.

[Apps](./apps.md) is the whole of it, method by method.

## The world

`World` holds the store, the capabilities one thread may reach the outside
through, and the registry that decodes a filed payload back into an effect. It
is passed into the code instead of stored globally. The UI thread and each
worker have their own, so a worker's effects live in that worker's world and
nowhere else. A test replaces the real world with an isolated in-memory one.
See [Data and Effects](./data-substrate.md).

## Stages and mounts

`Stage` is the main workspace widget. It owns the session, animation, hit
areas, and the end-to-end runner. A `Boot` value supplies its store, its grid,
its clock, and the mode its world is built in.

The application window has one stage. The panels library creates smaller
stages, called mounts, for its examples. A mount can show one panel or a whole
workspace, draws into its own render pass, and receives only its own events.
Simple component examples mount a widget with no stage at all. See
[Developer Experience](./dev-x.md#the-panels-library).

## Frame loop

Input changes the session. The session produces target panel and camera
positions. Animation moves the current positions toward those targets and asks
for frames only while something is moving. Trackpad movement updates the camera
directly.

After every event the shell reads the session's dirty flags: a layout change
recomputes the scene and retargets the springs, and anything else redraws. The
model is read again after a change, not on every animation frame, which is what
lets the application stop drawing while idle.

## Drawing and input

- The shell measures the monospace font once for each display scale. Panel
  content uses this character grid, while scrolling stays pixel-smooth. The
  column's width in characters is what a panel's size wish is given.
- A panel's body is a retained widget tree, built from the template its app
  registered and kept across draws. A slot that shows something else gets a new
  one; a slot that only moved keeps its own.
- A widget reaches the session and its own instance through Makepad's scope:
  `&mut Session` during events, shared during draws, beside the slot, the
  instance, and the hit collector.
- Drawing records a labelled rectangle for each control. A press resolves to
  the last one registered that contains the point, so a control drawn over
  another takes the click. A hosted widget's own rectangles are its to answer;
  the shell only routes the pointer there.
- Focused fields use the system input method. Text arrives as text input;
  chords arrive as key events and are routed in the order
  [Interaction Grammar](./interaction-grammar.md#accelerators-and-the-bar)
  describes.

## Keep expensive work out of drawing

Drawing runs on the UI thread and may run many times during an animation. It
must not perform file or network access, parse large messages, or decode large
images. Database reads are allowed through `Store::rows` because results are
cached and invalidated only when their source tables change.

Longer work runs on a worker. The panel keeps a stable placeholder, then
updates and redraws when the result arrives. Message images follow this rule: a
reader thread loads mail parts, Makepad's decode pool decodes them, and the
image header provides enough information to reserve their final size.

A panel that measures something for its size wish takes the measure once and
remembers it on the instance, because the wish is asked for on every relayout.

Some Makepad work, such as turning SVG into drawing commands, must stay on the
UI thread. Such input is limited instead: inline SVG is capped at 64 KiB, and
larger images use their alternative text.
