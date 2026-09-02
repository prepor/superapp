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
use crate::effect;
use crate::e2e::{self, Step};
use crate::mail;
use crate::panels::*;
use crate::scene::Scene;
use crate::store::Store;

/// Sets a component's state through its own API, once, when it mounts.
pub type Populate = Rc<dyn Fn(&mut Cx, &WidgetRef)>;
/// The kind a solo stage opens on, resolved against its seeded store — and
/// the one place a node may put something *into* that store. A subject the
/// demo seed does not cover plants its own rows here: the effect queue is
/// written by the executor and seeded by nobody, so a log with anything in
/// it is a log a node wrote (see [`plant_queue`]).
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

/// A job planted into a log node's store, as the executor would have left
/// it. Only states the executor will not revisit are used — the terminal
/// ones, and a `pending` still behind its backoff — so the scene is the
/// same picture on every boot.
struct Filed {
    /// `action.entity`: whose work it was.
    entity: &'static str,
    /// Whether a crash may retry it.
    idempotent: bool,
    /// When it was filed, and (for a backoff) the earliest it may run.
    at: f64,
    not_before: f64,
    /// pending | done | failed | obsolete.
    status: &'static str,
    attempts: i64,
    reply: Option<&'static str>,
    error: Option<&'static str>,
}

impl Default for Filed {
    fn default() -> Self {
        Filed {
            entity: "account:1",
            idempotent: true,
            at: at(9, 12),
            not_before: 0.0,
            status: "done",
            attempts: 1,
            reply: None,
            error: None,
        }
    }
}

/// Files one job into a node's store the way the executor's own row reads:
/// the **real** effect value, its `KIND` and its `Serialize`, so a refactor
/// of an effect moves this scene with it instead of leaving it lying. The
/// registry decodes these payloads back on the way out, exactly as it does
/// for a job the app filed itself.
fn file<E: effect::Effect + serde::Serialize>(store: &Store, e: &E, f: &Filed) {
    let payload = serde_json::to_string(e).expect("catalog: the fixture effect encodes");
    let row = (
        E::KIND,
        payload,
        f.entity,
        f.status,
        f.idempotent,
        f.reply,
        f.error,
        f.attempts,
        f.not_before,
        f.at,
    );
    store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO effect(kind, payload, entity, status, idempotent,
                                    reply, error, attempts, not_before, created, updated)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                rusqlite::params![
                    row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9
                ],
            )
        })
        .expect("catalog: planting a job");
}

/// The queue an effect-log node stands on: an ordinary morning of an
/// account that syncs — two pushes that landed, an archive undone before
/// the executor reached it, a flag still backing off after a refusal, and
/// the one genuinely irreversible effect having given up.
///
/// The mails are the demo seed's, so the payloads point at rows that exist.
fn plant_queue(store: &Store) {
    let hike = mail_like(store, "Sat hike");
    let q3 = mail_like(store, "Q3 infra");
    file(
        store,
        &mail::Seen {
            account: 1,
            message: hike,
            folder: "INBOX".into(),
            uid: 118,
            seen: true,
        },
        &Filed::default(),
    );
    file(
        store,
        &mail::Move {
            account: 1,
            message: hike,
            to_folder: 2,
            from: "INBOX".into(),
            to: "Archive".into(),
            uid: 118,
        },
        &Filed {
            at: at(9, 14),
            reply: Some("119"),
            ..Filed::default()
        },
    );
    file(
        store,
        &mail::Move {
            account: 1,
            message: q3,
            to_folder: 4,
            from: "INBOX".into(),
            to: "Trash".into(),
            uid: 121,
        },
        // Undo landed while it waited: revalidated, never performed.
        &Filed {
            at: at(9, 20),
            status: "obsolete",
            ..Filed::default()
        },
    );
    file(
        store,
        &mail::Seen {
            account: 1,
            message: q3,
            folder: "INBOX".into(),
            uid: 121,
            seen: false,
        },
        &Filed {
            at: at(9, 31),
            // Backing off: filed at 9:31, next attempt 9:36.
            not_before: at(9, 36),
            status: "pending",
            attempts: 3,
            error: Some("connection refused"),
            ..Filed::default()
        },
    );
    file(
        store,
        &mail::Submit { outbox: 7 },
        &Filed {
            entity: "outbox:7",
            idempotent: false,
            at: at(9, 40),
            status: "failed",
            attempts: 6,
            error: Some("535 authentication failed"),
            ..Filed::default()
        },
    );
}

