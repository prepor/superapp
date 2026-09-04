//! Mail's entries for the panels library.
//!
//! A row is a fixture on the app's own row template, populated through the
//! very function the table calls for a live row; a panel is a stage solo on
//! one identity, over a store of its own with the demo rows in it, replaying
//! a script to reach its state. Both are built out of mail's own types, so a
//! change that would break one of these scenes breaks the build instead of
//! the picture.

use kernel::scene::Scene;
use kernel::store::Store;
use kernel::time::ts;
use makepad_widgets::{live_id, LiveId};

use crate::shell::app_ui::Setup;
use crate::shell::catalog::{panel, widget, workspace_on};
use crate::shell::widgets::table::RowSpec;

use super::model::{Role, Seed, ThreadHead};
use super::panels::{AddAccount, Compose, Contact, Mailbox, Message, Settings};
use super::widgets::mailbox::MailboxRows;

/// Mail's scenes, in canvas order.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    vec![
        inbox_row(),
        mailbox(),
        message(),
        compose(),
        contact(),
        accounts(),
    ]
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn at(h: u32, min: u32) -> f64 {
    ts(2026, 8, 30, h, min)
}

fn head(who: &[&str], topic: &str, unread: bool, n: i64) -> ThreadHead {
    ThreadHead {
        thread: 1,
        target: 1,
        who: who.iter().map(|s| (*s).to_string()).collect(),
        topic: topic.to_string(),
        last: at(9, 12),
        unread,
        n,
    }
}

/// The newest seeded mail whose subject contains `pat` — how a panel node
/// names the one it opens on, without hard-coding a row id the seed is free
/// to renumber.
fn mail_like(store: &Store, pat: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT id FROM message WHERE subject LIKE ?1 ORDER BY date DESC, id DESC LIMIT 1",
            [format!("%{pat}%")],
            |r| r.get(0),
        )
        .unwrap_or(1)
}

// ---------------------------------------------------------------------------
// The scenes
// ---------------------------------------------------------------------------

/// One conversation as a mailbox lists it, in each of its states. Populated
/// by [`MailboxRows::populate`] — the function the live table calls — so the
/// row cannot drift from the list it belongs to.
fn inbox_row() -> Scene<Setup> {
    let row = |t: ThreadHead, selected: bool, marked: bool| {
        widget(live_id!(mail_row_tpl), move |cx, w| {
            MailboxRows::populate(cx, w, &t, selected, marked);
        })
    };
    let elena = || head(&["Elena Petrova"], "Sat hike — early start?", false, 1);
    let long = "[stelaxis] CI failed on main — workflow main #4128 failed on push 9f3c2a1, the full logs attached to the run";
    Scene::new("inbox row", (520.0, 56.0))
        .note("One conversation as the inbox lists it: who wrote and when, the topic on its own line.")
        .note("Unread rows are bold. The cursor adds a wash. Marked rows wear a dark bar.")
        .node("read", row(elena(), false, false))
        .node(
            "unread",
            row(head(&["Elena Petrova"], "Sat hike — early start?", true, 1), false, false),
        )
        .about("the whole row bold, not a dot")
        .node("cursor", row(elena(), true, false))
        .about("the wash under the cursor; focus stays in the list")
        .node("marked", row(elena(), false, true))
        .about("a dark bar marks the row without changing its size")
        .node(
            "marked, cursor",
            row(head(&["Elena Petrova"], "Sat hike — early start?", true, 1), true, true),
        )
        .about("wash and mark together; bold still means unread")
        .node(
            "conversation",
            row(head(&["me", "Vera", "Max"], "Q3 infra budget draft", true, 4), false, false),
        )
        .about("first names once there are two, then the count")
        .node("long topic", row(head(&["GitHub"], long, false, 1), false, false))
        .about("one line each, shortened to fit")
        .node(
            "narrow",
            row(head(&["me", "Vera", "Max"], "Q3 infra budget draft", true, 4), false, false),
        )
        .sized((320.0, 56.0))
        .about("the phone's width")
        .edge("read", "unread", "a reply arrives")
        .edge("read", "cursor", "↓ / click")
        .edge("read", "marked", "space")
        .edge("cursor", "marked, cursor", "space")
}

