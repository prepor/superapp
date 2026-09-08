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
mod dates;
mod edit;
mod model;
mod panels;
mod problems;
mod schema;
mod scoped;
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
        Some("Calendar uses shared account identities managed in Accounts. calendar_source holds Google calendar IDs, account IDs, access roles, time zones, Meet support and freshness. calendar_event holds expanded cached occurrences keyed by (source,remote); times are Unix seconds, all-day end dates are exclusive, series is the recurringEventId, raw preserves Google's complete event and ETag. Disconnected accounts and cancelled events are excluded. calendar_sync states the bounded cache coverage; absence from cache does not prove someone is free. calendar_draft is a persistent local form with revision, base snapshot and state; calendar_change is a durable queue (pending/processing/done/failed). Use calendar.draft and calendar.update_draft for local edits, calendar.commit for the exact reviewed revision, and calendar.operation for completion. Never write these tables directly to simulate a successful Google change. Attendee writes can send mail; commit/delete/respond/retry require approval. Google Meet creation is asynchronous: only a returned video entry point is a usable link. calendar_availability stores checked free/busy and per-calendar errors; unknown availability is not free and a suggested slot is not a reservation. Read-only and special Google event types retain their provider fields and may require the Google Calendar website for editing.")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
