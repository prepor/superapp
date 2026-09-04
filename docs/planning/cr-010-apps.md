# CR-010: the shell and its apps

The first iteration proved the product. This change redesigns its core so
that many more panels, effects, and background services can be added
without touching the shell. It is built as a prototype in `next/` first, on
fakes only, and ported into the real tree once the interfaces have settled.

After it, the code has three layers:

- the **kernel**: everything generic that does not draw. The panel model
  and navigation (today's `core.rs`), the store and its device sync,
  effects and the queue, undo history, the filter and the rich table state,
  search and the launcher list, problems, springs, the e2e grammar, and the
  interfaces an app implements. It never names Makepad, so it is what
  `cargo test` runs without a window.
- the **shell**: everything generic that draws or takes input. The stage,
  chrome, the verb bar, animation, overlays, the shared widgets, the panels
  library, and the hosting of panel widgets. It uses Makepad and depends on
  the kernel.
- the **apps**: mail and files. An app implements the kernel's interfaces
  and supplies its widgets to the shell. Apps may use each other: an app
  reaches another by id through the registry and gets `None` when it is not
  in the build.

The kernel and the shell never name an app. That is the whole rule; there
is no rule between apps.

The word "kernel" is new. The book today calls `core.rs` "the core" and
calls everything Makepad "the shell"; after this change "the kernel" is the
whole makepad-free half, `core.rs` becomes `kernel/layout.rs`, and "the
shell" means the Makepad half only. Where this document says "the shell"
without qualification it means the kernel and the shell together, as
against the apps.

Device sync is not an app. It replicates the store itself, every app's
tables included, and the shell depends on it: the write gate in `act`, the
lock screen, and the lease worker. The test for an app is not "does it have
a panel and a worker" but "does the shell work without it". It does not.

Settings are not a shell panel. What a person configures belongs to the app
it configures: mail owns its accounts panel and the add-account form; the
device-sync form is the kernel's, drawn by the shell.

## Why now

Today one panel kind touches about twenty places, each a place where the
shell knows a mail or files noun:

| Where | What it knows | Lines |
|---|---|---|
| `core.rs` | `Kind` enum with mail and files variants; `Seed`, `Role`, `MailId` | 150 |
| `store.rs` | `kind_cols` / `kind_from` per variant; mail tables in one migration ladder; `backfill_threads`, `backfill_attachments`, `backfill_html`, FTS | 400 |
| `mail.rs` | `title()` for every kind, files and effects included | 40 |
| `launcher.rs` | `kind_word`, `kind_detail`, `mail_extra`, `roots()` | 90 |
| `ui.rs` | `BtnAct`, `MarkVerb`, `FieldId` closed enums; `head_btns`, `head_btns_of`, `mark_verbs`, `preview_kind`, `field_order`, `accels` per kind | 350 |
| `history.rs` | `MarkKeys::{Threads, Names}` | 30 |
| `problems.rs` | `Source::{Account, Send, Sync}`; reads `account` and `outbox` | 200 |
| `effect.rs` | `Outside` has 30 verbs (IMAP, SMTP, OAuth, disk, secrets, clipboard, screen); `Deny`, `Fake`, `Real` each implement all of them | 1,300 |
| `sync.rs`, `send.rs` | `Pump` and the worker threads are mail's, but `State` owns them | 250 |
| `panels.rs` | `PanelAction` with account, draft, send, and dir variants; one 2,200-line DSL block; mailbox and files lists duplicate cursor, marks, keys, and the draw loop | 8,000 |
| `app.rs` | `hosted_tpl`; `draw_hosted` registers e2e hits per kind (480 lines); `resolve_click` runs every verb (400 lines); `handle_panel_actions` (330 lines); triage, filing, hold, delete, new-dir, marks restore, cursor successor (900 lines); wishes and expansion (220 lines); Gmail sign-in (150 lines); `State` fields `expand`, `hold`, `measured`, `signin`, `pump` | 2,700 |
| `catalog.rs` | every scene, shell and app alike, in one list | 1,600 |

Beyond the coupling, three things in the current design are wrong in
themselves and are fixed by the redesign rather than moved:

- a panel has no instance. Everything a panel knows between draws lives in
  the shell's `State` (`expand`, `hold`, `measured`, `signin`) or in the
  Makepad widget, so the shell carries every app's context;
- the header wears the verbs and the marks bar wears them again at the
  foot, with a lending rule to keep the two from disagreeing;
- navigation is decided in the shell (`resolve_click`), although the join
  and replace rules live in the kernel.

## Contract

This section becomes `docs/book/src/apps.md` at the port. The interfaces
are given as they will read in the source, doc comments included: the
comment on a method is its whole specification, and the book chapter
points at the rustdoc rather than repeating it.

### Names

- **PanelId**: what a panel shows, as a tag and arguments. `("message",
  ["42"])`. Two slots may show the same `PanelId`.
- **Slot**: a place in a column, holding one panel instance. `SlotId` is
  the number the layout, joins, focus, and history refer to (today's
  `PanelId = u64`).
- **PanelKind**: the factory an app registers for one tag. It opens
  instances.
- **Panel**: a live instance in a slot. It owns its state: its table, its
  cursor and marks, which messages are open, what it measured.
- **Verb**: one entry of a panel's bar: a button the panel runs by id, or
  a link that navigates.
- **Nav**: an intent to open, replace, preview, close, or focus, decided in
  the kernel.
- **Session**: the kernel object a verb or a widget acts on: the world, the
  layout, history, the workers, navigation, and the notification sink. The
  shell holds one and draws it.

The kernel in `next/kernel` follows these names. Where it had to depart from
the snippets below, the departure is recorded here rather than in the code:
the layout's own `Panel` struct became `Slot` (a slot holds one instance;
`Ws::slots`, `move_slot`, `focus_slot`); a `Tag` read back from a store is
interned so it compares equal to the literal; `Env` carries a concrete
`ClockSource` and `MemSecrets` because `Clock` and `Secrets` are the trait
names; `Session::set_cols` holds the column width in characters that
`Panel::wish` needs, since only the shell can measure it; `Workers::tick`
is the inline drive under virtual time, bounded so a job that files another
job shows as a backlog rather than a hang; a `Replace` counts as a read when
the new instance claimed anything at all; `Session::settle` re-derives the
wishes and writes the session as well as placing and dropping instances,
because all three read them, and `Nav::Close` labels its node with what the
slot shows rather than with the instance's title, because `nav` touches no
instance. Instances are built just before the action and their claims folded
into it, which lands them on the same node and in the same transaction as
the layout change.

