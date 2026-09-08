# Calendar UI draft

An interactive proposal for Calendar and shared Accounts, using superapp's
monochrome palette, Geist Mono, panel headers, joined previews, and action bars.
The strip above the panels switches review scenes; it is not proposed app chrome.

Open [index.html](./index.html) directly in a browser, or run from the repository:

```sh
python3 -m http.server 8765 --bind 127.0.0.1
```

Then visit <http://127.0.0.1:8765/docs/prototypes/calendar/>.
No build, package install, or internet connection is required.

## Review walk

1. **Timeline:** select an event, try *Needs reply* or *With Meet*, or type
   `@calendar:work`, `@account:personal`, `@with:nora`, or `@date:2026-09-09`.
   Free text searches titles, descriptions, locations, organizers, and guests.
   `/` focuses the filter; up/down walks the rows. The date range and calendar
   visibility controls also work.
2. **Month:** move between months, select a date for its agenda, or select an
   event directly. The September 12–13 trip shows a multi-day all-day event.
   The Studio all-hands illustrates a read-only source.
3. **New event:** enter a title, times, calendar, guests, location, notes, Meet,
   reminder, availability, and visibility. Saving changes the sample list.
   Edit an existing event, change a weekly series, delete, and undo from the toast.
4. **Find a time:** change the date and duration; select an opening and apply it
   to the draft. Nora and Leo share free/busy; Ava does not. Removing Ava from
   the draft lets you compare complete and partial availability.
5. **Accounts:** turn Calendar off for Work; Mail stays enabled. Return to
   Timeline to see its sources disappear. Re-enable it, or add/remove a sample
   Google connection.
6. **Agent:** open a prepared event draft from an example conversation. It uses
   the same editor as a manual event.

**Reset sample** restores the fixtures. The clock is fixed at 09:41 on
8 September 2026 in Europe/Berlin.

## Scope

This is a browser prototype for UI review, not the Makepad implementation.
It has no Google integration, database access, credentials, network calls,
real Meet destinations, or registered agent tools. State is held in memory
and resets on reload. Sign-in, sync, Meet joining, and invitation delivery
report that they are previews.

Weekly creation materializes three example occurrences. Recurring editing and
deletion operate on the seeded occurrences; they do not implement RRULEs or
Google series splitting. Reminders are editable values, not scheduled alerts.
Undo restores the most recent sample mutation; the native app will use its
existing history/effect machinery. The shell's complete workspace navigation,
restoration, accelerators, and draft persistence are outside this prototype.

The prose face uses the browser's system sans; native widgets should use the
app's shared IBM Plex prose style. The mono font comes from the repository.

The [design and API proposal](../../planning/cr-015-calendar.md) records the
intended native panels, shared-account transition, API constraints, agent
descriptions/tools, remaining states, and implementation sequence.

The native implementation now lives in `app/src/apps/calendar/` and `app/src/apps/accounts/`. It drops the “Next 30 days” preset and the checkbox strip from this original review prototype. See the Calendar and Accounts chapters in `docs/book/src/` for current behavior.
