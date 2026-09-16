//! KB: the knowledge base — pages an agent keeps, files a bucket keeps.
//!
//! Phase 0 of CR-022: the surface over a seeded wiki, so it can be judged
//! before the store's sync, the tools and the bucket exist. A page is a
//! row keyed by a random uid with its slug unique beside it; a file is a
//! path with a hash; every write of a page is a revision. Nothing here
//! replicates, offers a tool, or reaches a bucket yet.

use std::any::Any;

use kernel::app::{App, Mode, Root, Schema};
use kernel::panel::PanelKind;
use kernel::store::Store;

pub mod markdown;
pub mod model;
mod panels;
mod scenes;
mod schema;
mod seed;
#[cfg(test)]
mod tests;
mod ui;
mod widgets;

pub use panels::{Catalogue, Import};
pub use ui::UI;

pub struct Kb;
pub static KB: Kb = Kb;

impl App for Kb {
    fn id(&self) -> &'static str {
        "kb"
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
            Root::new(Catalogue::id(), "kb", "knowledge base wiki pages"),
            Root::new(Catalogue::ask(), "ask kb", "chat question knowledge base agent"),
            Root::new(Catalogue::filtered("@kind:skill"), "skills", "instructions procedures agent"),
            Root::new(Catalogue::filtered("@kind:memory"), "memory", "remembered facts agent"),
            Root::new(Import::id(), "kb import", "folder markdown wiki read in"),
        ]
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
