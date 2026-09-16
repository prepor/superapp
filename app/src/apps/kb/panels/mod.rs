//! The KB's panels: the catalogue, a page, its editor, its history and one
//! revision, a file's card, and the import form. The kernel half of each
//! tag; the widgets that draw them are beside.

use kernel::layout::SlotId;
use kernel::panel::PanelKind;
use kernel::session::Session;

mod catalogue;
mod edit;
mod file;
mod history;
mod import;
mod page;

pub use catalogue::Catalogue;
pub use edit::Edit;
pub use file::File;
pub use history::{History, Revision};
pub use import::Import;
pub use page::Page;

pub static KINDS: &[&dyn PanelKind] = &[
    &catalogue::CatalogueKind,
    &page::PageKind,
    &edit::EditKind,
    &history::HistoryKind,
    &history::RevisionKind,
    &file::FileKind,
    &import::ImportKind,
];

/// **ask**: the panel offered to the apps as context — the road
/// `shift+cmd+a` takes — so a chat joined to it opens with its chip. A
/// build with no app that takes a panel says so.
pub(super) fn ask(s: &mut Session, slot: SlotId) {
    let taker = s.apps().list().iter().copied().find(|a| a.id() == "agent");
    match taker {
        Some(app) if app.ask(s, slot) => {}
        _ => s.notify("no app in this build takes a panel as context", true),
    }
}

/// A chat another panel starts with its first turn written, carrying the
/// panel as a chip: the catalogue's **lint** and a skill's **use**.
pub(super) fn start_chat(s: &mut Session, slot: SlotId, text: &str) {
    use crate::apps::agent::{chip::Chip, Agent};
    let Some(agent) = s.apps().get_as::<Agent>() else {
        s.notify("no agent app in this build, so there is no chat to open", true);
        return;
    };
    let chips = Chip::panel(s, slot).into_iter().collect();
    agent.start(s, slot, text, chips, |s, chat| {
        if chat.is_none() {
            s.notify("the chat could not be started", true);
        }
    });
}