### The app list and the registry

The binary is the only place that knows which apps exist:

```rust
// src/main.rs
static APPS: &[&dyn App] = &[&mail::MAIL, &files::FILES, &system::SYSTEM];
static UIS: &[&dyn AppUi] = &[&mail::UI, &files::UI, &system::UI];
```

The kernel builds one `Apps` registry from that list at boot: every tag to
its `PanelKind`, every app by id, plus the effect registry, search
providers, problem sources, roots, and schema ladders. Two apps claiming
one tag stop the process at boot with both names.

```rust
impl Apps {
    /// An app by id, or `None` when it is not in this build. This is how
    /// apps reach each other: mail asks for `"files"` to read its
    /// clipboard, and offers no *attach* when there is no files app.
    pub fn get(&self, id: &str) -> Option<&dyn App>;

    /// The same, downcast to the app's own type, for its public API.
    pub fn get_as<T: App>(&self) -> Option<&T>;

    /// The kind that opens this tag, or `None` for a tag no app in this
    /// build owns.
    pub fn kind(&self, tag: Tag) -> Option<&dyn PanelKind>;
}
```

The shell's own panels (help, about, problems, effects, job, bucket) come
from the `system` app inside the shell, listed like any other, so the shell
uses its own extension points. It is listed last so the launcher's roots
keep today's order: the mailboxes lead, help and about close.

### `App` (kernel)

```rust
/// One app: what it adds to the shell. The binary lists the apps once; the
/// kernel asks the list for everything and never asks an app by name.
pub trait App: Any + Sync + Send + 'static {
    /// One word, stable. Prefixes the app's schema key (`schema:mail`) and
    /// names its e2e directory. What another app asks the registry for.
    fn id(&self) -> &'static str;

    /// Every panel kind the app owns. Read once at boot into the registry;
    /// two apps claiming one tag stop the process there, naming both.
    fn kinds(&self) -> &'static [&'static dyn PanelKind];

    /// The app's own migration ladder, applied at every store open after
    /// the kernel's, in app-list order. `None` for an app that stores
    /// nothing.
    fn schema(&self) -> Option<&'static Schema> {
        None
    }

    /// Demo rows for a new store. Called once, on the first open of an
    /// empty store, and only by the lease holder under replication. Must
    /// be idempotent: a crash between the seed and its record repeats it.
    fn seed(&self, store: &Store) -> rusqlite::Result<()> {
        Ok(())
    }

    /// Registers the app's deferred effects (`reg.register::<Move>()`).
    /// Called for every world built, on any thread, so the queue can be
    /// read back and run wherever it is opened.
    fn effects(&self, reg: &mut effect::Registry) {}

    /// Supplies the app's capabilities for one mode: `Real` gets the
    /// network and the OS, `Fake` the in-memory versions, `Deny` nothing.
    /// Called per world, so a sync worker's sessions live in that worker's
    /// world and nowhere else. `env` carries the store directory, whether
    /// this is a scripted run, the secrets backend, and the clock.
    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {}

    /// The launcher's sources beyond open panels and roots. Each gets its
    /// own thread and store reader in production; under virtual time they
    /// answer inline, so a scripted `type` is followed by its rows in the
    /// same tick.
    fn search_providers(&self) -> Vec<Box<dyn search::Provider>> {
        Vec::new()
    }

    /// Standing conditions this app can be in. Listed on every poll; a
    /// problem is announced once per key and cleared when the source stops
    /// listing it.
    fn problems(&self) -> &'static [&'static dyn ProblemSource] {
        &[]
    }

    /// The background passes the app wants running now, derived from the
    /// store. Asked at boot and again after every action, at the moment
    /// the workers are kicked, so a pass that a new row calls for starts
    /// without a restart. The kernel diffs the answer by `Worker::name`:
    /// new names are spawned, missing names retire. Must be cheap: one
    /// cached query.
    fn workers(&self, store: &Store) -> Vec<Box<dyn Worker>> {
        Vec::new()
    }

    /// The panels the launcher offers whether or not they are open, in
    /// the order the app wants them, each with the label and the words a
    /// query may match. Apps follow the app list.
    fn roots(&self) -> Vec<Root> {
        Vec::new()
    }

    /// For `Apps::get_as`.
    fn as_any(&self) -> &dyn Any;
}

/// One launcher root.
pub struct Root {
    pub id: PanelId,
    pub label: String,
    /// Extra words a query may match ("log queue" for the effects list).
    pub words: String,
}
```

An app's own knobs are environment variables it reads itself
(`SUPERAPP_SEND_DELAY`); argv belongs to the shell.

### `AppUi` (shell)

```rust
/// The Makepad half of an app: its widget templates and its scenes.
pub trait AppUi: Sync + Send + 'static {
    /// The app's own `script_mod!` block. The binary's `AppMain::script_mod`
    /// calls the shell's, then each app's. Template ids carry the app id
    /// (`mail_mailbox_tpl`), which keeps two apps apart in one script
    /// virtual machine.
    fn script_mod(&self, vm: &mut ScriptVm) -> ScriptValue;

    /// The template the shell instantiates for a panel of this tag: one
    /// widget per slot, kept across draws. A tag the app registered without
    /// a template is a boot error.
    fn template(&self, tag: Tag) -> Option<LiveId>;

    /// The app's entries for the panels library, in canvas order after the
    /// shell's own.
    fn scenes(&self) -> Vec<Scene<Setup>>;
}
```

