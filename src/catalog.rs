//! The catalogue: what the panels library shows (CR-006).
//!
//! One function per subject, each a [`Scene`] of the states worth a look
//! while that subject is being worked on. A node comes up one of three
//! ways:
//!
//! - a **component**: a bare widget from the library's template, populated
//!   through its own API with a fixture — no store, no clock, a texture
//!   the size of the piece;
//! - a **panel**: one panel widget on a world of its own (an in-memory
//!   store with the demo seed, a sealed outside, virtual time), chrome
//!   included, so entering it gives the keys, the archive, the undo;
//! - the **workspace**: the whole stage, kept for the shell's own subjects
//!   — joins, tabs, the phone grid.
//!
//! A panel or workspace node may name a few steps in the harness's grammar
//! that lead to its state; a component's state is its fixture. Fixtures
//! are the real structs and states use the widgets' own methods, so a
//! refactor that breaks a scene fails to compile.

use std::rc::Rc;

use makepad_widgets::*;

use crate::app::BootOutside;
use crate::core::{Grid, Kind, Seed};
use crate::e2e::{self, Step};
use crate::mail;
use crate::panels::*;
use crate::scene::Scene;
use crate::store::Store;

/// Sets a component's state through its own API, once, when it mounts.
pub type Populate = Rc<dyn Fn(&mut Cx, &WidgetRef)>;
/// The kind a solo stage opens on, resolved against its seeded store.
pub type Open = Rc<dyn Fn(&Store) -> Kind>;

/// How a node comes up.
pub enum Setup {
    /// A bare widget from the library's template `tpl`, populated once
    /// when it is mounted. `overlay` rides the scope for the sheets, whose
    /// rows come from their props on every draw.
    Widget {
        tpl: LiveId,
        populate: Populate,
        overlay: Option<OverlayProps>,
    },
    /// A stage on a world of its own: solo on the one panel `open` names,
    /// or the whole workspace. `steps` lead to the state.
    Stage {
        open: Option<Open>,
        steps: Option<Vec<Step>>,
        grid: Option<Grid>,
        outside: BootOutside,
    },
}

fn widget(tpl: LiveId, f: impl Fn(&mut Cx, &WidgetRef) + 'static) -> Setup {
    Setup::Widget {
        tpl,
        populate: Rc::new(f),
        overlay: None,
    }
}

fn sheet(tpl: LiveId, props: OverlayProps, f: impl Fn(&mut Cx, &WidgetRef) + 'static) -> Setup {
    Setup::Widget {
        tpl,
        populate: Rc::new(f),
        overlay: Some(props),
    }
}

fn panel(open: impl Fn(&Store) -> Kind + 'static, script: &str) -> Setup {
    Setup::Stage {
        open: Some(Rc::new(open)),
        steps: steps(script),
        grid: None,
        outside: BootOutside::Deny,
    }
}

fn workspace(script: &str) -> Setup {
    Setup::Stage {
        open: None,
        steps: steps(script),
        grid: None,
        outside: BootOutside::Deny,
    }
}

fn phone(script: &str) -> Setup {
    Setup::Stage {
        open: None,
        steps: steps(script),
        grid: Some(Grid { w: 4, h: 3 }),
        outside: BootOutside::Deny,
    }
}

/// How long a stage settles before its state counts as reached: the
/// springs a boot starts — a panel's fade-in, the camera's pan to focus —
/// have to land before the node freezes into a picture.
const SETTLE_MS: u64 = 900;