/// One row of the effect queue as the log lists it, taken **from the effect
/// itself**: its `KIND`, its own `Serialize` for the payload, and the
/// sentence its own `describe` returns. Those are the three the registry
/// hands the panel for a live job, so a component node shows what a real
/// row shows — and an effect that changes its wording changes this scene
/// rather than drifting from it.
fn shown<E: effect::Effect + serde::Serialize>(e: &E, f: &Filed) -> (effect::Job, String) {
    let job = effect::Job {
        id: 118,
        kind: E::KIND.to_string(),
        entity: Some(f.entity.to_string()),
        status: f.status.to_string(),
        reply: f.reply.map(str::to_string),
        error: f.error.map(str::to_string),
        attempts: f.attempts,
        payload: serde_json::to_string(e).expect("catalog: the fixture effect encodes"),
        idempotent: f.idempotent,
        created: f.at,
        updated: f.at + 120.0,
        not_before: f.not_before,
    };
    (job, e.describe())
}

/// A job whose kind this build has no handler for — a row an older version
/// wrote, or a domain not registered here. Nothing can decode it, so the log
/// shows the payload as it stands rather than dropping the row.
fn stranger(kind: &str, payload: &str, f: &Filed) -> (effect::Job, String) {
    let job = effect::Job {
        id: 118,
        kind: kind.to_string(),
        entity: Some(f.entity.to_string()),
        status: f.status.to_string(),
        reply: None,
        error: None,
        attempts: f.attempts,
        payload: payload.to_string(),
        idempotent: f.idempotent,
        created: f.at,
        updated: f.at + 120.0,
        not_before: f.not_before,
    };
    (job, payload.to_string())
}

/// The three effects this app files, as the scenes show them.
fn a_move() -> mail::Move {
    mail::Move {
        account: 1,
        message: 42,
        to_folder: 2,
        from: "INBOX".into(),
        to: "Archive".into(),
        uid: 118,
    }
}

fn a_seen() -> mail::Seen {
    mail::Seen {
        account: 1,
        message: 51,
        folder: "INBOX".into(),
        uid: 121,
        seen: true,
    }
}