### Panel identity

```rust
/// A kind's name. A plain static string, compared by content; never renamed
/// once written to a store.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Tag(pub &'static str);

/// What a panel shows: the whole identity of it, as the tag of the kind
/// that opens it and the arguments that say which one. `Eq` and `Hash` are
/// all the layout needs: wishes are keyed by it and `showing` compares it.
/// The kernel never reads an argument; their meaning and spelling are the
/// owning app's. Stored as `panel(kind, args)`, the arguments as one JSON
/// array in a text column, readable in `sqlite3` and reachable with
/// `args ->> 0` should a query ever want one.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PanelId {
    pub tag: Tag,
    pub args: Vec<String>,
}
```

The kernel needs four things from the arguments and nothing else: to
compare them, hash them, print them, and store them. A list of strings is
the smallest value that does all four and still lets a kind carry as many
values as it needs without inventing a separator.

An app owns its tags and gives them typed views, which is where the
spelling of the arguments is decided once:

```rust
impl Message {
    pub const TAG: Tag = Tag("message");

    pub fn id(mail: MailId) -> PanelId {
        PanelId::new(Self::TAG, [mail.to_string()])
    }

    /// The mail a `message` panel names; `None` for any other tag, or for
    /// an argument this build cannot read.
    pub fn of(id: &PanelId) -> Option<MailId> {
        (id.tag == Self::TAG).then(|| id.args.first()?.parse().ok()).flatten()
    }
}
```

An attachment is `("attachment", ["42", "3"])`, a forward
`("compose", ["forward", "42"])`, a filtered inbox `("inbox", ["vera@…"])`.

State starts from scratch with this change. The new build refuses a store
of the old shape with one line saying so, and there is no migration. From
here on tags and argument spellings are stable: a tag, once written to a
store, is never renamed, and an argument's meaning at a position never
changes.

A restored slot whose tag no app in the build owns is kept, not dropped.
The shell opens a `Missing` instance for it that shows the tag and *no app
for this panel in this build*, at the default size.

### `PanelKind` and `Panel` (kernel)

```rust
/// The factory for one tag: what an app registers. It opens instances and
/// nothing else; everything a panel knows lives on the instance.
pub trait PanelKind: Sync + Send {
    /// The persisted spelling. Unique across the app list.
    fn tag(&self) -> Tag;

    /// A live instance for `id`. Runs inside the action that is opening,
    /// replacing, or previewing the panel, so what the open claims of the
    /// world (mail marks the thread read) is added through `cx` and lands
    /// on the same undoable node as the layout change. Also runs at
    /// session restore, with a `cx` that takes no claims.
    fn open(&self, id: &PanelId, cx: &mut Opening) -> Box<dyn Panel>;
}

/// What an opening panel may reach and claim.
impl Opening<'_> {
    /// The session, read-only: the store, the world, the apps, the layout.
    pub fn session(&self) -> &Session;
    /// Why the panel is being opened. A cursor walk previews; a solid link
    /// opens; a dotted link replaces; a restore is none of these.
    pub fn how(&self) -> Open;
    /// A write to run in the opening action's transaction, with the intents
    /// that reverse it. Ignored on restore. Consecutive previews from one
    /// slot coalesce into one node, so a cursor walk is one undo.
    pub fn claim(&mut self, write: Write, intents: Vec<Box<dyn Intent>>);
}

/// One panel in one slot. Owns its state between draws: its table, its
/// cursor and marks, which messages are open, what it measured, the text of
/// its fields. The widget that draws it borrows it from the scope and calls
/// its methods on input; a data change is the instance writing through the
/// store it was opened with. Apps downcast their own instances through
/// `as_any`.
pub trait Panel: Any {
    fn id(&self) -> &PanelId;

    /// The header, the tab strip, the launcher label, and the action labels
    /// (*open "…"*). Called on every draw, so it reads only cached queries
    /// or its own state.
    fn title(&self) -> String;

    /// The size the panel asks for, width and height in grid units, given
    /// the column's width in characters. Constant for most kinds; a letter
    /// or a file card asks for the rows its content needs. The layout
    /// clamps it to the active grid. Asked on every relayout, so a measure
    /// that costs anything is taken once and remembered on the instance.
    fn wish(&self, cols: usize) -> (u32, u32);

    /// The bar at the panel's foot, left to right: the buttons that act on
    /// what the panel shows and the links that go somewhere from it, and,
    /// while the panel's table has marks, the batch verbs over the marked
    /// set with their count. Pulled on every draw, so a panel whose bar
    /// changed only has to ask for a redraw. The header wears nothing but
    /// the title and the close button.
    fn verbs(&self) -> Vec<Verb>;

    /// The identity to save in the session for this instance. A job panel
    /// on an in-memory effect saves as the effects list, because ring ids
    /// do not survive the process.
    fn persist(&self) -> PanelId {
        self.id().clone()
    }

    /// One of this panel's own verbs was pressed, or its chord struck: the
    /// verb by its id, with the session to act on. The instance holds
    /// `&mut self` throughout, so reading its own table and then acting is
    /// one method; `act` and `nav` never touch instances (see `settle`).
    fn run(&mut self, verb: &str, s: &mut Session) {}

    fn as_any(&mut self) -> &mut dyn Any;
}
```

### Verbs

