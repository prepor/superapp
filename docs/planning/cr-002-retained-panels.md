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

A. **Foundation** — **landed 2026-09-01**: `src/panels.rs` holds the DSL
   styles (`SMonoStyle`, `SLabel`, `SSection`, `SField` over TextInputFlat,
   `SBtn` over ButtonFlat, all themed monochrome) and the host plumbing:
   Stage collects its named DSL children as templates (PortalList-style
   `on_after_apply`), instantiates one widget per panel id, forwards every
   event with `PanelProps` (Rc<Store> + pid — Scope props ride an `Any`) on
   the scope, and catches bubbled `PanelAction`s into `State::act`. The e2e
   bridge: fields register pointer hits (synthesized mouse events — real
   TextInput focus/typing); buttons register *semantic* ops resolved to the
   same PanelAction a real click emits (synthetic pointer capture pairing
   diverges from the platform's inside PortalList's capture-overload dance;
   not worth chasing). In-block DSL self-references must be qualified
   (`mod.widgets.X`) — bare names only resolve across blocks.
B. **Settings pilot** — **landed 2026-09-01**: the panel is a widget tree
   (sections, PortalList account rows with live status + remove, the add
   form); real TextInput behaviour throughout (click-to-caret, selection,
   IME); tab/shift+tab and the enter chain walk widget key focus with
   select-all on advance (typing replaces); submit past the last field.
   PortalList needs a fixed height (Fit collapses it). The char-grid
   settings path is retired. Chrome fades over popping content — reads
   fine in practice.
C. **Compose** — **landed 2026-09-01**: to/subject `SField`s over a
   multiline body; prefill (draft-or-reply) at instantiation with focus
   deferred to the next event tick (key focus set during a draw does not
   take); typing bubbles `DraftEdited` actions the shell persists; send
   and discard (chrome buttons) read the widget's values; the char-grid
   auto-field stands down for hosted kinds.
D. **Inbox** on `PortalList` — **landed 2026-09-01**: virtualized rows
   (from/subject/date, bold-unread via label pairs, selection wash),
   filter as an `SField` (enter selects first and rests, `/` focuses,
   change clears selection), the whole letter grammar (`j`/`k`, enter,
   modifiers for fresh opens) handled by the widget from forwarded
   TextInput/KeyDown events. Tapping a subject opens (bubbled
   `OpenMail` → the same undoable `Act::Open` path, mark-read included);
   elsewhere selects. e2e rows register by subject, so every existing
   script keeps working unchanged.
