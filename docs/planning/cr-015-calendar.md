# CR-015 · Calendar and shared accounts

> Native implementation: see [Calendar](../book/src/calendar.md) and [Accounts](../book/src/accounts.md). The accepted implementation drops the “Next 30 days” preset and checkbox strip from the original prototype. The proposal below records the design discussion; the book describes current behavior.

Status: **UI proposal for review**, 2026-09-08.

[Open the interactive draft](../prototypes/calendar/index.html).
[Run instructions and review walk](../prototypes/calendar/README.md).

The requested first step is a UI draft. This change adds a browser prototype
and this proposal; it does not register a native app, move accounts, or connect
to Google. The existing book continues to describe implemented behavior.

## The shape

Calendar starts with what is coming up: a date-grouped timeline across selected
Google calendars. The month is a second presentation of those sources. A row
previews an event beside the list, and the event's edit view is a draft. Finding
a time opens another joined panel, keeping the draft available while comparing
participants.

Accounts becomes a shared app in the launcher. Mail and Calendar both lead to
it. A connection represents a person at a provider; Mail and Calendar are the
services enabled on that connection. Calendars are individual sources within
an enabled service. Signing in, enabling Calendar, and hiding a calendar are
three different operations, with separate controls.

This follows the [panel model](../book/src/panel-model.md) and
[interaction grammar](../book/src/interaction-grammar.md). Panel headers hold
only the title and close button. Navigation and actions live in the body or
bottom bar. Calendar identity uses names and monochrome marks, so no event's
meaning depends on a color. The prototype's numbered review tabs sit outside
the proposed app.

## Panels

These are proposed tags; the native registry will enforce uniqueness.

| Panel | Identity / starting size | Body and actions |
|---|---|---|
| Timeline | `calendar`, filter and range; 5×6 | Date groups, events, account/calendar filters. Sync; new event; replace with month; calendars; accounts. |
| Month | `calendar_month`, month and filter; 7×6 | Monday-first grid, today, previous/next month. Dates open a day timeline; event entries preview details. |
| Day | `calendar_day`, date and filter; 4×6 | All matching events overlapping that date, including all-day and multi-day events. New event starts on this date. |
| Event | `calendar_event`, connection, calendar, event, occurrence; 4×6 | Time, calendar/account, location, Meet, guests/RSVP, description, recurrence, reminders, permissions. |
| Draft | `calendar_draft`, stable draft ID; 4×6 | Create or edit; save; cancel; find a time. Existing series require explicit edit scope. |
| Find a time | `calendar_availability`, draft ID; 6×6 | Participants, date range, duration, time zone, working-hour window, free/busy, proposed slots. Apply a slot to the draft. |
| Calendars | `calendar_sources`, view context; 3×6 | Sources grouped by account, read/write status, selection checkboxes, sync status, link to Accounts. |
| Accounts | `accounts`; 5×6 | Shared connection list with per-service status. Add account. |
| Connection | `account_connection`, connection ID; 4×6 | Identity, enabled services, reconnect, service settings, remove connection. |
| Connect | `account_connect`; 4×6 | Provider and desired services; Google browser sign-in, or existing Mail account setup. |

Launcher roots: **calendar**, **calendar month**, **new event**, **accounts**.
The timeline remains the default Calendar root. New roots should not displace
Mail as the initial app on a fresh store.

Native joined opens retain their parent and replace that parent's joined
child. On narrow screens, the shell follows the new panel horizontally.
The prototype simulates the important list/detail and draft/availability
arrangements; review scene buttons jump directly between arrangements.

## Timeline and month

The initial range is the next 30 days, with a shorter seven-day option and a
paginated all-upcoming range. Today includes events still in progress.
All-day events form the first rows for a date. Each timed row shows start/end,
title, source, Meet or location, recurrence, and exceptional state such as
needs-reply or overlap.

Use the existing filter grammar and completion widgets. Proposed tags:

| Filter | Meaning |
|---|---|
| free text | Title, description, location, organizer, participant names/addresses |
| `@calendar:` | One calendar, disambiguated by account when names collide |
| `@account:` | All selected calendars of a connection |
| `@with:` | Organizer or participant |
| `@date:` | Event overlaps the selected local date |
| `@rsvp:needs-action` | An invitation awaiting this account's reply |
| `@meet` | Has Google Meet conferencing |

