# CR-002 · Retained panels: the semantic widget library

Status: **accepted direction** (Andrey, 2026-09-01: the char-grid
immediate-mode content model is the wrong long game; panel content should be
built from high-level, composable, retained widgets — ours, on makepad's
widget architecture, styled to the design language — with
[Robrix](https://github.com/project-robius/robrix) as the working proof).

## Why

The char grid bought speed: one renderer, trivial hit labels, a working mail
client in days. Its costs arrived with real use: every field behaviour is
hand-rolled (no selection, no click-to-caret, tab arrived as a patch), hit
rects occlude each other by construction order (the dead send button), forms
are line assembly with fixed columns, and none of it composes. Each fix
re-implements a solved widget-toolkit problem, badly.

## The evidence

Robrix — a production Matrix client — is on **the same makepad generation as
our pin** (`script_mod!`, `:=` named instances, `register_widget(vm)`), so
its patterns transfer literally:

- `src/shared/`: ~30 semantic components (`RobrixTextInput`,
  `RobrixIconButton` + variants, avatars, modals, badges) — base widgets
  wrapped once, extended via DSL overrides (`draw_bg +: { … }`), styled from
  one `styles.rs` of constants.
- Screens compose in DSL, Rust reaches widgets by `ids!()` paths; per-widget
  state lives on the widget struct (`#[rust]` fields).
- `PortalList` virtualizes 5000-item timelines: item *templates* in DSL,
  instances created per visible row, data passed per-draw via
  `Scope::with_props`.
- App data flows down through `Scope`; widget actions bubble up via
  `cx.widget_action` to a top-level `handle_actions`.
- Async → UI: queue updates, `SignalToUI`, redraw — exactly our worker model.

## The architecture

**Keep** (unchanged):

- the workspace layer: `core::Wm`, springs, camera, panel chrome (headers,
  borders, joins, drags), workspaces/slide;
- the substrate: store, reactive queries, actions/undo DAG, sync/send
  workers. The store rides `Scope` into every widget draw — Robrix's
  AppState pattern with a real database behind it. Widget actions land in
  `State::act`, so **undo semantics are untouched**;
- the launcher/history/ws overlays (migrate last, if at all).

**Change**: panel *content*. Each `Kind` becomes a retained widget
(`SettingsPanel`, `ComposePanel`, `InboxPanel`, …) composed from **our
semantic library** in `src/ui/`:

- `ui/styles.rs` — the design language as DSL constants: mono font, INK/BG,
  the underline grammar, paddings;
- `SField` — makepad `TextInput` wrapped once, themed monochrome: real
  caret, selection, click-to-position, IME (android soft keyboard) — all
  inherited, not re-implemented; the tab/enter walk on top;
- `SButton` (bordered side-effect button), `SLink` (solid/dotted underline
  semantics), `SSection`, `SRow`, `STable` as needs arrive.

The shell hosts content widgets inside the existing panel chrome: one
`WidgetRef` per open panel (template per kind — the PortalList pattern at
panel granularity), drawn inside the body turtle, data via `Scope`
(store + params), actions bubbled to the shell.

**Retires** (at the end): `build_lines`/`draw_line`/`draw_seg`, the
hand-rolled `TextField` + custom IME plumbing for fields, manual hit rects
for content.

## Costs, named honestly

- **Panel fade**: content widgets don't alpha-fade as a subtree; panel
  enter/exit becomes chrome-fade over popping/clipped content. The pilot
  judges whether that reads acceptably or needs a clip-reveal.
- **e2e bridge**: scripts address labels; retained widgets need a
  label→widget-path resolver (walk the tree, match text/ids). Part of the
  pilot, not an afterthought — the suites must stay green through the
  migration.
- **Two renderers during the strangler window**: migrated kinds are widget
  trees, the rest stay on the char grid until their phase. Deliberate.

## Phases

A. **Foundation**: `src/ui/` DSL styles + `SField`/`SButton`/`SSection`;
   the panel-host plumbing (templates, per-pid instances, Scope data,
   action bubbling); the e2e label bridge.
B. **Settings** (pilot — forms hurt most): real field behaviour, the
   tab/enter walk on widget focus, add/remove accounts through bubbled
   actions.
C. **Compose**: multiline `TextInput`, drafts/send unchanged underneath.
D. **Inbox** on `PortalList`: virtualized (821+ real messages already),
   row templates, selection/enter/j-k preserved.
E. **Message / Contact / Help / About**; retire the char-grid renderer and
   the custom field machinery.
F. **Overlays** onto the same library (launcher field becomes an `SField`,
   history rows become widgets) — optional, by pain.

Each phase lands green (unit + all e2e suites), book updated, committed.
