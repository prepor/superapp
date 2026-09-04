//! Files' entries for the panels library.
//!
//! A row is a fixture on the app's own row template, populated through the
//! function the live table calls; a listing and a card are stages solo on
//! one identity, over a world whose disk is the demo tree — which is why
//! they take a fake outside where a mailbox takes none.

use kernel::caps::{Entry, HOME};
use kernel::scene::Scene;
use kernel::time::ts;
use makepad_widgets::{live_id, LiveId};

use crate::shell::app_ui::Setup;
use crate::shell::catalog::{panel_fake, widget};
use crate::shell::widgets::table::RowSpec;

use super::model::DirRow;
use super::widgets::dir::DirRows;
use super::{Card, Dir};

/// Files' scenes, in canvas order.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    vec![files_row(), files(), file_card()]
}

fn entry(name: &str, is_dir: bool, size: u64) -> Entry {
    Entry {
        name: name.to_string(),
        is_dir,
        size,
        modified: ts(2026, 8, 30, 22, 47),
    }
}

fn row_of(name: &str, is_dir: bool, size: u64) -> DirRow {
    DirRow {
        dir: HOME.to_string(),
        entry: entry(name, is_dir, size),
    }
}

/// One entry of a listing, in each of its states.
fn files_row() -> Scene<Setup> {
    let row = |r: DirRow, selected: bool, marked: bool| {
        widget(live_id!(files_row_tpl), move |cx, w| {
            DirRows::populate(cx, w, &r, selected, marked);
        })
    };
    Scene::new("files row", (520.0, 34.0))
        .note("One entry of a directory: the name, then the size and the day it changed, on the columns the header draws.")
        .note("A directory wears its slash and no size — it is not a number of bytes.")
        .node("file", row(row_of("notes.md", false, 2 * 1024 + 130), false, false))
        .node("directory", row(row_of("Downloads", true, 0), false, false))
        .about("the slash is the name's, and the size is a dash")
        .node("cursor", row(row_of("notes.md", false, 2 * 1024 + 130), true, false))
        .about("the wash under the cursor")
        .node("marked", row(row_of("notes.md", false, 2 * 1024 + 130), false, true))
        .about("a dark bar, for the batch verbs on the bar")
        .node("big", row(row_of("budget-2026.xlsx", false, 38 * 1024 * 1024), false, false))
        .about("sizes are spelled for a human")
        .edge("file", "cursor", "↓ / click")
        .edge("file", "marked", "space")
}

/// The listing itself, live: the walk, the crumbs, the filter, the marks.
fn files() -> Scene<Setup> {
    let home = |script: &str| panel_fake(|_| Dir::id(HOME), script);
    Scene::new("files", (560.0, 640.0))
        .note("A directory as a column: where the panel stands as crumbs, the filter, the rows, and the bar at the foot.")
        .note("The disk is the demo tree, so every node lists the same thing twice running.")
        .note("Live — enter a node and walk it: a row previews what it names, a crumb replaces the panel in place.")
        .node("home", home(""))
        .about("directories first, each with its size and the day it changed")
        .node("cursor", home("key down 2\nwait 500"))
        .about("the walk previews the listing or the card the row names")
        .node("filtered", home("key /\nwait 300\ntype \"note\"\nwait 500"))
        .about("the filter over the names")
        .node(
            "marked",
            home("key down\nwait 300\ntype \" \"\nwait 300\nkey shift+down\nwait 500"),
        )
        .about("two marks, and the verbs that act on both")
        .node("deeper", panel_fake(|_| Dir::id("~/Downloads"), ""))
        .about("another directory, the same panel — the crumbs say where")
        .edge("home", "cursor", "↓ ×2")
        .edge("home", "filtered", "/ note")
        .edge("home", "marked", "space")
        .edge("cursor", "deeper", "enter on a directory")
}

/// One file, shown on the shell's own card.
fn file_card() -> Scene<Setup> {
    Scene::new("file card", (520.0, 420.0))
        .note("One file: its name, what it is and how big, when it changed, where it lives — and the first of it, where there is anything to show.")
        .note("The reading is capped at 64 KiB and a picture at 20 MB; past either the card is its four lines, and `open` shows the rest.")
        .node("text", panel_fake(|_| Card::id("~/notes.md"), ""))
        .about("the preview is the file's own first bytes")
        .node("picture", panel_fake(|_| Card::id("~/Pictures/fold-cover.png"), ""))
        .about("a PNG or a JPEG, decoded from its bytes and drawn at the text's width")
        .node("no preview", panel_fake(|_| Card::id("~/Downloads/report-q3.pdf"), ""))
        .sized((520.0, 260.0))
        .about("a pdf is never read: the card alone, and it says so")
        .node("gone", panel_fake(|_| Card::id("~/nothing.txt"), ""))
        .sized((520.0, 260.0))
        .about("a file that is not there says so rather than reading as an empty one")
}
