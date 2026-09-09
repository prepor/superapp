# Calendar

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
until Google accepts it. Conflicts keep the draft and report the changed event;
ambiguous failures offer retry using the same operation identity. The Problems
panel also exposes failed changes and synchronization failures.

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

Select a suggested time, then **use this time** to update the original local
draft and return to its editor. Selecting a row alone does not change the draft.
Unknown calendars stay unknown; partial suggestions only cover readable
calendars, and no suggestions appear if none can be checked. A suggestion is
not a reservation.

Editing search controls does not alter an in-flight request. Each check creates
a new request using the draft's current guests and account. Changed settings,
changed participants or account, and results older than five minutes require
another check before applying a time. Search controls survive panel restoration.

Google's API supports both features. Reading another person's free/busy depends
on sharing and Workspace permissions; knowing an email address grants no access.
Requests are bounded to fourteen days and fifty calendars, with meeting
lengths from fifteen minutes to eight hours. See Google's
[conference creation guide](https://developers.google.com/workspace/calendar/api/guides/create-events),
[FreeBusy reference](https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query),
and [sharing model](https://developers.google.com/workspace/calendar/api/concepts/sharing).

## Persistence and synchronization

| Table | Contents |
|---|---|
| `calendar_source` | account, provider ID, permissions, zone, conference support, freshness |
| `calendar_event` | expanded occurrences and full raw Google event, keyed by source and remote ID |
| `calendar_draft` | revisioned local form, base snapshot, status, error |
| `calendar_change` | durable create/update/delete/RSVP operation and result |
| `calendar_sync` | requested cache range, refresh generations, last status |
| `calendar_availability` | queued free/busy query, per-calendar coverage, candidate times |

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
| `calendar.draft`, `calendar.update_draft` | create, read and edit a persistent local draft |
| `calendar.commit` | queue the exact reviewed draft revision |
| `calendar.delete`, `calendar.respond` | delete with explicit scope, or send RSVP |
| `calendar.operation`, `calendar.retry` | inspect completion and retry the original operation |
| `calendar.availability`, `calendar.availability_result` | check free/busy across connected identities and inspect coverage, account attempts, errors and times |

Commit, delete, RSVP, and retry require agent approval because they can notify
other people. Local draft edits do not send invitations. A draft revision or
ETag is mandatory on writes. Tools report queued work as queued, and direct SQL
writes are not a substitute for a successful Google operation.

## Native panels and verification

The registered tags are `calendar`, `calendar-month`, `calendar-event`,
`calendar-edit`, `calendar-availability`, and `calendar-sources`. Timeline and month
are launcher roots. The panel library includes native scenes.

The deterministic fake supplies recurrence expansion, ETags, CRUD, Meet and
permission-dependent free/busy. Unit tests cover the write queue, recurring
scopes, conflicts, time zones, guest completion, immutable availability requests,
stale participants, original-draft updates, permission preservation and strict tools. Native
scripts in `e2e/calendar/` exercise filtering, month navigation, RSVP, editing,
availability and saving through real widget input paths. Live Google consent
and an actual account round trip require a configured desktop OAuth client.