/// Steps in the harness's grammar, ending in the arrival the stage waits
/// for. An empty script is the boot itself, settled.
fn steps(script: &str) -> Option<Vec<Step>> {
    if script.trim().is_empty() {
        return Some(vec![Step::Wait(SETTLE_MS), Step::Quit]);
    }
    let mut steps = e2e::parse(script).unwrap_or_else(|e| panic!("catalog: {e}: {script:?}"));
    if steps.last() != Some(&Step::Quit) {
        steps.push(Step::Quit);
    }
    Some(steps)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn at(h: u32, min: u32) -> f64 {
    mail::ts(2026, 8, 30, h, min)
}

fn head(who: &[&str], topic: &str, unread: bool, n: i64) -> mail::ThreadHead {
    mail::ThreadHead {
        thread: 1,
        target: 1,
        who: who.iter().map(|s| (*s).to_string()).collect(),
        topic: topic.to_string(),
        last: at(9, 12),
        unread,
        n,
    }
}

fn letter(
    id: i64,
    from: (&str, &str),
    subject: &str,
    body: &str,
    html: Option<&str>,
    status: Option<(&str, bool)>,
) -> mail::ThreadMail {
    mail::ThreadMail {
        mail: mail::MailFull {
            head: mail::MailHead {
                id,
                from_name: from.0.to_string(),
                from_email: from.1.to_string(),
                subject: subject.to_string(),
                date: at(9, 12),
                unread: false,
            },
            body: body.to_string(),
            html: html.map(str::to_string),
            status: status.map(|(s, e)| (s.to_string(), e)),
            to: "me@prepor.dev".to_string(),
            forwarded: false,
        },
        role: "inbox".to_string(),
        message_id: format!("fixture-{id}@prepor.dev"),
    }
}

fn account(email: &str, host: Option<&str>, status: Option<&str>) -> mail::Account {
    mail::Account {
        id: 1,
        label: "demo".to_string(),
        email: email.to_string(),
        imap_host: host.map(str::to_string),
        smtp_host: host.map(|h| h.replace("imap", "smtp")),
        status: status.map(str::to_string),
        synced: None,
    }
}

fn orow(num: &str, main: &str, detail: &str, right: &str) -> OverlayRowData {
    OverlayRowData {
        num: num.to_string(),
        main: main.to_string(),
        detail: detail.to_string(),
        right: right.to_string(),
        ..Default::default()
    }
}

/// The newest seeded mail whose subject contains `pat`.
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

/// The address of the seeded sender whose name contains `pat`.
fn sender_like(store: &Store, pat: &str) -> String {
    mail::senders(store)
        .iter()
        .find(|s| s.name.contains(pat))
        .map(|s| s.email.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The scenes
// ---------------------------------------------------------------------------

/// Every scene, in canvas order.
#[must_use]
pub fn scenes() -> Vec<Scene<Setup>> {
    vec![
        inbox_row(),
        thread_message(),
        overlay_row(),
        launcher(),
        account_row(),
        link(),
        inbox(),
        message(),
        compose(),
        small_panels(),
        phone_scene(),
        workspace_scene(),
    ]
}

fn inbox_row() -> Scene<Setup> {
    let row = |h: mail::ThreadHead, selected: bool| {
        widget(live_id!(inbox_row_tpl), move |cx, w| {
            w.as_inbox_row().populate(cx, &h, selected);
        })
    };
    let elena = || head(&["Elena Petrova"], "Sat hike", false, 1);
    let long = "[stelaxis] CI failed on main — workflow main #4116 failed on push 00a1b2c, the full logs attached to the run";
    Scene::new("inbox row", (520.0, 56.0))
        .note("One conversation as the inbox lists it: who wrote and when, the topic on its own line.")
        .note("Bold while any of it is unread; the wash while the cursor is on it.")
        .node("read", row(elena(), false))
        .node("unread", row(head(&["Elena Petrova"], "Sat hike", true, 1), false))
        .about("the whole row bold, not a dot")
        .node("selected", row(elena(), true))
        .about("the cursor's wash; focus stays in the list")
        .node("conversation", row(head(&["me", "Elena", "Vera"], "Q3 infra", true, 4), false))
        .about("first names once there are two, then the count")
        .node("long topic", row(head(&["GitHub"], long, false, 1), false))
        .about("one line each, ellipsized")
        .node("narrow", row(head(&["me", "Elena", "Vera"], "Q3 infra", true, 4), false))
        .sized((320.0, 56.0))
        .about("the phone's width")
        .edge("read", "unread", "a reply arrives")
        .edge("read", "selected", "↓ / click")
}

fn thread_message() -> Scene<Setup> {
    let msg = |t: mail::ThreadMail, open: bool, quoted: bool| {
        widget(live_id!(thread_msg_tpl), move |cx, w| {
            w.as_thread_msg().populate(cx, 0, &t, open, quoted);
        })
    };
    let from = ("Elena Petrova", "elena@prepor.dev");
    let plain = "Thanks — the logs point at the cache step.\n\nI will retry with the cache off and report back tomorrow.";
    let quoted = "Retrying with the cache off fixed it.\n\nOn Sat, 30 Aug 2026, Elena Petrova wrote:\n> the logs point at the cache step\n> I will retry with the cache off";
    let html = "<p>Thanks — the logs point at the <b>cache step</b>.</p><ul><li>retry with the cache off</li><li>report back tomorrow</li></ul>";
    Scene::new("thread message", (640.0, 200.0))
        .note("One message of a conversation, as the thread panel stacks them: a header line collapsed, the letter under it open.")
        .node("collapsed", msg(letter(1, from, "Re: Q3 infra", plain, None, None), false, false))
        .sized((640.0, 44.0))
        .about("who, the first line, when")
        .node("open", msg(letter(1, from, "Re: Q3 infra", plain, None, None), true, false))
        .node("quote folded", msg(letter(2, from, "Re: Q3 infra", quoted, None, None), true, false))
        .about("the tail it answered, folded to a line")
        .node("quote unfolded", msg(letter(2, from, "Re: Q3 infra", quoted, None, None), true, true))
        .sized((640.0, 300.0))
        .node("html", msg(letter(3, from, "Re: Q3 infra", plain, Some(html), None), true, false))
        .about("the letter keeps its lists, emphasis and links")
        .node(
            "failed",
            msg(
                letter(
                    4,
                    ("me", "me@prepor.dev"),
                    "Re: Q3 infra",
                    plain,
                    None,
                    Some(("send failed: connection refused", true)),
                ),
                false,
                false,
            ),
        )
        .sized((640.0, 44.0))
        .about("the status line, in the one colour errors get")
        .edge("collapsed", "open", "click the header")
        .edge("quote folded", "quote unfolded", "click the fold")
}

fn overlay_row() -> Scene<Setup> {
    let row = |d: OverlayRowData| {
        widget(live_id!(overlay_row_tpl), move |cx, w| {
            w.as_overlay_row().populate(cx, &d);
        })
    };
    Scene::new("overlay row", (520.0, 40.0))
        .note("One row of a modal sheet — the workspaces roster, the undo history, a launcher hit.")
        .note("The sheet is the chassis; this is what it stacks.")
        .node("plain", row(orow("", "inbox", "", "")))
        .node(
            "hovered",
            row(OverlayRowData {
                hovered: true,
                ..orow("", "inbox", "", "")
            }),
        )
        .about("the wash a button takes under the pointer")
        .node(
            "current",
            row(OverlayRowData {
                current: true,
                ..orow("", "inbox", "", "")
            }),
        )
        .about("inverted: the current workspace, the selected hit, the head of the history")
        .node(
            "muted",
            row(OverlayRowData {
                muted: true,
                ..orow("", "archive “Sat hike”", "", "")
            }),
        )
        .about("an undone branch: quiet, still walkable")
        .node("workspace", row(orow("3", "inbox · Sat hike · compose", "", "")))
        .node("launcher hit", row(orow("", "Q3 infra", "Elena Petrova, Vera Kovac · 4", "ws 4")))
        .about("a hit on another workspace wears its badge")
        .edge("plain", "hovered", "pointer over")
        .edge("plain", "current", "↓ / enter")
}

fn launcher() -> Scene<Setup> {
    let sheet = |query: &str, rows: Vec<OverlayRowData>| {
        let q = query.to_string();
        let props = OverlayProps {
            rows,
            query: q.clone(),
            alpha: 1.0,
        };
        sheet(live_id!(launcher_overlay_tpl), props, move |cx, w| {
            w.text_input(cx, ids!(query_input)).set_text(cx, &q);
        })
    };
    Scene::new("launcher", (560.0, 300.0))
        .note("Double-cmd raises it: one field over the hits — open panels first, then roots, people, mail.")
        .node(
            "empty",
            sheet(
                "",
                vec![
                    orow("", "inbox", "", ""),
                    orow("", "help", "", ""),
                    orow("", "settings", "", ""),
                    orow("", "compose", "", ""),
                ],
            ),
        )
        .about("nothing typed: what is open, then what can be")
        .node(
            "hits",
            sheet(
                "q3",
                vec![
                    OverlayRowData {
                        current: true,
                        ..orow("", "Q3 infra", "Elena Petrova, Vera Kovac · 4", "")
                    },
                    orow("", "Re: Q3 infra", "me", "ws 4"),
                ],
            ),
        )
        .node("nothing", sheet("zzz", Vec::new()))
        .about("a query nothing answers says so")
        .edge("empty", "hits", "type q3")
        .edge("hits", "nothing", "type zzz")
}

fn account_row() -> Scene<Setup> {
    let row = |a: mail::Account| {
        widget(live_id!(account_row_tpl), move |cx, w| {
            w.as_account_row().populate(cx, &a);
        })
    };
    Scene::new("account row", (520.0, 56.0))
        .note("One account in settings: the address, its host, the last sync — or what went wrong with it.")
        .node("local demo", row(account("me@prepor.dev", None, None)))
        .node(
            "synced",
            row(account(
                "andrey@fastmail.com",
                Some("imap.fastmail.com"),
                Some("synced 2 min ago"),
            )),
        )
        .node(
            "error",
            row(account(
                "andrey@fastmail.com",
                Some("imap.fastmail.com"),
                Some("error: login failed (535)"),
            )),
        )
        .about("the status line turns into the error")
        .edge("synced", "error", "the password expires")
}

fn link() -> Scene<Setup> {
    let l = |text: &'static str, dotted: bool, accel: Option<char>| {
        widget(live_id!(link_tpl), move |cx, w| {
            w.as_slink().set_accel(cx, 0, text, Kind::About, dotted, accel);
        })
    };
    Scene::new("link", (240.0, 28.0))
        .note("The underline grammar: solid opens beside, dotted replaces in place.")
        .note("A link that has a chord wears its letter.")
        .node("solid", l("Elena Petrova", false, None))
        .about("opens joined, to the right")
        .node("dotted", l("messages from elena", true, None))
        .about("replaces this panel")
        .node("accelerator", l("reply", false, Some('r')))
        .about("⌘r — the letter drawn bold")
}

fn inbox() -> Scene<Setup> {
    let inbox = |script: &str| panel(|_| Kind::Inbox { filter: None }, script);
    Scene::new("inbox", (520.0, 640.0))
        .note("The mail list: a rich table over the conversations, the filter above it.")
        .note("Live — enter a node and walk it; ⌘a archives, ⌘z takes it back.")
        .node("fresh", inbox(""))
        .node("cursor", inbox("click \"inbox\"\nwait 200\nkey down 3\nwait 400"))
        .about("the walk previews; the list keeps the keyboard")
        .node("filtered", inbox("click \"filter\"\nwait 200\ntype \"github\"\nwait 400"))
        .node("filter error", inbox("click \"filter\"\nwait 200\ntype \"(github\"\nwait 400"))
        .about("what the filter could not read, under the field")
        .node("phone", inbox(""))
        .sized((380.0, 720.0))
        .edge("fresh", "cursor", "↓ ×3")
        .edge("fresh", "filtered", "/ github")
        .edge("fresh", "filter error", "/ (github")
}

fn message() -> Scene<Setup> {
    Scene::new("message", (560.0, 640.0))
        .note("A conversation as a page: every message of the thread, the one it opened on and the unread ones open, the rest collapsed to their header lines.")
        .node("thread", panel(|s| Kind::Message { id: mail_like(s, "[stelaxis] CI") }, ""))
        .about("the CI thread: six runs, two failed")
        .node("single", panel(|s| Kind::Message { id: mail_like(s, "Sat hike") }, ""))
        .about("one mail is a thread of one")
        .node("forwarded", panel(|s| Kind::Message { id: mail_like(s, "invoice 2026-08") }, ""))
        .about("passed on: the mark by the date, off the $Forwarded keyword")
        .node("phone", panel(|s| Kind::Message { id: mail_like(s, "[stelaxis] CI") }, ""))
        .sized((380.0, 720.0))
}

fn compose() -> Scene<Setup> {
    let reply = |script: &str| {
        panel(
            |s| Kind::Compose {
                seed: Seed::Reply(mail_like(s, "Q3 infra")),
            },
            script,
        )
    };
    Scene::new("compose", (560.0, 420.0))
        .note("A reply: TO and SUBJECT from the mail it answers, the cursor in the body.")
        .note("A forward: the letter under its header block, SUBJECT from it, the cursor in the empty TO.")
        .note("Send is a side effect with an undo window.")
        .node("reply", reply(""))
        .node("suggesting", reply("click \"to\"\nwait 200\ntype \", v\"\nwait 400"))
        .about("the TO field completes people from the mail world")
        .node(
            "forward",
            panel(|s| Kind::Compose { seed: Seed::Forward(mail_like(s, "Q3 infra")) }, ""),
        )
        .about("the same mail passed on: a conversation of its own")
        .node("blank", panel(|_| Kind::Compose { seed: Seed::Blank }, ""))
        .about("from the launcher's root: nothing prefilled")
        .edge("reply", "suggesting", "type in TO")
}

fn small_panels() -> Scene<Setup> {
    Scene::new("small panels", (520.0, 420.0))
        .note("Settings and its form, a sender's card, the manual, the colophon.")
        .node("settings", panel(|_| Kind::Settings, ""))
        .node("add account", panel(|_| Kind::AddAccount, ""))
        .about("four fields and the one button")
        .node("contact", panel(|s| Kind::Contact { email: sender_like(s, "Elena") }, ""))
        .sized((520.0, 260.0))
        .node("help", panel(|_| Kind::Help, ""))
        .sized((560.0, 760.0))
        .node("about", panel(|_| Kind::About, ""))
        .sized((420.0, 200.0))
        .edge("settings", "add account", "add an account")
}

fn workspace_scene() -> Scene<Setup> {
    Scene::new("workspace", (1440.0, 900.0))
        .note("The shell's own subjects: columns, joins, tabs — twelve by six units, panels placed niri-style.")
        .node("boot", workspace(""))
        .about("help and the inbox: the default session")
        .node("joined", workspace("click \"Sat hike\"\nwait 700"))
        .about("a preview joined to the right; the inbox keeps focus")
        .node(
            "chain",
            workspace("click \"Sat hike\"\nwait 700\nclick \"Elena Petrova\"\nwait 700"),
        )
        .about("from → contact: a joined chain")
        .node(
            "tabbed",
            workspace("click \"Sat hike\"\nwait 700\nkey cmd+bracketleft\nwait 700\nkey cmd+t\nwait 700"),
        )
        .about("the message consumed into the inbox's column, then tabs")
        .edge("boot", "joined", "click a row")
        .edge("joined", "chain", "click the sender")
        .edge("joined", "tabbed", "⌘[ then ⌘t")
}

fn phone_scene() -> Scene<Setup> {
    Scene::new("phone", (380.0, 780.0))
        .note("The cover display: a 4×3 grid, panels clamp to it.")
        .node("cover", phone(""))
        .about("the inbox fills the screen")
        .node("message", phone("wait 300\nswipe \"Q3 infra\" 2 0\nwait 800"))
        .about("full-screen; the camera follows focus")
        .edge("cover", "message", "tap a row")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_is_a_dag_with_a_name_per_state() {
        let all = scenes();
        assert!(all.len() >= 10);
        for s in &all {
            s.check().unwrap_or_else(|e| panic!("{e}"));
            assert!(!s.nodes.is_empty(), "{}: no nodes", s.name);
        }
        // The canvas's script addresses scenes by name.
        let mut names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn steps_end_in_an_arrival() {
        assert_eq!(steps(""), Some(vec![Step::Wait(SETTLE_MS), Step::Quit]));
        assert_eq!(steps("  \n"), Some(vec![Step::Wait(SETTLE_MS), Step::Quit]));
        let s = steps("wait 10\nkey down 2").unwrap();
        assert_eq!(s.last(), Some(&Step::Quit));
        assert_eq!(s.len(), 3);
        let q = steps("wait 10\nquit").unwrap();
        assert_eq!(q.len(), 2);
    }
}