```rust
/// One entry of a panel's bar. Two entries are the same verb when they
/// carry the same `id`, which is how a test checks that a batch verb and
/// its single-row twin wear one letter.
pub struct Verb {
    /// Stable and prefixed (`mail.archive`): what history labels, the e2e
    /// harness, and tests name it by.
    pub id: &'static str,
    /// The text drawn. Contains `accel` when there is one; the letter is
    /// drawn bold. A batch verb says so in its label (*archive 3*).
    pub label: String,
    /// The letter, or none. Never one of the workspace's reserved chords;
    /// unique within one bar. The bar asserts both in debug builds; each
    /// app tests its own bars.
    pub accel: Option<char>,
    pub act: VerbAct,
}

pub enum VerbAct {
    /// A button of the panel's own: the bar calls `Panel::run` with the
    /// verb's id on click or chord.
    Run,
    /// A link: navigates on click or chord. Drawn as a link, not a button,
    /// so the three signals of the interaction grammar still hold.
    Go(Nav),
    /// A button that belongs to no panel: a problem row's *retry*. The
    /// closure is the whole behaviour.
    Call(Rc<dyn Fn(&mut Session)>),
}
```

Chords are routed in one order: the workspace's reserved chords first; then
the focused widget, which may take the chord (a live text field takes
`cmd+a`); then the focused panel's bar; then the bar of the panel it
previews, if it drives one. The last step is what lets a list archive the
mail under its cursor without moving focus. Nothing in it names a kind.

A bold letter is a promise that the chord fires that verb now, so the bar
draws bold exactly what the routing would reach: the focused panel's bar
shows its letters except those the focused widget takes while one of its
fields has the keyboard; the bar of the panel it previews shows only the
letters the focused bar leaves free; every other bar shows no letter at
all. A verb whose letter is not bold still fires on click.

### Navigation (kernel)

```rust
/// An intent to change what a slot shows or which slot has focus. The join
/// and replace rules, the preview's focus rule, and the history kind and
/// coalescing are applied by the kernel; the shell animates the result.
pub enum Nav {
    /// A new slot joined to `from`, or un-joined with `fresh`; the new
    /// panel takes focus.
    Open { from: SlotId, id: PanelId, fresh: bool },
    /// `slot` opens `id` in place. Its joined descendants close.
    Replace { slot: SlotId, id: PanelId },
    /// A new slot joined to `from`; focus stays where it was, and the camera
    /// shows the child once. Where the pair cannot share the screen, the
    /// open simply goes.
    Preview { from: SlotId, id: PanelId },
    Close(SlotId),
    Focus(SlotId),
}
```

A list that previews keeps its cursor and its child in step by reading, on
draw, what its joined child shows. No kind declares what it previews into.

Closing is one rule. The kernel owns the joins. `Nav::Close(slot)` may
come from anywhere: the header's close button, a verb that removed what
its own panel shows, a script. Closing a slot closes its joined
descendants and moves focus, by the kernel's rules, and inside a verb's
action it lands on the same node as the data, so one undo brings both
back. That is all there is: no verb looks for other panels on the same
subject, and a list whose rows were removed does not close anything; its
cursor moves to the nearest row and previews it, and the join rule
replaces the old preview child as it does on any cursor step. A panel
elsewhere keeps showing what it shows, and says so if that is gone.

### `Session` (kernel)

`Session` is the whole surface a verb, an instance, or a widget acts on. It
is the kernel's: the shell holds one, lends it to widgets through the scope
(`&mut` during events, shared during draws), and after every event reads
its dirty flags to relayout or redraw. Nothing bubbles up to the stage.
Bodies are elided.

```rust
impl Session {
    pub fn store(&self) -> &Rc<Store>;
    pub fn world(&self) -> &Rc<World>;
    pub fn apps(&self) -> &Apps;
    /// The world's clock.
    pub fn now(&self) -> f64;
    /// The directory beside the store; `None` in memory.
    pub fn db_dir(&self) -> Option<&Path>;
    /// Whether this device may write. A follower's store refuses, and a
    /// verb that touches the disk must ask before it acts.
    pub fn writable(&self) -> bool;

    /// The instance in a slot. Apps downcast their own through `as_any`.
    pub fn panel(&self, slot: SlotId) -> Option<Rc<RefCell<Box<dyn Panel>>>>;
    /// Every slot on every workspace with its instance.
    pub fn panels(&self) -> Vec<(SlotId, Rc<RefCell<Box<dyn Panel>>>)>;
    /// Every slot showing exactly this identity.
    pub fn showing(&self, id: &PanelId) -> Vec<SlotId>;
    pub fn focus(&self) -> Option<SlotId>;
    pub fn joined_child(&self, slot: SlotId) -> Option<SlotId>;
    pub fn join_parent_of(&self, slot: SlotId) -> Option<SlotId>;

    /// One undoable action: mutates the layout, writes the session and
    /// `data` in one transaction on the writer thread, records a history
    /// node with the layout before and after plus the intents, then kicks
    /// the workers and replication. Refuses with a toast when not writable.
    /// Returns what `data` returned, which is how an action learns a new
    /// row id.
    pub fn act<R: Send + 'static>(&mut self, a: Action<R>) -> Option<R>;
    /// `act` without the write gate, for a claim that has already happened
    /// on the disk: the caller checked `writable` before acting and records
    /// the node whatever the lease did in between.
    pub fn act_done<R: Send + 'static>(&mut self, a: Action<R>) -> Option<R>;
    /// The compensation when the lease turned over between a disk write and
    /// its node: reverses the intent and answers the sentence to toast, or
    /// `None` when the device is still writable.
    pub fn give_back(&mut self, intent: &dyn Intent) -> Option<String>;
    /// Adds an intent to the head node after the fact, for an action whose
    /// claim needs the row id `act` returned.
    pub fn claim(&mut self, intent: Box<dyn Intent>);
    pub fn nav(&mut self, n: Nav);
    /// A line for the person; `err` marks it as one. The shell draws it as
    /// a toast; a test reads it back.
    pub fn notify(&mut self, msg: impl Into<String>, err: bool);

    /// Drops the instances of slots that closed and places the ones that
    /// opened. `act` and `nav` never touch instances themselves, so a verb
    /// may hold its own `&mut self` across them; the shell calls this after
    /// every event, and a test calls it before looking at the slots.
    pub fn settle(&mut self);

    /// `kick_all()` wakes every worker and re-asks the apps for the set;
    /// `act` does this itself. `kick(entity)` wakes one. `any()` says
    /// whether anything is running at all.
    pub fn workers(&self) -> &Workers;
    /// Reconcile what has been announced now rather than at the next poll,
    /// so the next failure of the same key is news again.
    pub fn announce_problems(&mut self);
}

/// One action as `act` records it.
pub struct Action<R> {
    /// The history kind (`move`, `read`, `send`). Together with `entity`
    /// it decides coalescing.
    pub kind: &'static str,
    /// The node's label, as the history overlay shows it.
    pub label: String,
    /// What the action is about, as `noun:id` (`slot:7`, `outbox:9`). A
    /// new action with the same `kind` and `entity` as the head node,
    /// within a short window, amends that node instead of adding one: five
    /// moves of one panel are one undo, and a cursor walk that previews a
    /// row at a time is one undo that closes the whole walk. `None` never
    /// coalesces. The same spelling names an effect's row in the queue and
    /// a worker's kick address, so one id means one thing everywhere.
    pub entity: Option<String>,
    /// The layout half.
    pub layout: Box<dyn FnOnce(&mut Wm)>,
    /// The data half; runs on the writer thread.
    pub data: Box<dyn FnOnce(&Transaction) -> rusqlite::Result<R> + Send>,
    /// What the action claims of the world.
    pub intents: Vec<Box<dyn Intent>>,
}
```

