# Calendar

Google account sign-in is shared with Mail and works through
[Accounts on Android](./accounts.md#android-sign-in). Select Calendar during
Google consent, or reconnect an existing account with Calendar enabled.
The phone keeps its own grant; syncing the account row alone does not sign in.

Calendar opens on an upcoming timeline. Each Google event occurrence is a
separate row, with its time, calendar, account, invitation state, and Meet
indicator. Date headings use the shared rich-table section support. The filter
is the standard text filter; there is no preset “Next 30 days” selector or row
of calendar checkboxes beneath it.

**month** replaces the timeline with a Monday-first, six-week grid. Previous,
next, and today move through months. Selecting a date opens that day's agenda;
selecting an agenda row previews its event in a joined panel. Both views keep
the filter when switching. Multi-day events occupy every overlapping date.
The grid gives each event its own padded row and calendar accent, marks today
with a dark date badge, and softens weekends and dates outside the current month.
Busy dates show an overflow count; opening the day reveals every event.

Event details use the shared rich-text reader: links in descriptions, locations,
and Meet URLs are clickable. Descriptions retain HTML formatting and labelled
links; plain web addresses and email addresses are detected automatically.
Text remains selectable, and a link opens once even with several events visible.

## Sources and accounts

The **calendars** panel lists each source's account, Google calendar ID,
permissions, time zone, Meet support, and synchronization status. **show events**
opens a filtered timeline; **new event here** chooses that writable calendar.
Connect and reconnect Google identities in the shared [Accounts](./accounts.md)
app. Mail and Calendar use the same identity and refresh grant.

The filter accepts free text and these tags:

| Tag | Meaning |
|---|---|
| `@calendar:` | calendar title |
| `@source:` | local calendar source ID |
| `@account:` | account address |
| `@with:` | guest address or name |
| `@date:` | occurrence date |
| `@after:`, `@before:` | Unix timestamp bounds |
| `@meet` | has a returned Meet link |
| `@invited` | needs your RSVP |
| `@all_day` | all-day events |

The default timeline begins now. The synchronized range initially covers the
previous 90 days and the next year. Approaching the end of the timeline fetches
another year automatically. If a filter finds no further events, scrolling
onward checks more dates; idle redraws do not keep extending the range.
Displaying or switching months fetches all dates in the grid, including days
from adjacent months; opening a day agenda fetches that day. Restored views do
the same. There is no manual “load more” control. Loading status and the last
refresh are visible. Missing cached events are never evidence that someone is free.

## Event editing

**new event**, **edit**, and **duplicate** open persistent local drafts. The
editor includes title, calendar, local start/end, IANA time zone, all-day state,
guests and optional guests, location, notes, Meet, recurrence, visibility,
busy/free, reminders, guest permissions, and whether to notify guests. Tab and
Shift-Tab move among fields and reveal the focused field when necessary.

Calendar, Show as, and Visibility open choice menus. Use the pointer or arrow
keys to browse, Enter to choose, and Escape to dismiss without changing the
draft. Calendar choices include writable calendars; an existing event keeps
its original calendar.

Draft preparation and saves run in the background. Typing stays visible while
the writer is busy; consecutive pending saves keep the newest form. Saving to
Google waits for those local saves before submitting the reviewed revision.
Accepted saves continue after the editor closes and drain during shutdown.
Tools use the same prepared transactions, including a final check of the
draft revision, event ETag and calendar permissions before the write commits.

Guests autocomplete by name or email using cached event guests and organizers,
non-spam Mail correspondents, and connected account addresses. Suggestions omit
people already invited and preserve the `?` prefix for optional guests. Choose
with the arrow keys and Enter or Tab, or click a suggestion; Escape dismisses it.
This uses local data and does not require Google Contacts access.

Time zones are searchable by IANA name or city. Locations suggest recently used
event locations, and repeat and reminder fields offer readable presets while
retaining custom values. A completed preset closes its suggestion box so Tab can
continue through the form.

Start and end accept `YYYY-MM-DDTHH:MM`, or an RFC3339 timestamp with an explicit
offset. Ambiguous and skipped local times around daylight saving are rejected;
an existing event in a repeated hour retains its offset. All-day events use an
exclusive end date: September 8–9 represents September 8 alone.

The repeat control cycles daily, weekly, monthly, yearly, and no recurrence.
The custom field accepts RFC5545 recurrence lines, for example
`RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=12`. An occurrence keeps the original series
rule unless it is changed explicitly. Recurring changes offer this occurrence,
all occurrences, or this and following occurrences. Splitting following events
uses Google's two-operation procedure; complex multi-rule sets require editing
in Google Calendar. Exceptions on a split series follow Google's behavior.

Reminders accept `default`, an empty value for none, or entries such as
`popup:10,email:60`. Guests are comma-separated addresses; `?person@example.com`
marks an optional attendee. Existing attendee response and resource fields are
preserved. Edits use PATCH so provider fields the editor does not expose survive.

**save event** queues the reviewed draft revision. Local draft, saving, saved,
and failed states are distinct. A queued operation is not reported as complete
until Google accepts it. Conflicts keep the draft and offer **review latest**;
the rejected version cannot be edited or blindly retried. Reviewing opens the
current event so a new change can be made deliberately. **Dismiss error** clears
a rejected conflict locally and preserves its draft and operation history.
Ambiguous failures offer retry using the same operation identity. The Problems
panel also exposes failed changes and synchronization failures. When a later
reviewed deletion of the same occurrence succeeds, obsolete deletion conflicts
are retired automatically, including ones left by an earlier app version.

Local draft edits participate in undo. Submitted Google changes cannot be
recalled by workspace history; making a further change requires an explicit
edit or deletion.

Deletion has an explicit confirmation and recurring scope. Invitation details
offer yes/maybe/no RSVP, and events with a returned video entry point offer
**join meet**. Special Google event types remain readable, with the provider
website available for unsupported editing.

## Meet and finding a time

Meet uses `conferenceData.createRequest` with `conferenceDataVersion=1`. Google
can return a pending conference request; only a returned video entry point is
shown as a join link. The source must advertise Meet support.

**find a time** opens a scheduling sheet beside the draft. Choose the date,
meeting length, search hours and time zone, then **check availability**. Date
arrows check the previous or next day; meeting lengths and times offer presets.
The default search covers 09:00–17:00 on the draft's date, or the next applicable
date when that window has passed.

The sheet checks Google's FreeBusy endpoint for the draft's guests and all
connected owned calendars, regardless of display filters. Other connected
accounts use their own grants: if a guest cannot be checked through the event's
account, Calendar tries the remaining connected accounts until it finds access.
Successful checks are kept, and each guest row identifies the account that
provided availability. Unavailable calendars show the reason Google returned
and which accounts were tried; a temporary error is distinct from missing access.
Participant rows share a single time axis, with
busy blocks, striped unknown availability, and the selected time outlined across
every row. Owned calendars are combined in the **You** row.

Select a suggested time, or drag the outlined time on any participant track.
Dragging snaps to fifteen-minute increments, keeps the pointer's position within
the selection, and fits the whole meeting inside the checked window. Clicking
empty track space chooses a new start; Escape cancels a drag. Busy overlaps are
highlighted and named above the tracks. Then **use this time** updates the
original local draft and returns to its editor. Selecting or dragging alone
does not change the draft.
Unknown calendars stay unknown; partial suggestions only cover readable
calendars, and no suggestions appear if none can be checked. A suggestion is
not a reservation.

Changing the meeting length immediately resizes the selection and recalculates
suggestions from the same checked busy intervals. The selected start stays in
place unless the longer meeting needs to move earlier to fit the window.
Incomplete duration input can be corrected without losing that start.

Hover a busy block to see the event's title, exact times and location when
available. Details load after the free/busy result, trying connected accounts
for readable events without delaying time selection. Private or unreadable
events remain **Busy**; missing details never make an occupied interval free.
Hovering the combined **You** row identifies the calendars behind its events.

Editing search controls does not alter an in-flight request. Each check creates
a new request using the draft's current guests and account. Changed dates,
search hours, time zone, participants or account, and results older than five
minutes require another check before applying a time. Duration changes and
event-detail lookups do not renew that timestamp. Search controls survive panel
restoration.

Google's API supports both features. Reading another person's free/busy depends
on sharing and Workspace permissions; knowing an email address grants no access.
Requests are bounded to fourteen days and fifty calendars, with meeting
lengths from fifteen minutes to eight hours. See Google's
[conference creation guide](https://developers.google.com/workspace/calendar/api/guides/create-events),
[FreeBusy reference](https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query),
[event-list reference](https://developers.google.com/workspace/calendar/api/v3/reference/events/list),
and [sharing model](https://developers.google.com/workspace/calendar/api/concepts/sharing).

## Persistence and synchronization

| Table | Contents |
|---|---|
| `calendar_source` | account, provider ID, permissions, zone, conference support, freshness |
| `calendar_event` | expanded occurrences and full raw Google event, keyed by source and remote ID |
| `calendar_draft` | revisioned local form, base snapshot, status, error |
| `calendar_change` | durable create/update/delete/RSVP operation and result |
| `calendar_sync` | requested cache range, refresh generations, last status |
| `calendar_availability` | queued free/busy query, per-calendar coverage, candidate times and optional shared event details |

A worker processes outgoing changes and availability requests, and refreshes
the bounded cache every five minutes or on request. Calendar lists and event
lists are paginated. A source snapshot replaces cached occurrences only after
all pages have arrived; a failed refresh retains the previous cache. This is a
bounded full refresh, not a `syncToken` implementation.

Outgoing operations persist a random operation ID before networking. Creates
use it as a deterministic Google event ID; retries recover an accepted request
whose response was lost. Updates use ETags and an operation marker, and reject
unreviewed changes. Following-series edits persist the split identity so retry
can finish a partial split without creating another replacement series.
An operation retains uncertainty across retries and process restarts: a later
ETag rejection cannot prove an earlier request was unapplied. These operations
remain visible and keep their original retry identity. A rejected preflight
publishes the event version it fetched to the local cache immediately, without
waiting for a complete calendar refresh. Conflict recovery and history cleanup
run through the background worker and serialized SQLite writer.
Tokens stay outside the store, transport rejects redirects, and failures do not
copy token-bearing requests or provider response bodies into panel context.

## Agent surface

Calendar supplies an app description, per-panel context, strict tool schemas,
and the same draft/queue implementation used by the UI.

`calendar.events` automatically requests dates outside the cached range, too.
It returns the current cache with `loading: true` while synchronization is
pending; query again once it finishes and inspect `coverage` for any errors.

| Tools | Purpose |
|---|---|
| `calendar.calendars`, `calendar.events`, `calendar.event` | sources, bounded cached occurrences, full event and revision |
| `calendar.suggest` | the editor's guest, location, time zone and field preset suggestions |
| `calendar.create`, `calendar.update` | create or modify an event directly, retaining a draft and queueing the Google write atomically |
| `calendar.draft`, `calendar.update_draft` | create, read and edit a persistent local draft |
| `calendar.commit` | queue the exact reviewed draft revision |
| `calendar.delete`, `calendar.respond` | delete with explicit scope, or send RSVP |
| `calendar.operation`, `calendar.retry` | inspect completion and retry the original operation |
| `calendar.availability`, `calendar.availability_result` | check free/busy across connected identities and inspect coverage, account attempts, errors, times and shared event details; optional `minutes` recalculates fresh results locally |
| `calendar.use_time` | apply a snapped start and duration to the checked request's original local draft, returning busy conflicts and unknown calendars |

`calendar.create` takes a writable `source` from `calendar.calendars` and a
`form` with `title`, `start`, `end`, `zone`, and an explicit `notify` choice.
Optional fields use the editor's formats described above. `calendar.update`
takes the `event` ID and current `etag` from `calendar.event`, plus `changes`
containing the fields to modify and explicit `scope` (`this`, `all`, or
`following`) and `notify` choices. Omitted fields and unexposed Google fields
are preserved; an empty string clears a text field. Updates keep the event's
calendar.

Both direct tools return `draft`, `operation`, and `queued: true`. Inspect
`calendar.operation` until it reports `done` or `failed`; queueing is not
completion. A failed operation retains its draft for review and recovery.
Preparation does not write, and a permission or event-version change before
the transaction commits leaves neither an orphan draft nor an operation.
The separate draft tools remain available for preparing and reviewing local
changes before submission.

Create, update, commit, delete, RSVP, and retry require agent approval because
they can notify other people. Local draft edits do not send invitations. A
draft revision or ETag is mandatory when changing an existing draft or event.
`calendar.use_time` uses the latest local draft revision and does not submit
to Google; `calendar.commit` remains a separate operation for that draft
workflow. Tools report queued work as queued, and direct SQL writes are not
a substitute for a successful Google operation.

## Native panels and verification

The registered tags are `calendar`, `calendar-month`, `calendar-event`,
`calendar-edit`, `calendar-availability`, and `calendar-sources`. Timeline and month
are launcher roots. The panel library includes native scenes.

Event details, draft forms, availability candidates, participant tracks and hover
details load through explicit background snapshots. Each snapshot permits one
preparation at a time and discards superseded results; loading data never looks
like a missing draft or an empty checked schedule. Parsing, sanitizing event
HTML, resolving names and calculating candidate times happen outside the UI
thread. Pointer movement shares prepared interval arrays and checks conflicts
with a binary search. Guest completion filters background SQL results to eight
suggestions per source and refreshes the unchanged caret when they arrive.
Source metadata and synchronization status use their own snapshots. Timeline
and month views wait for the source timezone instead of treating loading as
an empty calendar list. The month worker prepares counts and at most three
entries for each of the 42 displayed days; redraws never clone or scan the
full cached month. Retrying a draft resolves its latest operation inside the
SQLite transaction.

The deterministic fake supplies recurrence expansion, ETags, CRUD, Meet and
permission-dependent free/busy. Unit tests cover the write queue, recurring
scopes, conflicts, time zones, guest completion, immutable availability requests,
stale participants, snapped dragging, live duration changes, event-detail
pagination and account fallback, original-draft updates, permission preservation
and strict tools. Native
scripts in `e2e/calendar/` exercise filtering, month navigation, RSVP, editing,
availability and saving through real widget input paths. Live Google consent
and an actual account round trip require a configured desktop OAuth client.

Headless regression tests also send 10,000 pointer moves through the scheduling
widgets using an in-memory hit rectangle, without a window, renderer or event
loop. They check that only snap changes produce updates, movement reuses the
checked data, and dragging does not refresh the workspace or macOS menus.
The same tests cover empty and swept widget areas, and cache invalidation when
controls, participants, shared event details or result freshness change.
