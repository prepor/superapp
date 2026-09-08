//! RSS subscriptions and an oldest-first reading queue.

use kernel::app::{App, Capabilities, Env, Mode, Root, Schema, Worker};
use kernel::panel::PanelKind;
use kernel::store::Store;
use std::any::Any;

pub mod model;
mod opml;
pub mod panels;
pub mod parse;
mod schema;
mod seed;
mod sync;
#[cfg(test)]
mod tests;
mod ui;
mod widgets;

pub use ui::UI;
pub struct Rss;
pub static RSS: Rss = Rss;

impl App for Rss {
    fn id(&self) -> &'static str {
        "rss"
    }
    fn kinds(&self) -> &'static [&'static dyn PanelKind] {
        panels::KINDS
    }
    fn schema(&self) -> Option<&'static Schema> {
        Some(&schema::SCHEMA)
    }
    fn seed(&self, store: &Store, mode: Mode) -> rusqlite::Result<()> {
        seed::seed(store, mode)
    }
    fn roots(&self) -> Vec<Root> {
        vec![
            Root::new(panels::Articles::id(), "rss", "reader articles news unseen"),
            Root::new(panels::Feeds::id(), "feeds", "rss subscriptions add remove"),
        ]
    }
    fn outside(&self, mode: Mode, env: &Env, caps: &mut Capabilities) {
        match mode {
            Mode::Deny => {}
            Mode::Real if !env.scripted && !env.clock.is_virtual() => {
                caps.insert::<dyn sync::Fetch>(Box::new(sync::Http::default()))
            }
            _ => caps.insert::<dyn sync::Fetch>(Box::new(sync::Fake)),
        }
    }
    fn workers(&self, store: &Store) -> Vec<Box<dyn Worker>> {
        sync::workers(store)
    }
    fn describe(&self) -> Option<&'static str> {
        Some("rss_feed: subscriptions, URLs, titles and refresh status. subscribed=0 retains a removed feed for undo. \
         rss_article: cached entries keyed by (feed,guid), HTML reading, original content in raw with content_type and base_url, publication date and seen flag. HTML is derived from raw; legacy entries have no raw until refreshed. \
         Lists include only subscribed feeds. Articles default to @unseen and sort oldest first.")
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