The scope carries `PanelProps { slot: SlotId, panel: Rc<RefCell<Box<dyn Panel>>>, hits: Hits }`
beside the session. A widget that needs the store, the world, or another
app reads them off the session; one that changes data calls its instance;
one that navigates calls `nav`. A test drives the same session with no
widget at all.

There is no context bag, no hold, no list interface, no command type, and
no per-kind refresh on the shell: an instance holds its own context, a
clipboard is an app's, a list is a component inside a panel, and an app
that changed the world walks `panels()` and refreshes its own.

### The rich table

The rich table is one component in two halves. The kernel half is state
owned by a panel instance: the `Table<D>` over a `Datasource`, the filter,
the pages, the cursor as key and index, and the `Marks`. The shell half is
one widget that draws any such state: the filter field with completion,
the rows through a portal list, hidden marks above them, the cursor wash,
the mark bars, and the gestures on rows: a click that moves the cursor and
previews, a long press that marks, a sideways drag that runs one of two
verbs the panel names for it.

The panel supplies the row template, how to populate a row, and what a
row opens. Marks restore on undo through an intent the batch verb adds,
holding a handle to its own table. The cursor advancing past a filed mail
is mail's verb finding the driving list through `join_parent_of` and its
own instance through `as_any`. Nothing about lists reaches the shell.

Mailbox, files, and effects lists are three panels over this one
component, which is where three copies of 350 lines become one.

### Hits

A hit is a labelled rectangle with a cursor shape, nothing more. Shell
components register their own into the `Hits` collector on the draw scope:
the table widget its rows by label and its filter field, a link its text, a
button its label, a field its name, a selectable run its text. The e2e
harness resolves a label to its rectangle and synthesizes a real pointer
event there; the widget under it handles the click as it would a human's.
A panel built from shell components is addressable with no code of its
own; only a custom widget adds hits. Later hits win where they overlap, so
a box drawn over rows registers after them.

Rows in a portal list are rebuilt per draw, which is why today's rows
resolve semantically instead of by pointer. The table widget handles the
press itself, by the row rectangles of its last draw, so a synthesized
press lands the same way a finger does. This replaces `WidgetOp` and the
per-kind hit registration in `draw_hosted`.

### Clipboards and other cross-app state

An app that wants to be used by others exposes an ordinary public API and
is found through `Apps::get_as`. The files app owns a clipboard of held
items; its `copy` and `move` verbs fill it and ask for a redraw. Mail's
compose instance, when building its bar, asks for the files app and, when
it is there and holds something, adds *attach*. The item type is files' to
define; when it becomes open enough to hold a message part, mail can put
its attachments on it, which settles open questions 14 and 15 with no
shell concept.

State an app wants others to observe needs no subscription: bars are pulled
on every draw, and a redraw is the one signal.

### Capabilities

```rust
/// Which outside a world gets.
pub enum Mode {
    Real,
    Fake,
    /// Nothing but the clock: an effect that asks for anything else fails
    /// with *this world has no …*. The default for a library mount.
    Deny,
}

/// What `App::outside` may need to build a real or fake backend.
pub struct Env {
    pub db_dir: Option<PathBuf>,
    /// A scripted run: nothing may touch a human's keychain or disk.
    pub scripted: bool,
    pub secrets: Secrets,
    pub clock: Clock,
    pub demo_disk: bool,
}

impl Capabilities {
    /// A backend under the trait it implements. A fake registers itself
    /// under both its traits and its concrete type, so a test can reach
    /// `get::<mail::FakeServers>()` to plant a mail.
    pub fn insert<C: ?Sized + 'static>(&mut self, imp: Box<C>);
    pub fn get<C: ?Sized + 'static>(&mut self) -> Option<&mut C>;
}

impl Ctx<'_> {
    /// How an effect's `perform` reaches the outside:
    /// `cx.cap::<dyn Imap>()?.fetch(...)`. A missing capability is the
    /// error, in words.
    pub fn cap<C: ?Sized + 'static>(&mut self) -> Result<&mut C, String>;
}

impl World {
    /// The UI-thread read a draw is allowed: a files panel lists its
    /// directory through `Disk`.
    pub fn with_cap<C: ?Sized + 'static, T>(&self, f: impl FnOnce(&mut C) -> T) -> Result<T, String>;
}
```