/// The list itself, live: the walk, the filter, and the marks.
fn mailbox() -> Scene<Setup> {
    let inbox = |script: &str| panel(|_| Mailbox::id(Role::Inbox), script);
    Scene::new("mailbox", (520.0, 640.0))
        .note("The mail list: a rich table over the conversations, the filter above it, the bar at the foot.")
        .note("One panel over two folders — the inbox and the archive. Same rows, same walk, same grammar in the filter.")
        .note("Live — enter a node and walk it; cmd+a archives, cmd+z takes it back.")
        .node("fresh", inbox(""))
        .node("cursor", inbox("key down 3\nwait 500"))
        .about("the walk previews; the list keeps the keyboard")
        .node("filtered", inbox("key /\nwait 300\ntype \"github\"\nwait 500"))
        .about("what the filter shows, as it is typed")
        .node(
            "marked",
            inbox("key down\nwait 300\ntype \" \"\nwait 300\nkey shift+down 2\nwait 500"),
        )
        .about("space marks the cursor's row, shift+↓ the two under it; the bar grows the batch verbs")
        .node("archive", panel(|_| Mailbox::id(Role::Archive), ""))
        .about("what was filed away — the same rows, another folder")
        .node(
            "joined",
            workspace_on(|_| Mailbox::id(Role::Inbox), "key down\nwait 700"),
        )
        .sized((1200.0, 700.0))
        .about("not solo: the reader the walk previews, joined to the right of the list that drives it")
        .edge("fresh", "cursor", "↓ ×3")
        .edge("fresh", "filtered", "/ github")
        .edge("cursor", "marked", "space")
        .edge("fresh", "archive", "cmd+a / sync")
        .edge("cursor", "joined", "the same walk, in a workspace")
}

/// A conversation as a page.
fn message() -> Scene<Setup> {
    Scene::new("message", (560.0, 640.0))
        .note("A conversation as a page: every message of it, the one it opened on and the unread ones open, the rest collapsed to their header lines.")
        .node(
            "thread",
            panel(|s| Message::id(mail_like(s, "[stelaxis] CI")), ""),
        )
        .about("the CI thread: several runs, one of them failed")
        .node("single", panel(|s| Message::id(mail_like(s, "Sat hike")), ""))
        .about("one mail is a thread of one")
}

/// The sheet a letter is written in.
fn compose() -> Scene<Setup> {
    Scene::new("compose", (560.0, 420.0))
        .note("A reply: TO and SUBJECT from the mail it answers, its letter quoted under the attribution line, the cursor in the body above both.")
        .note("Send is a side effect with an undo window; discard closes the sheet.")
        .node(
            "reply",
            panel(|s| Compose::id(Seed::Reply(mail_like(s, "Q3 infra"))), ""),
        )
        .node(
            "written",
            panel(
                |s| Compose::id(Seed::Reply(mail_like(s, "Q3 infra"))),
                "type \"Numbers check out — egress line is stale, I will redo it.\"\nwait 500",
            ),
        )
        .about("what is typed lands in the letter and nowhere else")
        .node("blank", panel(|_| Compose::id(Seed::Blank), ""))
        .about("from the launcher's root: nothing prefilled")
        .edge("reply", "written", "type in the body")
}

/// A correspondent's card.
fn contact() -> Scene<Setup> {
    Scene::new("contact", (420.0, 220.0))
        .note("One sender: their name as of their latest letter, their address, how much they have written.")
        .note("The one link off it opens the inbox filtered to that address — the same navigation the bar carries.")
        .node("card", panel(|_| Contact::id("max@ivanov.dev"), ""))
        .node("stranger", panel(|_| Contact::id("nobody@example.org"), ""))
        .about("an address nobody has written from still opens, and says as much")
        .node(
            "letters",
            workspace_on(|_| Contact::id("max@ivanov.dev"), "click \"messages from\"\nwait 700"),
        )
        .sized((1200.0, 700.0))
        .about("the link followed: the inbox, filtered, its title carrying the filter")
        .edge("card", "letters", "messages from max")
}

/// The accounts, and the form that adds one. Settings are not a shell panel:
/// what a person configures belongs to the app it configures.
fn accounts() -> Scene<Setup> {
    Scene::new("accounts", (560.0, 420.0))
        .note("Mail's own settings: the accounts it syncs, the host each reads from, and what the last pass said.")
        .note("The address, the host and the status line are selectable runs — a sync error is the one line here a human needs to carry somewhere else.")
        .node("settings", panel(|_| Settings::id(), ""))
        .node("form", panel(|_| AddAccount::id(), ""))
        .about("four fields for an app password, and the one button that is not one")
        .node(
            "google refused",
            panel(|_| AddAccount::id(), "click \"sign in with google\"\nwait 600"),
        )
        .about("a mount never leaves for a browser, and the line says so")
        .node(
            "joined",
            workspace_on(|_| Settings::id(), "click \"add account\"\nwait 700"),
        )
        .sized((1200.0, 700.0))
        .about("the form joined to the right of the accounts it adds to")
        .edge("settings", "joined", "add account")
        .edge("form", "google refused", "sign in with google")
}
