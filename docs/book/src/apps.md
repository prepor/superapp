# Apps

An app is what the shell can be extended with without being touched. Mail and
files are apps; so is `system`, the shell's own, which supplies help, about,
the effect log, the problems list, the device-sync form, and the card a panel
gets when no app in this build owns its tag.

The binary is the only place that knows which apps exist. `app/src/lib.rs`
holds two lists side by side: `APPS`, what each app adds to the store, the
queue and the launcher, and `UIS`, what it adds to the screen. The kernel
builds one registry from the first at boot; the shell asks the second for
templates.

The doc comments on the traits are the specification. This chapter says what
the pieces are and how they fit; `cargo doc -p kernel --open` says exactly what
each method promises.

## Panel identity

`kernel::panel::Tag` is a kind's name as a plain word. `PanelId` is a tag and
its arguments: `inbox`, `message(42)`, `attachment(42, 3)`,
`compose(forward, 42)`. The kernel needs four things from the arguments and
nothing else: to compare them, hash them, print them, and store them. A list of
strings does all four and lets a kind carry as many values as it needs without
inventing a separator.

An app owns its tags and gives them typed views, which is where the spelling of
the arguments is decided once: `Message::TAG`, `Message::id(mail)`, and
`Message::of(&id)` are mail's, and no other layer reads that argument.

A tag, once written to a store, is never renamed, and an argument's meaning at
a position never changes. A restored slot whose tag no app in this build owns
is kept, not dropped: it opens as `panel::Missing`, which shows the tag and
*no app for this panel in this build*, and it saves back unchanged. Another
build has the app, and the session is shared.

## What an app registers

`kernel::app::App` requires only an `id`, the kinds it owns, and the downcast
hook. Everything else has a default, so an app supplies only what it has.

| Method | What it adds |
|---|---|
| `id` | one stable word: the schema key `schema:<id>`, the e2e directory, what another app asks the registry for |
| `kinds` | every `PanelKind` the app owns; two apps claiming one tag stop the process at boot, naming both |
| `schema` | the app's own migration ladder, applied after the kernel's in app-list order |
| `seed` | demo rows for a new store, once, on the first open of an empty one |
| `effects` | the app's deferred effect kinds, registered per world so a filed job decodes wherever it is read |
| `outside` | the app's capabilities for one mode: `Real` gets the network and the OS, `Fake` the in-memory versions, `Deny` nothing |
| `search_providers` | the launcher's sources beyond open panels and roots |
| `problems` | the standing conditions the app can be in |
| `workers` | the background passes the app wants running now, derived from the store |
| `roots` | the panels the launcher offers whether or not they are open |

`AppUi`, in `app/src/shell/app_ui.rs`, is the Makepad half: `script_mod` for
the app's own template block, `template(tag)` for the widget the shell
instantiates per slot, and `scenes` for its entries in the panels library.
Template ids carry the app id (`mail_inbox_tpl`), which keeps two apps apart
in one script virtual machine. A tag registered without a template is a boot
error.

## Kinds, instances, and verbs

`PanelKind` is the factory for one tag and does nothing else: it answers `tag`
and it opens instances. Everything a panel knows lives on the instance.

`PanelKind::open` runs inside the action that is opening, replacing, or
previewing the panel. Its `Opening` context lends the session read-only, says
`how` the panel is being opened, and takes `claim`s: a write with the intents
that reverse it. That is how marking a thread read on open lands on the same
undoable node as the layout change. A restore is an `Open::Restore` and takes
no claims.

`Panel` is the live instance in a slot. It owns its own state between draws:
its table, its cursor and marks, which messages are open, what it measured, the
text of its fields. It answers `id`, `title`, `wish`, `verbs`, `persist`, and
`run`, is told `placed(slot)` once the layout has run, and lends itself through
`as_any` so its own app can downcast it. The widget that draws it borrows it
from the scope and calls its methods on input.

`Verb` is one entry of the bar. Its `id` is stable and prefixed
(`mail.archive`) and is what history labels, the e2e harness, and tests name it
by; two entries with one id are one verb, which is how a test checks that a
batch verb and its single-row twin wear one letter. Its `act` is one of three:
`Run` calls `Panel::run` with the id, `Go(nav)` navigates and draws as a link,
and `Call(f)` is a closure for a button that belongs to no panel, such as a
problem row's *retry*.

Verbs dispatch by id, never by matching a type. A bar is pulled on every draw,
so a panel whose bar changed only has to ask for a redraw, and a panel that
wants another app's state in its bar simply asks for it while building one.

## The session

`kernel::session::Session` is the whole surface a verb, an instance, or a
widget acts on. The shell holds one, lends it to widgets through the scope
(`&mut` during events, shared during draws), and after every event reads its
dirty flags to relayout or redraw. Nothing bubbles up to the stage.

It answers for the world: `store`, `world`, `apps`, `now`, `db_dir`,
`writable`. It answers for the layout: `panel(slot)`, `panels()`,
`showing(id)`, `focus()`, `joined_child`, `join_parent_of`. It changes things
through `act`, `act_done`, `nav`, `notify`, and `claim`. It runs the
background through `workers()`.

`act` is one undoable action. It mutates the layout, writes the session and the
action's own `data` closure in one transaction on the writer thread, records a
history node with the layout before and after plus the intents, then kicks the
workers and replication. It refuses with a toast when the device may not write.
It returns what `data` returned, which is how an action learns a new row id.