The kernel defines and implements `Clock`, `Secrets`, `Clipboard`,
`Screen`, and `Disk` (real, demo, and none), because attachments, the
harness, and the files app all use them. An app defines its own (mail:
`Imap`, `Smtp`, `OAuth`) and supplies them in `App::outside`. `Effect`,
`Deferred`, `Registry`, the queue, the ring, `World::run`, `enqueue_in`,
and the executor do not change.

### Schema

```rust
/// One app's migration ladder. `meta['schema:<app>']` records how many
/// steps have run. An app never alters another app's tables; new apps
/// prefix their table names with their id.
pub struct Schema {
    pub app: &'static str,
    pub steps: &'static [Step],
}

pub enum Step {
    /// Applied once, in order.
    Sql(&'static str),
    /// Applied once, in order.
    Run(fn(&Connection) -> rusqlite::Result<()>),
    /// Data rebuilt from other rows (the FTS index, the HTML narrowing,
    /// attachment rows): runs whenever `meta[key]` is not `version`, then
    /// sets it. The pattern the three backfills follow by hand today.
    Derived {
        key: &'static str,
        version: i64,
        rebuild: fn(&Connection) -> rusqlite::Result<()>,
    },
}
```

The kernel's ladder owns `meta`, `workspace`, `ws_col`, `panel`, `wm`,
`effect`, and `repl*`. Mail's owns `account`, `folder`, `message`,
`server_msg`, `draft`, `outbox`, `attachment`, `draft_attachment`, and
`message_fts`. Both ladders start at one: a fresh schema, with `panel`
reduced to `kind` and `args`.

### Workers

```rust
/// One background pass with its own thread and its own world (its own store
/// reader, its own real capabilities).
pub trait Worker: Send + 'static {
    /// Unique among running workers (`sync-2`, `sender`); how the kernel
    /// diffs the set after each action.
    fn name(&self) -> String;

    /// The kick address, in the `action.entity` vocabulary: `kick(entity)`
    /// wakes only this one, `kick_all` wakes everyone.
    fn entity(&self) -> Option<String>;

    /// Which queued jobs this thread may run. A job may need something only
    /// one thread holds, such as a live session; the worker holding it
    /// claims the job and no other worker does, so it never burns an
    /// attempt on the wrong thread.
    fn claims(&self, job: &Job) -> bool;

    /// One pass. After it the kernel runs the queued jobs this worker
    /// `claims`, notifies the UI, and sleeps as `Wake` says or until kicked.
    /// Under virtual time the kernel runs every pass inline from the frame
    /// loop instead, then drains the queue until it stops moving.
    fn pass(&mut self, w: &World) -> Wake;
}

pub enum Wake {
    After(Duration),
    OnKick,
}
```

The device-sync lease driver keeps its own thread and command channel
inside the kernel; it needs acquire, release, and override, not only a
kick.

### Problems

```rust
/// Standing conditions derived from rows, never stored: fixing the source
/// condition removes the row.
pub trait ProblemSource: Sync + Send {
    fn list(&self, store: &Store) -> Vec<Problem>;
}

pub struct Problem {
    /// Stable while the condition stands (`account:2`, `outbox:7`), so the
    /// shell can tell a new problem from a standing one.
    pub key: String,
    /// What it concerns, in one line.
    pub label: String,
    /// What is wrong, for a human. Drawn in the one colour.
    pub line: String,
    /// The muted line under it: last success, the recipient, the backlog.
    pub detail: String,
    /// The toast on first sight, or none for a source that announces itself
    /// another way.
    pub announce: Option<String>,
    /// The row's controls as data, so the Problems panel draws any source
    /// without a match.
    pub verbs: Vec<Verb>,
}
```

The unreachable-bucket problem is the kernel's own source.

### Rules

1. Code under `kernel/` names no Makepad type and no app.
2. Code under `shell/` names no app.
3. Apps reach each other only through `Apps::get` and `Apps::get_as`, and
   work when the answer is `None`.
4. A tag, a verb id, an effect kind, and a table name never change once
   written to a store.
5. Every deferred effect says whether it writes. Every bar has no reserved
   chord and no duplicate letter.
6. An app's e2e suites live under `e2e/<app>/` and name only labels its own
   panels draw, plus shell chrome.
7. A test in CI enforces rules 1 and 2 by reading the source.

## The prototype

The redesign is built in `next/` first, on fakes only, and the real tree is
left running. Finding out inside the working tree what Makepad does with
instance-owned tables or a bottom bar would break the product for weeks;
finding it out in a sandbox costs nothing but the sandbox.

### Package

- `next/` at the repo root: its own package `superapp-next`, its own empty
  `[workspace]`, the same Makepad pin and patches, and a shared
  `CARGO_TARGET_DIR` so Makepad builds once for both trees.
- Its own `e2e/` and its own `README.md`. This document is its design
  document until the port; the book stays about the shipping app.

### What is copied

Kernel pieces that are already clean and tested come over as files, then
lose their app nouns: `core.rs` as `kernel/layout.rs` without `Kind`,
`Seed`, `Role`, `MailId`; `spring.rs`; `filter.rs`; `richtable.rs` as the
table state; `history.rs` without `MarkKeys`; `store.rs` without the mail
ladder and backfills; `effect.rs` without `Outside`, `Deny`, `Fake`, `Real`
and the mail types; `search.rs`; `launcher.rs` without its tables;
`problems.rs` without `Source`; `scene.rs`; `e2e.rs`; `theme.rs`. Left
behind: `html.rs`, `oauth.rs`, `sync.rs`, `send.rs`, `repl.rs`,
`object.rs`, `r2.rs`, `secret.rs`, `mac.rs`, and everything under
`panels.rs`, `app.rs`, `catalog.rs`, `library.rs`.