fn a_submit() -> mail::Submit {
    mail::Submit { outbox: 7 }
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

/// The newest planted job in this state — how a job node names the row it
/// opens on, the way [`mail_like`] names a mail.
fn job_in(store: &Store, status: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT id FROM effect WHERE status = ?1 ORDER BY id DESC LIMIT 1",
            [status],
            |r| r.get(0),
        )
        .unwrap_or(1)
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
        effect_row(),
        link(),
        inbox(),
        effect_log(),
        job(),
        message(),
        compose(),
        small_panels(),
        problems(),
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

fn effect_row() -> Scene<Setup> {
    let row = |(j, what): (effect::Job, String), selected: bool| {
        widget(live_id!(effect_row_tpl), move |cx, w| {
            w.as_effect_row().populate(cx, &j, &what, selected);
        })
    };
    // Filed and waiting its turn — the state every job starts in, so the
    // three verbs below differ by nothing but themselves.
    let queued = || Filed {
        status: "pending",
        attempts: 0,
        ..Filed::default()
    };
    let outbox = || Filed {
        entity: "outbox:7",
        idempotent: false,
        ..queued()
    };
    Scene::new("effect row", (560.0, 60.0))
        .note("One job of the effect queue: the verb and whose it was, then the sentence the effect describes itself with.")
        .note("The first three are the effects this app files — everything it has tried on the outside world is one of them. The rest is what becomes of one.")
        .node("move", row(shown(&a_move(), &queued()), false))
        .about("make the server agree which folder a mail lives in")
        .node("seen", row(shown(&a_seen(), &queued()), false))
        .about("…and whether it has been read")
        .node("submit", row(shown(&a_submit(), &outbox()), false))
        .about("hand a mail to SMTP — the one effect a crash may not repeat, and the one that is not an account's")
        .node(
            "done",
            row(
                shown(
                    &a_move(),
                    &Filed {
                        reply: Some("119"),
                        ..Filed::default()
                    },
                ),
                false,
            ),
        )
        .about("the round trip landed; the answer is on the row")
        .node(
            "retrying",
            row(
                shown(
                    &a_move(),
                    &Filed {
                        status: "pending",
                        attempts: 3,
                        error: Some("connection refused"),
                        not_before: at(9, 36),
                        ..Filed::default()
                    },
                ),
                false,
            ),
        )
        .sized((560.0, 76.0))
        .about("the count appears once a job has fought; the error in the one colour errors get")
        .node(
            "given up",
            row(
                shown(
                    &a_submit(),
                    &Filed {
                        status: "failed",
                        attempts: 6,
                        error: Some("535 authentication failed"),
                        ..outbox()
                    },
                ),
                false,
            ),
        )
        .sized((560.0, 76.0))
        .about("six attempts, then it stops and waits for a human")
        .node(
            "undone",
            row(
                shown(
                    &a_move(),
                    &Filed {
                        status: "obsolete",
                        ..Filed::default()
                    },
                ),
                false,
            ),
        )
        .about("undo landed while it waited: revalidated, never performed, the server untouched")
        .node("cursor", row(shown(&a_move(), &queued()), true))
        .about("the cursor's wash — the walk previews the job beside the list")
        .node(
            "unreadable",
            row(
                stranger("telegram_send", r#"{"chat":88,"text":"on my way"}"#, &queued()),
                false,
            ),
        )
        .about("a kind this build cannot decode keeps its payload rather than vanishing")
        .node("narrow", row(shown(&a_move(), &queued()), false))
        .sized((380.0, 60.0))
        .about("the phone's width")
        .edge("move", "done", "the executor's round trip")
        .edge("move", "retrying", "the server said no")
        .edge("move", "undone", "⌘z first")
        .edge("submit", "given up", "six attempts")
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

fn effect_log() -> Scene<Setup> {
    // Every node plants the same morning into its own store — a mount's
    // world is its own, so the five states stand still while it is read.
    let log = |script: &str| {
        panel(
            |s| {
                plant_queue(s);
                Kind::Effects
            },
            script,
        )
    };
    Scene::new("effect log", (600.0, 640.0))
        .note("The effect queue read back: everything the app has tried on the outside world, newest first, one page at a time.")
        .note("The inbox's shape over another table — the cursor walk previews the job beside the list, enter goes to it. Live: enter a node and walk it.")
        .node("queue", log(""))
        .about("a morning of one account: two pushes landed, one undone, one backing off, one given up")
        .node("cursor", log("click \"filter\"\nwait 200\nkey esc\nwait 200\nkey down 3\nwait 400"))
        .about("the walk previews the job it lands on; the list keeps the keyboard")
        .node("empty", panel(|_| Kind::Effects, ""))
        .sized((600.0, 300.0))
        .about("nothing has left the process yet — said, rather than left blank")
        .node("phone", log(""))
        .sized((380.0, 720.0))
        .edge("queue", "cursor", "↓ ×3")
}

fn job() -> Scene<Setup> {
    // Every node ends by touching the sentence it drew: a job panel that
    // stopped naming its effect has no such element, and the node fails to
    // arrive instead of quietly showing an empty page.
    const ASSERT: &str = "click \"job effect\"\nwait 200";
    let job = |status: &'static str| {
        panel(
            move |s| {
                plant_queue(s);
                Kind::Job { id: job_in(s, status) }
            },
            ASSERT,
        )
    };
    Scene::new("job", (520.0, 420.0))
        .note("One job of the queue in full — what the effect log previews into, the way the inbox previews a message.")
        .note("The sentence the effect describes itself with, then the row as `sqlite3` would show it: the job's own facts, the payload it was filed as, the answer the world gave. All of it selectable.")
        .node("done", job("done"))
        .about("a push that landed; the server's answer under REPLY")
        .node("retrying", job("pending"))
        .about("refused once, backing off — the error, the count, and when it may run again")
        .node("given up", job("failed"))
        .about("six attempts on the one effect a crash may not repeat")
        .node("undone", job("obsolete"))
        .about("undo landed first: revalidated, never performed, no REPLY to show")
        .node("gone", panel(|_| Kind::Job { id: 4242 }, ASSERT))
        .sized((520.0, 200.0))
        .about("a row the queue no longer holds — the panel says so rather than inventing one")
        .edge("retrying", "given up", "six attempts")
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

/// Two standing problems in a stage's world, written through the opener:
/// an account whose password expired, and a send the executor gave up on.
fn seed_problems(s: &Store) {
    let _ = s.write(|c| {
        c.execute(
            "INSERT INTO account(label, email, imap_host, smtp_host, status)
             VALUES('fastmail', 'andrey@fastmail.com', 'imap.fastmail.com',
                    'smtp.fastmail.com', 'error: login failed (535)')",
            [],
        )?;
        c.execute(
            "INSERT INTO draft(panel, account, to_addr, subject, body, updated)
             VALUES(900, 1, 'vera@kovac.io', 'Re: Q3 infra', 'Thanks — will look today.', 0)",
            [],
        )?;
        c.execute(
            "INSERT INTO outbox(id, account, send_after, status, error)
             VALUES(900, 1, 0, 'failed', 'connection refused')",
            [],
        )?;
        c.execute(
            "INSERT INTO effect(kind, payload, entity, status, idempotent, error,
                                attempts, not_before, created, updated)
             VALUES('submit', '{\"outbox\":900}', 'outbox:900', 'failed', 0,
                    'connection refused', 6, 0, 0, 0)",
            [],
        )
        .map(|_| ())
    });
}

fn problems() -> Scene<Setup> {
    use crate::problems::{Problem, Source};
    let row = |p: Problem| {
        widget(live_id!(problem_row_tpl), move |cx, w| {
            w.as_problem_row().populate(cx, 0, &p);
        })
    };
    let account = Problem {
        source: Source::Account {
            id: 1,
            email: "andrey@fastmail.com".into(),
        },
        label: "andrey@fastmail.com".into(),
        line: "login failed (535)".into(),
        detail: "last synced aug 30 09:12".into(),
    };
    let send = |given_up: bool| Problem {
        source: Source::Send {
            outbox: 9,
            subject: "Re: Q3 infra".into(),
            seed: Seed::Reply(1),
            given_up,
        },
        label: "send “Re: Q3 infra”".into(),
        line: "connection refused".into(),
        detail: if given_up {
            "to vera@kovac.io — gave up after 6 attempts".into()
        } else {
            "to vera@kovac.io — attempt 2 of 6, next at aug 30 09:17".into()
        },
    };
    let sync = Problem {
        source: Source::Sync,
        label: "device sync".into(),
        line: "the bucket is unreachable".into(),
        detail: "3 frames waiting to publish".into(),
    };
    // Solo stages on a world with the two problems above standing. The
    // first pump round announces them — a toast — so the node waits for
    // that round, then out-waits the toast: it freezes on the standing
    // state, not the arrival. (Two waits: a replay consumes each whole,
    // and one long wait would jump the clock before the round announces.)
    let seeded = |kind: Kind| {
        panel(
            move |s: &Store| {
                seed_problems(s);
                kind.clone()
            },
            "wait 600\nwait 3600",
        )
    };
    Scene::new("problems", (560.0, 100.0))
        .note("What is wrong in the background, a row each: the account, the send, device sync — the error in the one colour, what can be done beside it.")
        .note("A toast announced it; the mark in the toast's corner counts what still stands.")
        .node("account", row(account))
        .about("sync kicks the worker; settings is where the password lives")
        .node("send retrying", row(send(false)))
        .about("the executor is on it: nothing to press")
        .node("send given up", row(send(true)))
        .about("retry files it again; reopen brings the draft back")
        .node("device sync", row(sync))
        .about("fixed by the network coming back")
        .node("mark", seeded(Kind::Inbox { filter: None }))
        .sized((520.0, 640.0))
        .about("bottom-right, static, red — the toast's corner, on any panel")
        .node("panel", seeded(Kind::Problems))
        .sized((560.0, 420.0))
        .about("the rows, live: sync, settings, retry, reopen")
        .node("clear", panel(|_| Kind::Problems, ""))
        .sized((560.0, 420.0))
        .about("nothing standing says so")
        .edge("send retrying", "send given up", "the sixth attempt fails")
        .edge("mark", "panel", "click the mark")
        .edge("panel", "clear", "the conditions clear")
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