An `Action` carries a `kind` (`move`, `read`, `send`), a `label` for the
history overlay, and an `entity` as `noun:id` (`slot:7`, `outbox:9`). A new
action with the same kind and entity as the head node, within a short window,
amends that node instead of adding one: five moves of one panel are one undo,
and a cursor walk that previews a row at a time is one undo that closes the
whole walk. The same spelling names an effect's row in the queue and a worker's
kick address, so one id means one thing everywhere.

`act` and `nav` never touch instances. `settle` is what does: it drops the
instances of slots that closed, places the ones that opened, re-derives the
wishes, and writes the session. The shell calls it after every event and a test
calls it before looking at the slots. That is what lets a verb running as
`&mut self` close its own slot.

There is no context bag, no hold, no list interface, no command type, and no
per-kind refresh on the shell. An instance holds its own context, a clipboard
is an app's, a list is a component inside a panel, and an app that changed the
world walks `panels()` and refreshes its own.

## Navigation

`kernel::nav::Nav` is an intent to open, replace, preview, close, or focus. The
join and replace rules, the preview's focus rule, and the history kind and
coalescing are applied in the kernel; the shell animates the result.

Closing is one rule. `Nav::Close` may come from anywhere, closes the slot's
joined descendants, moves focus by the layout's rules, and inside a verb's
action lands on the same node as the data, so one undo brings both back. It
carries the panel's title as an optional label, read off the instance by the
caller, because `nav` touches no instance. A list whose rows were removed
closes nothing: its cursor moves to the nearest row and previews it, and the
join rule replaces the old preview child as it does on any cursor step.

A list that previews keeps its cursor and its child in step by reading, on
draw, what its joined child shows. No kind declares what it previews into.

## Apps reaching each other

`Apps::get(id)` answers an app by id, or `None` when it is not in this build.
`Apps::get_as::<T>()` is the same downcast to the app's own type, for its
public API. An app that wants to be used by others exposes ordinary public
methods and is found that way.

The files app owns a clipboard of held items; its *copy* and *move* verbs fill
it. Mail's compose instance, while building its bar, asks for the files app and
adds *attach* when it is there and holds something. Take files out of the
build and compose simply offers no *attach*. State an app wants others to
observe needs no subscription: bars are pulled on every draw, and a redraw is
the one signal.

## Capabilities

A capability is a trait an effect reaches the outside through. The kernel owns
the five every build needs, in `kernel/src/caps/`: `Clock`, `Secrets`,
`Clipboard`, `Screen`, and `Disk`, because the harness, attachments, and a file
browser all use them. An app defines its own and supplies them in
`App::outside`; mail's are `Imap`, `Smtp`, and `OAuth`.

`Mode` says which outside a world gets: `Real` the network and the OS, `Fake`
the in-memory versions, `Deny` nothing but the clock, which is the default for
a library mount. `Env` carries what an implementation needs to be built: the
store directory, whether this is a scripted run, the secrets backend and the
shared in-memory secrets, the clock, and whether the disk is the demo tree.

The kernel installs its own capabilities before the apps', so an app or the
shell may replace one. `Ctx::cap::<dyn Imap>()` is how an effect asks for one
and a missing capability is the error, in words. `World::with_cap` is the read
a draw is allowed, which is how a files panel lists its directory.

## Schema

`Schema` is one app's migration ladder, and `meta['schema:<app>']` records how
many steps have run. `Step::Sql` and `Step::Run` are applied once, in order.
`Step::Derived` is data rebuilt from other rows, versioned by the walk that
made it rather than by the ladder's counter, so a better walk rebuilds every
store on its next open however old it is.

An app never alters another app's tables, and new apps prefix their table names
with their id. The kernel owns `meta`, `workspace`, `ws_col`, `panel`, `wm`,
`effect`, and the two `repl` tables, and nothing else.

## Workers

`Worker` is one background pass with its own thread and its own world: its own
store reader and its own real capabilities. It answers `name` (unique among
running workers, `sync-2`, `sender`), `entity` (its kick address, in the
`action.entity` vocabulary), `claims` (which queued jobs this thread may run),
and `pass`, which returns `Wake::After(d)` or `Wake::OnKick`.

`App::workers(store)` is asked at boot and again after every action, at the
moment the workers are kicked. The kernel diffs the answer by name: new names
are spawned, missing names retire, so a pass that a new row calls for starts
without a restart. The answer must be cheap: one cached query.

Under virtual time there are no threads. Every pass runs inline from the frame
loop and the queue is then drained until it stops moving, bounded, so a job that
files another job shows as a backlog rather than a hang.

## Problems

`ProblemSource::list(store)` is asked on every poll. A `Problem` carries a
`key` that is stable while the condition stands (`account:2`, `outbox:7`), the
line and the detail a person reads, an optional `announce` for the toast on
first sight, and its own `verbs` as data, so the Problems panel draws a source
it has never heard of. Nothing is stored: fixing the source condition removes
the row. The unreachable-bucket problem is the kernel's own source, and it is
listed first.

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
7. Tests in CI enforce rules 1 and 2 by reading the source. See
   [Developer Experience](./dev-x.md#the-boundary-tests).