### What is built

- `kernel/panel.rs`: `Tag`, `PanelId`, `PanelKind`, `Panel`, `Opening`,
  `Verb`, the `Missing` panel.
- `kernel/app.rs`: `App`, `Apps`, `Root`, `Schema`, `Worker`, `Workers`,
  `ProblemSource`, `Capabilities`, `Env`, `Mode`.
- `kernel/nav.rs`: `Nav` and its application to the layout, with the
  history kind and coalescing.
- `kernel/caps.rs`: `Clock`, `Secrets`, `Clipboard`, `Screen`, `Disk`, with
  the demo disk.
- `kernel/session.rs`: `Session`, `Action`, notifications, the dirty
  flags.
- `shell/`, split by concern from the first day: `boot`, `stage`, `keys`,
  `pointer`, `touch`, `anim`, `draw`, `hosted`, `overlays`, `bar`, `hits`,
  `dsl`, `widgets/` (the rich table widget, link, field, button, suggest,
  overlays, file card).
- `shell/system/`: help, about, problems, effects, job.
- `apps/mail/`: inbox, message, and compose over the demo seed and a fake
  server. One deferred effect (`move`), one worker (a fake sync pass), one
  search provider, one problem source (a failing send), and the *attach*
  verb that appears when the files app holds something.
- `apps/files/`: a directory list and a card over the demo tree, with a
  clipboard and the copy, move, delete, and new-dir verbs on the demo
  `Disk`.
- `e2e/`: the grammar and harness as they are; suites for the join and
  replace journey, focus, workspaces, launcher, undo, filter, marks,
  files, compose, effects.

Out of scope in the prototype, but the interfaces must not preclude them:
touch and Android, the IME, the panels library, HTML mail and pictures,
attachments, OAuth, real IMAP and SMTP, device sync and the lock screen.
The virtual clock and the e2e bridge are in scope from the first commit,
because every suite depends on them.

### Done

The prototype is done when:

- the interaction grammar's suites pass on it under `--no-draw`;
- the bar, the chord routing, and the preview walk feel right in a
  windowed run;
- nothing under `next/kernel` or `next/shell` names mail or files, by
  the boundary test;
- adding a third fake panel kind to an app touches only that app's
  directory;
- the book's shell chapters could be written from it.

### The port

Then the real tree is rewritten onto the prototype, app by app, in one
branch and one pull request:

1. `next/kernel` and `next/shell` replace `src/` wholesale, with `mac.rs`
   and `secret.rs` under `platform/` and device sync under `kernel/repl/`.
2. Mail is ported panel by panel: mailboxes, message with its thread,
   HTML, pictures, attachments, compose, contact, accounts, add-account
   with the Gmail sign-in; then sync, send, and the real `Imap`, `Smtp`,
   `OAuth` capabilities.
3. Files is ported with the real `Disk`, then the file card is shared
   with attachments.
4. The panels library comes back with per-app scenes.
5. Every existing e2e suite is regrouped under `e2e/<app>/`, with a
   `# args:` and `# env:` header replacing the `case` table in
   `run-all.sh`.
6. The book is restructured (below) and `README.md` describes the real
   layout. This document is deleted.

Old stores are refused with one line; sibling checkouts on the old shape
use their own store file until they catch up.

## The book

The book keeps its rule: the code and the book describe the same product,
and one of them is fixed when they disagree. It gains a second rule: a shell
chapter states rules and components and may use an app as a one-line
example; an app chapter states what that app does and links to the rule it
follows rather than restating it.

New summary:

```
[About this Book]
[Vocabulary]

# The shell
- Overview
- Panel Model
- Interaction Grammar
- Look & Feel
- Architecture
- Data and Effects
- Device Sync
- The Rich Table
- Apps

# Apps
- Mail
- Files

# Development
- Tech Stack
- Developer Experience
- Open Questions
```

What moves:

- `interaction-grammar.md` keeps the three signals, preview, keyboard,
  workspaces, launcher, undo, mouse, and touch. Its accelerator section is
  rewritten for the bar: the bar's letters, the routing order, no lending
  rule. *Four mailboxes*, *Reading a conversation*, *Attachments*, the
  account and send parts of *Problems*, and *Files* move to the app
  chapters. The generic half of *Problems* stays.
- `look-and-feel.md` describes the header with only a title and a close
  button, and the bar at the foot.
- `data-substrate.md` keeps writing data, effects, queued jobs, the ring,
  cached queries, session persistence, and data outside SQLite. *Mail sync*,
  *Conversations and attachments*, *Sending*, and *Gmail sign-in* move to
  `mail.md`; *Files* to `files.md`. *Device sync* becomes its own shell
  chapter, `device-sync.md`, with the lease, the bucket, the lock screen,
  and a pointer to the TLA+ model under `formal/`.
- `richtable.md` describes the component in its two halves and keeps
  *Adding a table*. The mailbox tag list moves to `mail.md`.
- `vocabulary.md` keeps panel, workspace, column, grid, wish, mark, join,
  bridge, chain, camera, scene, ghost, toast, problem, effect, job. *Kind*
  becomes *panel identity* and *slot*; it gains kernel, shell, app, tag,
  verb, bar, capability, worker. *Mailbox* and *Thread* move to `mail.md`.
- `overview.md` lists the apps with links instead of describing mail.
- `architecture.md` opens with the three layers and their direction, then
  gets three module tables (kernel, shell, apps), the rules, and a short
  version of the contract. Its "two main parts" paragraph goes.
- `apps.md` is the contract above.
- `dev-x.md` documents the boundary test, the per-app e2e layout, and an
  *adding an app* checklist.
