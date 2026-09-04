//! The system app's entries for the panels library: the shell's own panels.
//!
//! They are here rather than in the shell's [`catalog`](crate::shell::catalog)
//! for the reason the whole app is here — the shell names no app, its own
//! included, and the catalogue asks the app list like it asks for anything
//! else.

use kernel::scene::Scene;

use crate::shell::app_ui::Setup;
use crate::shell::catalog::panel;

use super::{About, Effects, Help, Problems, Search};

/// The system app's scenes, in canvas order.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    vec![small_panels(), lists(), search()]
}

/// The manual and the colophon: the two panels a build always has.
fn small_panels() -> Scene<Setup> {
    Scene::new("small panels", (560.0, 760.0))
        .note("The manual and the colophon — the shell's own panels, drawn with the very widgets the grammar they describe is made of.")
        .node("help", panel(|_| Help::id(), ""))
        .about("the legend, the keys, and a bar whose links really go")
        .node("about", panel(|_| About::id(), ""))
        .sized((420.0, 220.0))
        .about("three lines and the way back")
        .edge("help", "about", "the colophon link")
}

/// The panel that asks: empty, answered, and narrowed to one source.
///
/// A node opens on the panel's own store, which is seeded like any other —
/// so what the rows show is what this build's sources really answer with,
/// found by really asking them.
fn search() -> Scene<Setup> {
    let ask = |script: &str| panel(|_| Search::id(), script);
    Scene::new("search", (560.0, 620.0))
        .note("One question, put to every app's own source at once. The words in the field are the question; the @ tags narrow the answer that comes back.")
        .note("Live — enter a node and type. The rows are a rich table like any other: the walk previews, enter opens, space marks.")
        .node("empty", ask(""))
        .about("nothing asked: the panel says so rather than showing a blank box")
        .node("found", ask("type \"vera\"\nwait 600"))
        .about("mail answers with the person who wrote before the letters they wrote")
        .node(
            "by source",
            ask("type \"vera @app:mail\"\nwait 400\nkey esc\nwait 400"),
        )
        .about("@app: keeps one source's rows — a filter over the answer, not a second question")
        .node("nothing", ask("type \"zzz\"\nwait 600"))
        .about("an answer of nothing says which question it was an answer to")
        .edge("empty", "found", "a word")
        .edge("found", "by source", "a tag")
}

/// The two lists the shell keeps about itself.
fn lists() -> Scene<Setup> {
    Scene::new("system lists", (600.0, 420.0))
        .note("Everything that left the process, and everything standing wrong: the shell's own two rich tables.")
        .note("Both are empty in a world nothing has happened in — and both say so rather than showing a blank box.")
        .node("effects", panel(|_| Effects::id(), ""))
        .about("the queue and the in-memory ring, joined, newest first")
        .node("problems", panel(|_| Problems::id(), ""))
        .about("nothing standing says so")
        .edge("effects", "problems", "the other list")
}