Month and timeline share the current filter and source selection. The timeline's
future range does not prevent browsing an earlier month. Filtering changes what
is visible; it does not change availability or RSVP. The native implementation
should show marked-but-hidden events using the existing table convention.
Batch writes should be added only with the same permission and recurrence-scope
rules as an individual write.

A multi-day all-day event appears on every date it covers in Month and Day.
The main upcoming timeline can show it once with the end date. All-day values
stay dates; a timed event retains its own time zone and displays in the viewing
zone. DST must be resolved using an IANA time-zone library in the native app.

The same meeting received through two Google accounts must retain its
per-account copy and response. A visual grouping may collect matching iCalUIDs
and occurrence times, but must expose those copies before edit or RSVP.
An iCalUID alone is not a unique occurrence or writable account choice.

## Event lifecycle

The draft exposes title, target writable calendar, start/end dates and times,
all-day, IANA time zone, recurrence, guests, Meet, location, description,
free/busy, visibility, and reminders. Guest notification is an explicit choice;
the commit verb says **save & notify guests** when applicable.

The native version should support daily/weekly/monthly/yearly and custom
recurrence, an end date or occurrence count, multiple reminders, optional
guests, and guest permissions. The first draft demonstrates weekly recurrence
and a single reminder; these are review scope, not the full feature boundary.
Google-specific status types, rooms/resources, attachments, and moving an
existing meeting across calendars need their own capability checks and UI.

Drafts persist as the person types, using the app's existing stable slot/draft
pattern. Discard leaves the server event unchanged. A failed save leaves the
draft and its cause visible. The submitted write has a durable operation ID
and is visibly pending until the server acknowledges it.

An organizer can edit and cancel. A guest can RSVP and remove their own copy
where supported; those actions must never cancel the organizer's meeting.
The prototype shows RSVP and read-only events. Native edit permissions should
use effective calendar access plus event flags, including
`guestsCanModify`; organizer identity alone is insufficient.