- `open-questions.md` tags each item with the chapter it belongs to, and
  drops 14 and 15.
- `README.md` describes the real layout; today it names modules that do not
  exist.

## Where today's code goes

| Today | After |
|---|---|
| `core::Kind` variants, `Seed`, `Role`, `MailId` | `PanelId { tag, args }`; typed views in mail and files |
| `core::PanelId` (u64) | `SlotId` |
| `Kind::grid`, `message_rows`, `file_rows`, `wish_ahead`, the wish loop in `State::sync` | `Panel::wish` |
| `mail::title` | `Panel::title` |
| `launcher::{kind_word, kind_detail, mail_extra, roots}` | `Root`, `Hit`, and `Panel::title` |
| `store::{kind_cols, kind_from}` | `PanelId` to and from `(kind, args)`; `Panel::persist` for the ring-id case |
| `ui::{head_btns, head_btns_of, mark_verbs, hold_btn, preview_kind, field_order, accels, lends}` | `Panel::verbs` and the chord routing order |
| `ui::{BtnAct, MarkVerb, FieldId}`, `RowSwipe`, `list_archives` | `Verb` values built by instances; swipes in the table widget |
| `hosted_tpl` | `AppUi::template` |
| the reseed block in `draw_hosted`, `pending_focus`, `reseed_composes`, `refresh_files` | instance state and the app's own walk over `panels()` |
| the per-kind blocks of `draw_hosted`, `WidgetOp`, `MarkRow`, `mark_row`, `row_rect` | `Hits` registered by shell components |
| `Stage::{marked, field_focused}`, `lender`, `wears` | the chord routing order |
| `restore_marks`, `history::MarkKeys`, `successor_of`, `neighbour_of`, `toggle_mark` | the table state on the instance; a marks intent; mail's verb through `as_any` |
| `Act::Btn` arms, `triage`, `triage_marked`, `here`, `delete_paths`, `delete_marked`, `hold_marked`, `new_dir`, attach | arms of `Panel::run` on the instances |
| `PanelAction::{DraftEdited, AddAccount, ConnectBucket, NewDir, …}`, `WidgetOp::Suggest`, `handle_panel_actions` | instance methods called by the widget; a bar verb where the action is undoable |
| `PanelAction::{Open, Preview, Select, FollowLink}`, `Act::{Open, Replace, Preview, Close, Focus}`, `resolve_click` | `Nav` in the kernel |
| the three *mark the thread read* copies in `resolve_click`, `seed_expansion`, `toggle_msg` | `Opening::claim` and the message instance |
| `State` itself, `State::{act, act_done, act_nav}`, `lost_the_lease`, `history.claim`, `toast` | `Session` in the kernel: `act`, `act_done`, `give_back`, `claim`, `notify` |
| `State::{expand, hold, measured, signin}` | instance fields; files' clipboard |
| `files::Hold` | files' clipboard, read by mail through `Apps::get_as` |
| `panels_under`, the reader-closing in `triage` and `file_mails`, `delete_paths`'s close loop | `Nav::Close` of the verb's own slot; the kernel closes descendants; nothing else closes |
| direct `state.ws` reads | `Session::{panels, showing, panel, …}` |
| `effect::Outside`, `Deny`, `Fake`, `Real` | `Capabilities`; kernel `Clock`, `Secrets`, `Clipboard`, `Screen`, `Disk`; mail `Imap`, `Smtp`, `OAuth` |
| `effect::Scope` | `Worker::claims` |
| `sync::Pump`, `sync::spawn`, `send::spawn`, `sync::{tick, settle}`, `spawn_workers` | `Worker`, `Workers`, `App::workers` |
| the migration ladder, `backfill_*`, FTS in `store.rs` | `Schema` per app, `Step::Derived` |
| `mail::{register, registry}` | `App::effects` |
| `search_engine`'s provider list | `App::search_providers` |
| `problems::Source`, `problems::list`, `announce_problems`'s match, the problems block of `draw_hosted` | `ProblemSource`, `Problem::{announce, verbs}` |
| `Config::send_delay` | `SUPERAPP_SEND_DELAY`, read by mail |
| `catalog::scenes` | `AppUi::scenes` plus the shell's own |
| `panels.rs`'s one `script_mod!` | `AppUi::script_mod` per app, prefixed template ids |
| `ui::tests::every_kind`, `store::tests::every_kind_round_trips_through_its_row` | each app's tests over its own panels |

## Risks and decisions

- **Losing exhaustive matches.** An open `PanelId` means the compiler no
  longer lists every kind at each site. That is the point: those sites are
  the coupling. Completeness moves to boot checks (every tag has a
  template) and each app's tests.
- **Widgets mutate the session through the scope.** Makepad's `Scope`
  lends `&mut Session` to hosted widgets during events. Instances are
  never touched by `act` or `nav`; closed ones are dropped and new ones
  placed in `settle`, after the event, so a verb running as `&mut self`
  may close its own slot.
- **A chord the widget takes.** The stage offers a chord to the focused
  widget before the bar and needs to hear back. The widget answers with an
  action in the same event; the stage checks for it before running a verb.
- **Instance-owned tables in Makepad.** The table widget borrows the state
  from the scope every draw and event, and answers a synthesized press by
  its own row geometry.
- **Two trees for a while.** The prototype is the kernel, the shell, and
  the contract, on fakes. The temptation to make it real is resisted; the
  port brings the real apps.
- **Shared stores across builds.** The old store shape is refused, not
  migrated. A build without an app keeps the slot rather than dropping it.
- **Root order in the launcher** follows the app list (mail, files, then
  the shell's own); the mailboxes still lead and help and about still
  close.
- **The crate split** is not part of this change. Makepad's
  `crate_resource` paths, the font bundle, and the Android packager are per
  crate, and the cost of moving them is unknown. The boundary test gives
  the guarantee now.