E. **Message / Contact** — **landed 2026-09-01**: `SLink` completes the
   design language in widget form (label over a 1 px underline; the
   dotted variant's dashes are shader-drawn); links carry their target
   `Kind` and bubble `FollowLink` into the same undoable Open/Replace
   paths, workspace modifier included. The message panel walks j/k/r
   from forwarded letters; neighbours grey out at the ends. `PanelProps`
   now carries the panel's `Kind`, so read panels self-derive entirely
   from scope. **Help / About stay on the char grid deliberately** —
   static text where the grid is harmless; they migrate with F when the
   overlays go, and only then does the old renderer retire.
F. **Overlays** onto the same library (launcher field becomes an `SField`,
   history rows become widgets) — optional, by pain.

Each phase lands green (unit + all e2e suites), book updated, committed.

## Post-E: real input vs the harness (2026-09-01)

The first real-mouse/keyboard session surfaced four defects the suites could
not see — all in the seam between the char-grid-era input model and hosted
widgets, none inside the widgets:

- `MouseDown` claimed key focus for the stage on every click (the char
  grid's need), stealing it back from the TextInput the forwarded event had
  just focused — every field dead to a real mouse. Skipped now when the
  click lands on a hosted field rect.
- macOS delivers letters as `TextInput` events only while the IME is shown,
  and `kick()` stood the IME down for hosted panels — the letter grammar
  (j/k, `/`, r) never fired from a real keyboard. The IME now stays on
  whenever a panel has focus; and since a blurring TextInput hides it (its
  own lifecycle), a `KeyFocus` watcher re-shows it when key focus returns
  to the shell.
- The e2e bridge's semantic rects also resolved under real clicks — a real
  row/button click fired twice (widget path + shell resolve). Real clicks
  now take only panel focus from those rects; semantic resolve is the
  scripts' door alone.
- Key/text events were forwarded to every hosted widget — a "j" typed into
  compose walked other inboxes. They go to the focused panel only now.

Why the suites stayed green through all four: the harness enters below the
platform seam — `handle_text` synthesizes `TextInput` events without any
IME, and script clicks either synthesize pointers straight at widgets or
resolve semantic ops directly. Both doors end in the same widget code
(which is the bridge's point), so the layer above them — IME lifecycle,
key-focus ownership, real-click resolution — is exactly what scripts cannot
exercise. That layer is verified store-level now: drive letters through a
temp db and assert the panels they produce (j·j·enter must open the second
row, not the first), which proves the path without pixels.

A fifth followed the fourth, and settled the phase-A mystery: PortalList
items are rebuilt every draw, so **their areas go stale the moment a
mid-gesture redraw lands** — and a panel-focus click triggers exactly such
a redraw between down and up. A down/up pair inside a list item can never
be trusted (this, not capture-overload exotica, is why synthetic button
clicks died in phase A). The rule that falls out: **in-list controls
(inbox rows, account remove buttons) resolve real clicks through the
shell's registered rects — the same semantic door the scripts use, one
door total; standalone widgets (the add button, links, fields) keep their
native pointer paths, and their registered rects stay e2e-only.** Rows
register two rects: the whole row selects, the subject band (later in the
list, and hit_at searches back-to-front) opens — so the mouse keeps the
open-vs-select split the widget's own hit test used to judge.

And a sixth, the deepest: **everything was working invisibly.** Plain
stock-shader quads inside PortalList items merge (per-shader draw-call
merging) into a call that paints *under* the panel background — the
selection wash and row rules never rendered, while text, buttons and
fields (distinct shaders, own calls) drew fine. So selection moved,
clicks selected, arrows walked — with no pixels to show it, which reads
as "nothing works". The fix is a trivial custom `pixel: fn()` on such
quads (distinct shader ⇒ own, correctly-ordered call); relatedly, a Fill
walk inside `flow: Overlay` defers forever, so the wash is a twin line
with its own bg, toggled like the bold pairs. The TextInput selection
quad paints *over* its glyphs with a state mix that doesn't engage — one
translucent ink for every selection state, plus collapse-on-blur, gives
the frameworks' behaviour. Testing doctrine from the same episode:
occluded windows skip presents, so background e2e screenshots are stale
frames (byte-identical shots are the tell) — visual claims need
`--front`; state claims stay store-level; and `e2e/cgpost.c` posts real
HID events at a scratch instance for the platform seam that synthesis
cannot reach (idle machine only — real events land wherever the front
window is).

Also in this pass, visual parity: the library draws at the theme's sizes
(10.5/8.25 — it had hardcoded 8.0, which read as a different font entirely),
subjects hold to one line (`max_lines: 1` + `Ellipsis`), unread bold is the
char grid's nudged double-draw reborn as `SBold` (Menlo ships no `wght`
axis, and makepad's `weight` is variable-font-only), inbox arrows mirror
j/k with scroll-follow, the message body scrolls, and settings lost its
fixed-height list (accounts fill the middle, the form holds the bottom).
Tab between fields stays ours: neither makepad's TextInput nor Robrix
handles Tab at all — the enter chain is Robrix's ceiling; the Tab walk is
ours on top of it. It grew into a proper ring (Andrey's ask): buttons are
stops too (settings: remove rows → fields → add), the ring wraps, a panel
holding focus starts the ring at its first stop (compose: Tab lands on
"to"), Enter/Space press a focused button (makepad buttons take key focus
but ship no keyboard activation), a keyboard-focused button wears the
selection wash, and — the frameworks' norm — a field's selection paints
only while it is focused, so tab-out lets the highlight go. Arrows also
scroll the message body again (three lines, the char grid's behaviour),
synthesized as Scroll events so the ScrollBars keep the clamping.

## The makepad patch (2026-09-01)

The Fold's soft keyboard forced the one thing this project had avoided:
carrying patches. Three app-side attempts failed against the same wall —
`TextInput::handle_focus_lost` hides the IME **unconditionally** on blur,
so every field-to-field move (the keyboard's "next", our Tab ring) closed
the keyboard and reopened it. Re-showing in the same op flush only turned
a 1.1 s close-wait-reopen into a visible dip: Samsung's insets controller
honours the hide first no matter how tightly the show follows. The hide
has to not happen, and only upstream can decide that.

`~/code/makepad-superapp` (branch `superapp-pin`) is a clone of the exact
pin carrying two commits, wired in via `[patch]` path overrides:

- `bdb23508` — hide the IME only when `KeyFocusLost` says focus went to
  `Area::Empty`. A widget-to-widget move leaves the keyboard to the next
  field, whose draw-time show dedups to nothing when the config matches:
  the keyboard never moves. Verified on device — a next-key press now logs
  the guard arming and *nothing else*.
- `5bf8e78f` — `text_ime_was_dismissed` no longer re-issues `HideTextIME`
  for a keyboard Java has already reported down (Samsung re-presents it
  just to replay the hide animation: dismissals "closed twice"), and a
  primary tap on a `TextInput` clears the dismissed latch, so re-tapping
  an already-focused field can raise the keyboard again.

A third followed once the fork existed, because the reason to avoid one was
gone:

- `99b2a58f` — **mosaic's patch 0003, rebased**: `MAKEPAD_PRESENT_WHEN_
  OCCLUDED=1` keeps an occluded window presenting. This restores *honest
  headless runs* — the e2e window sits behind the user's windows on purpose,
  and upstream's occlusion present-skip meant its screenshots were whatever
  stale frame the surface still held (this session caught `s1` and `s2`
  byte-identical). `background_run()` already set the variable; now
  something reads it. `--front` reverts to what it should be: a way to
  *watch* a run, not a prerequisite for trusting its pictures.

The branch lives at `prepor/makepad@superapp-pin` and Cargo pins its rev, so
the repo builds anywhere; `~/code/makepad-superapp` is the working tree for
the patches. Bumping the pin means rebasing three commits — the mosaic
precedent, without the vendoring. No upstream PRs (Andrey's call). The
app-side timer guard (`ime_guard_*` in `app.rs`) stays as a dormant safety
net for IMEs that behave differently.