Editing or deleting a recurring event offers **this event**, **this and
following**, or **all events**. Single occurrences use their instance identity.
All-series edits operate on the master. Google's “following” operation splits
the series with two requests and can reset later exceptions; represent both
steps durably and explain affected exceptions before commit. Do not rewrite
every instance to simulate a master edit.
[Google recurring events](https://developers.google.com/workspace/calendar/api/guides/recurringevents).

New creates and writes use the same action/effect code path from UI and tools.
Use a stable client-chosen event ID for idempotent creation. Read the current
event and its ETag before update; surface a concurrent change instead of
silently overwriting it. Preserve fields this editor does not expose, including
existing conference data and attendee state.
[Event creation](https://developers.google.com/workspace/calendar/api/guides/create-events),
[event updates](https://developers.google.com/workspace/calendar/api/v3/reference/events/update).

Undo can retract an unsent operation. Once a cancellation or invitation has
left the process, it cannot unnotify guests; use the existing history-expiry
mechanism and expose a new compensating change where possible. The browser
prototype's one-step sample undo does not model external effects.

## Google Meet and finding a time

**Meet is supported by the Calendar API.** Check the calendar's allowed
conference solutions. Request a new conference through
`conferenceData.createRequest`, a unique request ID, and
`conferenceDataVersion=1`. Creation is asynchronous: show pending, then the
returned link, or a failure with retry. Preserve an existing conference on edit;
duplicating a meeting should request a fresh conference. Do not invent a Meet
URL or require a separate Meet API for ordinary event links.
[Google conference creation](https://developers.google.com/workspace/calendar/api/guides/create-events).

**Participant availability is supported, subject to access.**
`freebusy.query` returns busy intervals for calendars the signed-in account
can access. It does not grant access merely because an email was entered.
Sharing or Workspace policy can expose busy blocks while hiding event details.
Unknown, denied, failed, and stale results must remain distinct from an empty
busy list.
[Free/busy API](https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query),
[calendar sharing](https://developers.google.com/workspace/calendar/api/concepts/sharing).

The app computes suggestions by merging busy intervals and subtracting them
from the requested date/hour window, then selecting intervals at least as long
as the desired duration. Include the user's own connected personal calendars
even when hidden from the current timeline. Do not include every subscribed
team calendar as if every team event were the user's commitment.

Show each participant as shared, unknown/not shared, or failed. The phrase
**works for everyone** is permitted only when all participants have fresh,
successful availability for the whole requested range. With partial coverage,
say **works for checked calendars; check with Ava**. Event-level availability
does not establish someone's working hours or preferences; the initial window
is the organizer's explicit choice, and future per-person preferences belong
in the scheduling form.

Query in batches within the API's limit (50 calendars per expansion), retain
per-calendar errors, and recheck the chosen window before a live save. Google
does not reserve a slot during a free/busy query, so an overlap can still appear
after that check.
[Free/busy limits and response](https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query).

## Shared Google accounts

Currently Mail owns the `account` table, the `settings` and `add_account`
panels, OAuth, Google client loading, and keychain refresh keys
(`oauth:{email}`). Google sign-in asks for IMAP/SMTP mail access and identity,
and Mail checks the returned mail scope. Simply adding Calendar scopes to that
flow would leave Calendar dependent on Mail and exclude a Calendar-only account.

Proposed ownership:

| Owner | Responsibility |
|---|---|
| Shared identity module | Provider/subject identity, OAuth client/PKCE loopback flow, scope checks, serialized token refresh, keychain access |
| Accounts app | Connection list, enabled services, connect/reconnect/disconnect, account-level problems and agent description |
| Mail app | IMAP/SMTP configuration, mailbox rows/workers and mail-specific status, referencing a shared connection |
| Calendar app | Calendars, events, drafts, instances, sync/write effects, availability, and calendar-specific status |

Put shared identity contracts in an app-level common module, as the existing
reader is shared by Mail and RSS. Keep Google and app names out of the kernel,
shell, and platform. Avoid importing Mail internals from Calendar.

Use provider plus the verified Google subject as the stable identity; email is
display and lookup information. Keep existing Mail account IDs, mailbox rows,
and refresh-key compatibility during the transition. Link existing Google Mail
accounts to shared connections without reconnecting or deleting their mail.
Where an old grant has no stored subject, resolve it through a verified
provider response before consolidating identities.

The shared model should distinguish requested/enabled services from actually
granted scopes and each service's latest error. A reconnect with missing
Calendar permission must leave usable Mail access intact. Store no access or
refresh token in SQL, panel context, logs, or agent tools.

The Desktop OAuth client needs care: Google's installed-app flow does not
support incremental authorization. When a user enables another service,
reauthorize for the combined scopes needed by enabled services, validate the
returned scopes, and retain the existing usable grant until the replacement
is valid. Do not assume a Calendar-only authorization automatically preserves
the mail grant, or overwrite a refresh token with an absent value.
[Google native OAuth](https://developers.google.com/identity/protocols/oauth2/native-app).

Proposed Calendar permissions are `calendar.calendarlist.readonly`,
`calendar.events`, and `calendar.events.freebusy`. Read settings only if needed
for calendar preferences. Continue using `https://mail.google.com/` only for
the enabled IMAP/SMTP service. Gate each Calendar operation on effective
calendar access as well as OAuth scopes.
[Calendar scopes](https://developers.google.com/workspace/calendar/api/auth).

Disabling Calendar is a local service choice: stop its workers and stop
offering its sources. It does not revoke the entire Google grant used by Mail.
Removing a connection is separate, names every affected app, and leaves remote
mail/events untouched. Resolve pending drafts and writes before removal.
Non-Google IMAP/SMTP setup remains supported through Mail's existing capability.

Keep persisted `settings` and `add_account` tags working through compatibility
panels; their stored argument meanings cannot be silently changed. Replace
Mail's generic launcher “settings” root with the shared Accounts root and keep
mail-specific configuration accessible from the connection.

## Agent descriptions, context, and tools

Calendar and Accounts implement `App::describe`, `App::tools`,
`Panel::about`, and the existing contextual query tracing. The description is
a data dictionary, not a promise that a tool bypasses the normal event flow.

Proposed Calendar description:

> Calendar stores calendars under shared account connections, cached server
> events and recurring instances, local drafts, and pending changes. A calendar
> ID is scoped to a connection; an event is scoped to that calendar. Recurring
> masters and their occurrences are different identities. Timed events retain
> their IANA time zone; all-day start/end are dates with an exclusive end in
> the provider representation. Server snapshots, sync tokens, ETags, conference
> state, and operation status are maintained by sync, not direct SQL writes.
> Use Calendar tools for changes. Availability results include coverage,
> retrieval time, and per-calendar errors; missing availability never means
> free. A draft is local and does not invite anybody. Committing a draft can
> create an event and notify guests.

Proposed Accounts description:

> Accounts stores provider identities and service access shared by Mail and
> Calendar. A service being enabled does not imply all requested scopes were
> granted. A calendar visibility checkbox is not an account disconnect.
> Credentials are held outside the database and are never returned by tools.
> Query public connection/service metadata to select the correct account.
> Connection changes go through the account UI and its ordinary actions.

Tool names and contracts to implement after UI review:

| Tool | Inputs and result | Mutation / gate |
|---|---|---|
| `accounts.list` | Optional service; connections, enabled/granted services, and health; never secrets | Read |
| `calendar.calendars` | Optional connection; stable source identities, access role, time zone, and conference support | Read |
| `calendar.events` | Explicit range/zone, source identities, filter, cursor; events/occurrences and next cursor | Read |
| `calendar.event` | Connection + calendar + event/occurrence identity; complete allowed event details | Read |
| `calendar.availability` | Querying connection, participant calendars, range, duration, zone, working-hour window; intervals, suggestions, freshness and coverage | External read |
| `calendar.draft` | New fields or existing identity + explicit recurrence scope; persisted draft ID and opened draft panel | Local, undoable |
| `calendar.update_draft` | Draft ID and changed fields; revised draft | Local, undoable |
| `calendar.commit` | Existing reviewed draft ID and revision; durable operation ID, then provider result | Writes; asks before external commit |
| `calendar.delete` | Exact identity, recurrence scope, and guest notification policy; cancellation/removal operation ID | Writes; asks |
| `calendar.respond` | Exact attendee copy and accepted/tentative/declined; response operation ID | Writes; asks because it sends a response |

All schemas are strict objects with `additionalProperties: false`. Require
explicit source identity for writes; “primary” is resolved within a named
connection. Reject writes to a read-only source. A stale draft revision cannot
be committed using approval for earlier contents.

The tool gate follows the agent app's existing `writes` / `asks` distinction.
Tools prepare drafts without asking; externally visible event commits,
cancellations, and responses are concrete reviewable actions. Use the same
UI action handlers, store writes, history entries, and durable effects from
both buttons and tools. These are proposed in-app agent policies, not an
approval request for building this prototype.

A timeline context includes filters, range, display zone, selected sources,
visible occurrence identities, sync freshness, and truncation. Event context
includes organizer, account copy, effective rights, series/instance identity,
local draft/pending state, and actual conference status. Availability context
includes the exact queried range and whose calendars were inaccessible.
An account context includes only safe identity and service metadata.

## Remaining UI states for the native catalogue

| State | Expected behavior |
|---|---|
| No connected Calendar service | Explain the empty state and open Accounts to enable Calendar |
| Loading / first sync | Identify the account being read; keep other accounts usable |
| Offline or stale | Keep cached events, show last successful sync; distinguish queued writes |
| Permission missing / revoked | Account-specific problem with reconnect; other services remain available where their grants still work |
| Save conflict | Keep draft, show changed server version, offer reload/review |
| Meet pending or failed | Show status, preserve event, retry conference creation without duplicate events |
| Partial free/busy failure | Mark affected participants unknown; suggest only against successful coverage |
| No shared opening | Change date range, duration, hours, or participants |
| Unsupported event type / private event | Show permitted details and restrict edits according to capabilities |
| Series with exceptions | Explain affected scope before a series split or cancellation |
| Cross-account duplicate | Expose account copies and select one for RSVP/edit |
| Pending operation on disconnect | Resolve or discard pending work explicitly before stopping the service |

The prototype exercises empty/filtered lists, read-only calendars, overlaps,
needs-reply, multi-day events, recurrence scope, and unknown availability.
The remaining states above belong in Makepad fixtures before live integration.

## Implementation sequence after review

1. Agree the panel arrangements, timeline density, event fields, and shared
   Accounts model using this draft.
2. Introduce the shared identity module and Accounts app while preserving Mail
   IDs, tags, grants, and existing mail/e2e behavior.
3. Register Calendar panels with native widgets, fixtures, catalogue scenes,
   data dictionary, read tools, and panel context.
4. Add calendar/event sync, occurrence handling, permission-aware cached reads,
   and problem sources. Test pagination, stale sync-token recovery, cancellation,
   and two accounts with overlapping identities.
5. Add durable create/edit/delete/RSVP and Meet effects with concurrent-edit
   handling; connect draft and commit tools to those same actions.
6. Add free/busy queries and scheduling, including partial failure and stale
   data tests. Recheck at commit.
7. Run the workspace unit suite, clippy, boundaries, and headless e2e suites.
   Add targeted Calendar and Accounts journeys, including narrow panels and
   agent-created draft review.
