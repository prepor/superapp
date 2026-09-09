//! Google Calendar: a future timeline, a month, shared event drafts and tools.
use kernel::{
    app::{App, Capabilities, Env, Mode, Root, Schema, Worker},
    panel::PanelKind,
    store::Store,
    tool::Tool,
};
use std::any::Any;
mod api;
mod availability;
mod availability_details;
mod availability_ui;
mod completion;
mod completion_ui;
mod dates;
mod edit;
mod model;
mod panels;
mod problems;
mod schema;
mod scoped;
mod snapshot;
mod seed;
mod sync;
#[cfg(test)]
mod tests;
mod tools;
mod ui;
mod widgets;
pub use ui::UI;
pub struct Calendar;
static PROBLEMS: &[&dyn kernel::app::ProblemSource] = &[&problems::PROBLEMS];
pub static CALENDAR: Calendar = Calendar;
impl App for Calendar {
    fn id(&self) -> &'static str {
        "calendar"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        panels::KINDS
    }
    fn schema(&self) -> Option<&'static Schema> {
        Some(&schema::SCHEMA)
    }
    fn seed(&self, s: &Store, m: Mode) -> rusqlite::Result<()> {
        seed::seed(s, m)
    }
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(
                panels::Timeline::id(),
                "calendar",
                "events agenda upcoming timeline Google Meet",
            ),
            Root::new(panels::Month::id("", ""), "month", "calendar dates"),
        ]
    }
    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {
        match mode {
            Mode::Deny => {}
            Mode::Real if !env.scripted && !env.clock.is_virtual() => {
                caps.insert::<dyn api::Api>(Box::new(api::Http::new(env)))
            }
            _ => {
                let fake = api::Fake::seeded(env.clock.read());
                caps.insert::<api::Fake>(Box::new(fake.clone()));
                caps.insert::<dyn api::Api>(Box::new(fake));
            }
        }
    }
    fn workers(&self, _: &Store) -> Vec<Box<dyn Worker>> {
        vec![Box::new(sync::Sync)]
    }
    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }
    fn problems(&self) -> &'static [&'static dyn kernel::app::ProblemSource] {
        PROBLEMS
    }
    fn describe(&self) -> Option<&'static str> {
        Some("Calendar uses shared account identities managed in Accounts. calendar_source holds Google calendar IDs, account IDs, access roles, time zones, Meet support and freshness. calendar_event holds expanded cached occurrences keyed by (source,remote); times are Unix seconds, all-day end dates are exclusive, series is the recurringEventId, raw preserves Google's complete event and ETag. Disconnected accounts and cancelled events are excluded. calendar_sync states the requested cache range and refresh generations; pending dates are not loaded yet, and absence from cache does not prove someone is free. Timeline scrolling automatically fetches later dates; month and day views fetch their displayed civil dates, including after restore. calendar.events also requests missing dates and returns loading=true with the current cache until synchronization finishes; query it again and inspect coverage for errors. calendar_draft is a persistent local form with revision, base snapshot and state; calendar_change is a durable queue (pending/processing/done/failed). Use calendar.create to add an event on an explicit writable source with form fields title, start, end, zone and notify. Use calendar.update with the event ID, current ETag from calendar.event, and changes including explicit scope (this/all/following) and notify; omitted fields are preserved. Use calendar.delete with the event ID, current ETag, scope and notify to remove events. Direct create/update return draft and queued operation IDs; inspect calendar.operation until done or failed. For a separate local review step, use calendar.draft and calendar.update_draft, then calendar.commit for the exact reviewed revision. All submissions share the editor validation and durable queue, retaining failed drafts for recovery. calendar.suggest returns the editor’s cached guest/name, location, time zone, recurrence, reminder and duration suggestions; it performs no external contact lookup. Never write these tables directly to simulate a successful Google change. Attendee writes can send mail; create/update/commit/delete/respond/retry require approval. Google Meet creation is asynchronous: only a returned video entry point is a usable link. calendar_availability stores checked free/busy, per-calendar errors and each account attempt; unreadable guests are retried through the other connected Calendar accounts, with the successful identity shown in the UI and results; unknown availability is not free and a suggested slot is not a reservation. Find a time shows participant tracks with a proposal that drags in 15-minute steps; busy conflicts are highlighted before applying to the original local draft. Hover busy blocks for shared event titles, exact times and locations, loaded separately through connected accounts; unavailable details stay Busy and never change free/busy coverage. calendar.availability_result exposes per-person details state and events; optional minutes recalculates candidates from the same fresh result. calendar.use_time applies a snapped start and duration to the original local draft and returns conflicts and unknown calendars; calendar.commit separately submits it to Google. Search controls are separate from immutable requests. Duration changes immediately resize the proposal and recalculate suggestions without renewing freshness; changed dates, hours, zone, guests or account require another check, as do results older than five minutes. Read-only and special Google event types retain their provider fields and may require the Google Calendar website for editing.")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
