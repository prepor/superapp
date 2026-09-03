//! Scene definitions for the Panels Library.
//!
//! A node contains a component fixture, an isolated panel, or a workspace.
//! Panel and workspace nodes may replay end-to-end steps to reach a state.
//! Fixtures use real Rust types and widget APIs, so incompatible changes fail
//! at compile time.

use std::rc::Rc;

use makepad_widgets::*;

use crate::app::BootOutside;
use crate::core::{Grid, Kind, Role, Seed};
use crate::effect;
use crate::e2e::{self, Step};
use crate::files;
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
    /// the whole workspace starting from that panel, or the default
    /// session. `steps` lead to the state.
    Stage {
        open: Option<Open>,
        /// With `open`: the panel alone at the viewport (a panel node)
        /// rather than as the first column of a strip.
        solo: bool,
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
        solo: true,
        steps: steps(script),
        grid: None,
        outside: BootOutside::Deny,
    }
}

/// A panel on a **fake** outside: what reads beyond the store — a files
/// panel lists its directory through the outside — draws the demo tree
/// rather than *this world has no outside*.
fn panel_fake(open: impl Fn(&Store) -> Kind + 'static, script: &str) -> Setup {
    Setup::Stage {
        open: Some(Rc::new(open)),
        solo: true,
        steps: steps(script),
        grid: None,
        outside: BootOutside::Fake,
    }
}

/// The default session — help and the inbox — for the shell's own
/// subjects.
fn workspace(script: &str) -> Setup {
    Setup::Stage {
        open: None,
        solo: false,
        steps: steps(script),
        grid: None,
        outside: BootOutside::Deny,
    }
}

/// A workspace that starts from one panel and nothing else: a story about
/// what that panel opens beside itself.
fn workspace_on(open: impl Fn(&Store) -> Kind + 'static, script: &str) -> Setup {
    Setup::Stage {
        open: Some(Rc::new(open)),
        solo: false,
        steps: steps(script),
        grid: None,
        outside: BootOutside::Fake,
    }
}

fn phone(script: &str) -> Setup {
    Setup::Stage {
        open: None,
        solo: false,
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
        // The library's accounts are the password kind; a Gmail sign-in has
        // no fixture because the scene is what settings *draws*, and that
        // is the same row either way.
        auth: None,
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

/// The effect queue's fixtures run on the **mount's** clock, not on the mail
/// seed's. A mount starts at [`crate::app::virtual_epoch`]; a `not_before`
/// placed on the mail fixtures' own morning is therefore already due, and
/// the executor would claim the row and settle it while the scene was still
/// settling — a node that draws something different every time it is read.
fn ago(mins: f64) -> f64 {
    crate::app::virtual_epoch() - mins * 60.0
}

/// The same clock, forward: a backoff the mount will not reach.
fn ahead(mins: f64) -> f64 {
    crate::app::virtual_epoch() + mins * 60.0
}

impl Default for Filed {
    fn default() -> Self {
        Filed {
            entity: "account:1",
            idempotent: true,
            at: ago(180.0),
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
        e.writes(),
    );
    store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO effect(kind, payload, entity, status, idempotent,
                                    reply, error, attempts, not_before, created, updated,
                                    writes)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)",
                rusqlite::params![
                    row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9,
                    row.10
                ],
            )
        })
        .expect("catalog: planting a job");
}

/// Files one in-memory effect into a node's ring, the way [`file`] files a
/// job into its queue — from the real effect value, so its `KIND` and its
/// own `describe` are what the scene shows.
fn keep<E: effect::Effect>(store: &Store, e: &E, at: f64, error: Option<&str>) {
    store.mem().record(effect::MemEffect {
        seq: store.mem().next_seq(),
        kind: E::KIND,
        entity: e.entity(),
        writes: e.writes(),
        what: e.describe(),
        error: error.map(str::to_string),
        at,
    });
}

/// The queue an effect-log node stands on: an ordinary morning of an
/// account that syncs — two pushes that landed, an archive undone before
/// the executor reached it, a flag still backing off after a refusal, and
/// the one genuinely irreversible effect having given up. Then the two the
/// queue never saw: the session those pushes went through, and the read
/// that found nothing new. Nobody files those, and the ring is the only
/// place they exist.
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
            at: ago(178.0),
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
            at: ago(172.0),
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
            at: ago(161.0),
            // Backing off: refused a moment ago, due again in five minutes —
            // which the mount's clock never reaches, so the row stands still.
            not_before: ahead(5.0),
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
            at: ago(152.0),
            status: "failed",
            attempts: 6,
            error: Some("535 authentication failed"),
            ..Filed::default()
        },
    );
    keep(
        store,
        &mail::Connect {
            account: 1,
            creds: effect::Creds::password("imap.fastmail.com", "elena@fastmail.com", ""),
        },
        ago(181.0),
        None,
    );
    keep(
        store,
        &mail::Fetch {
            account: 1,
            folder: "INBOX".into(),
            from: 122,
        },
        ago(155.0),
        None,
    );
    // …and one the panel's default keeps: a read is the ring's usual
    // business, but not all of it.
    keep(
        store,
        &effect::Clip {
            text: "",
            what: "panel context",
        },
        ago(150.0),
        None,
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
        what: None,
        writes: e.writes(),
    };
    (job, e.describe())
}

/// Builds a row for an effect kept only in memory. A negative ID marks its
/// source:
/// no payload to decode, no attempts to count, no idempotence to promise,
/// and the sentence carried on the row rather than derived from JSON.
fn kept<E: effect::Effect>(
    e: &E,
    seq: i64,
    at: f64,
    error: Option<&'static str>,
) -> (effect::Job, String) {
    let what = e.describe();
    let job = effect::Job {
        id: -seq,
        kind: E::KIND.to_string(),
        entity: e.entity(),
        status: if error.is_some() { "failed" } else { "done" }.to_string(),
        reply: None,
        error: error.map(str::to_string),
        attempts: 1,
        payload: String::new(),
        idempotent: false,
        created: at,
        updated: at,
        not_before: 0.0,
        what: Some(what.clone()),
        writes: e.writes(),
    };
    (job, what)
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
        what: None,
        // The column is all there is to go on for a kind nothing can
        // decode — and it is on the row, which is the point of it being a
        // column at all.
        writes: true,
    };
    (job, payload.to_string())
}

/// The four effects this app files, as the scenes show them.
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

fn a_forwarded() -> mail::Forwarded {
    mail::Forwarded {
        account: 1,
        message: 51,
        folder: "INBOX".into(),
        uid: 121,
        on: true,
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
        marks_bar(),
        thread_message(),
        overlay_row(),
        launcher(),
        account_row(),
        effect_row(),
        link(),
        mailbox(),
        inbox_marks(),
        effect_log(),
        job(),
        message(),
        compose(),
        files_row(),
        files(),
        file_card(),
        attachment_card(),
        files_walk(),
        small_panels(),
        problems(),
        phone_scene(),
        workspace_scene(),
    ]
}

fn inbox_row() -> Scene<Setup> {
    let row = |h: mail::ThreadHead, selected: bool, marked: bool| {
        widget(live_id!(mailbox_row_tpl), move |cx, w| {
            w.as_mailbox_row().populate(cx, &h, selected, marked);
        })
    };
    let elena = || head(&["Elena Petrova"], "Sat hike", false, 1);
    let long = "[stelaxis] CI failed on main — workflow main #4116 failed on push 00a1b2c, the full logs attached to the run";
    Scene::new("inbox row", (520.0, 56.0))
        .note("One conversation as the inbox lists it: who wrote and when, the topic on its own line.")
        .note("Unread rows are bold. The cursor adds a grey background. Marked rows have a dark bar.")
        .node("read", row(elena(), false, false))
        .node("unread", row(head(&["Elena Petrova"], "Sat hike", true, 1), false, false))
        .about("the whole row bold, not a dot")
        .node("selected", row(elena(), true, false))
        .about("grey background under the cursor; focus stays in the list")
        .node("marked", row(elena(), false, true))
        .about("a dark bar marks the row without changing its size")
        .node("marked, cursor", row(head(&["Elena Petrova"], "Sat hike", true, 1), true, true))
        .about("cursor background and mark together; bold still means unread")
        .node("conversation", row(head(&["me", "Elena", "Vera"], "Q3 infra", true, 4), false, false))
        .about("first names once there are two, then the count")
        .node("long topic", row(head(&["GitHub"], long, false, 1), false, false))
        .about("one line each, shortened to fit")
        .node("narrow", row(head(&["me", "Elena", "Vera"], "Q3 infra", true, 4), false, false))
        .sized((320.0, 56.0))
        .about("the phone's width")
        .edge("read", "unread", "a reply arrives")
        .edge("read", "selected", "↓ / click")
        .edge("read", "marked", "space")
        .edge("selected", "marked, cursor", "space")
}

fn thread_message() -> Scene<Setup> {
    let msg = |t: mail::ThreadMail, open: bool, quoted: bool| {
        widget(live_id!(thread_msg_tpl), move |cx, w| {
            w.as_thread_msg().populate(cx, 0, &t, open, quoted, &[]);
        })
    };
    // What a letter carries, drawn under it.
    let carrying = |t: mail::ThreadMail, atts: Vec<mail::Attachment>| {
        widget(live_id!(thread_msg_tpl), move |cx, w| {
            w.as_thread_msg().populate(cx, 0, &t, true, false, &atts);
        })
    };
    let part = |at: u32, name: &str, size: u64| mail::Attachment {
        message: 1,
        at,
        name: name.to_string(),
        mime: crate::files::mime_of(name).to_string(),
        size,
        cid: String::new(),
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
        .node(
            "carries",
            carrying(
                letter(5, from, "Re: Q3 infra", plain, None, None),
                vec![
                    part(1, "q3-budget.csv", 4 * 1024 + 210),
                    part(2, "invoice-2026-08.pdf", 96 * 1024),
                ],
            ),
        )
        .sized((640.0, 200.0))
        .about("its parts under it, each a link to the card over it")
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
        .note("The first four are the effects this app files — everything it has tried on the outside world is one of them. The rest is what becomes of one.")
        .node("move", row(shown(&a_move(), &queued()), false))
        .about("make the server agree which folder a mail lives in")
        .node("seen", row(shown(&a_seen(), &queued()), false))
        .about("…and whether it has been read")
        .node("forwarded", row(shown(&a_forwarded(), &queued()), false))
        .about("…and whether it has been passed on ($Forwarded)")
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
                        not_before: ahead(5.0),
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
        .node(
            "in memory",
            row(
                kept(
                    &mail::Fetch {
                        account: 1,
                        folder: "INBOX".into(),
                        from: 118,
                    },
                    12,
                    ago(2.0),
                    None,
                ),
                false,
            ),
        )
        .about("an effect nobody files: it ran at the call, and the ring is the only place it exists")
        .node(
            "in memory, refused",
            row(
                kept(
                    &mail::Connect {
                        account: 1,
                        creds: effect::Creds::password("imap.fastmail.com", "elena@fastmail.com", ""),
                    },
                    13,
                    ago(2.0),
                    Some("connection refused"),
                ),
                false,
            ),
        )
        .sized((560.0, 76.0))
        .about("which is exactly why the ring exists — before it, this line lived as long as the string it returned")
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

fn mailbox() -> Scene<Setup> {
    let inbox = |script: &str| panel(|_| Kind::Mailbox { role: Role::Inbox, filter: None }, script);
    let over = |role: Role| panel(move |_| Kind::Mailbox { role, filter: None }, "");
    Scene::new("mailbox", (520.0, 640.0))
        .note("The mail list: a rich table over the conversations, the filter above it.")
        .note("One panel over four folders — the inbox, the archive, sent, spam. Same rows, same walk, same grammar in the filter; only the folder the query starts from changes.")
        .note("Live — enter a node and walk it; ⌘a archives, ⌘z takes it back.")
        .node("fresh", inbox(""))
        .node("cursor", inbox("click \"inbox\"\nwait 200\nkey down 3\nwait 400"))
        .about("the walk previews; the list keeps the keyboard")
        .node("filtered", inbox("click \"filter\"\nwait 200\ntype \"github\"\nwait 400"))
        .node("filter error", inbox("click \"filter\"\nwait 200\ntype \"(github\"\nwait 400"))
        .about("what the filter could not read, under the field")
        .node("archive", over(Role::Archive))
        .about("what was filed away — the rows a conversation still makes there")
        .node("sent", over(Role::Sent))
        .about("my own letters; the participants read “me”")
        .node("spam", over(Role::Spam))
        .about("no archive verb on the bar: the mail is already out of the inbox")
        .node("phone", inbox(""))
        .sized((380.0, 720.0))
        .edge("fresh", "cursor", "↓ ×3")
        .edge("fresh", "filtered", "/ github")
        .edge("fresh", "filter error", "/ (github")
        .edge("fresh", "archive", "⌘a / sync")
        .edge("fresh", "sent", "send")
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
    // What the panel opens on is `@wrote`; clearing the field is what puts
    // the reads back. Typed rather than folded into the query, so a node
    // that clears it is the whole demonstration.
    const CLEAR: &str = "click \"filter\"\nwait 200\nkey cmd+a\nkey backspace\nwait 400\nkey esc\nwait 300";
    Scene::new("effect log", (600.0, 640.0))
        .note("Everything that left the process, newest first, one page at a time — the queue and the in-memory ring, joined.")
        .note("It opens on `@wrote`: a sync pass asks a dozen questions for every answer it acts on, and what a human came for is what was changed. The field holds the default, so clearing it is one gesture.")
        .note("The inbox's shape over another table — the cursor walk previews the job beside the list, enter goes to it. Live: enter a node and walk it.")
        .node("queue", log(""))
        .about("a morning of one account: two pushes landed, one undone, one backing off, one given up — and the clipboard write nobody filed")
        .node("everything", log(CLEAR))
        .about("the filter cleared: the session it all went through, and the read that found nothing new")
        .node("cursor", log("click \"filter\"\nwait 200\nkey esc\nwait 200\nkey down 3\nwait 400"))
        .about("the walk previews the job it lands on; the list keeps the keyboard")
        .node("empty", panel(|_| Kind::Effects, ""))
        .sized((600.0, 300.0))
        .about("nothing has been changed out there yet — said, rather than left blank")
        .node("phone", log(""))
        .sized((380.0, 720.0))
        .edge("queue", "everything", "clear the filter")
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

fn marks_bar() -> Scene<Setup> {
    let bar = |kind: Kind, marked: usize, total: usize, hidden: usize| {
        widget(live_id!(mark_bar_tpl), move |cx, w| {
            w.as_mark_bar()
                .populate(cx, crate::ui::mark_verbs(&kind), marked, total, hidden);
        })
    };
    let inbox = move |m, t, h| bar(Kind::Mailbox { role: Role::Inbox, filter: None }, m, t, h);
    let files = move |m, t, h| bar(Kind::Files { dir: files::HOME.into() }, m, t, h);
    Scene::new("marks bar", (520.0, 40.0))
        .note("Shown when rows are marked: the count, available actions, select all, and clear.")
        .note("Appears with the first mark and closes with the last. Shortcuts match the single-row actions.")
        .node("three", inbox(3, 143, 0))
        .about("of the rows under the filter")
        .node("hidden", inbox(3, 12, 1))
        .sized((520.0, 64.0))
        .about("hidden marks are still counted; actions move to a new line")
        .node("all", inbox(143, 143, 0))
        .about("select all is disabled")
        .node("narrow", inbox(3, 143, 1))
        .sized((356.0, 64.0))
        .about("actions wrap at phone width")
        .node("files", files(2, 8, 0))
        .about("file actions for the marked rows: copy ⌘p, move ⌘m, delete ⌘d")
        .edge("three", "hidden", "/ github")
        .edge("three", "all", "⌘l / all")
}

fn inbox_marks() -> Scene<Setup> {
    let inbox = |script: &str| panel(|_| Kind::Mailbox { role: Role::Inbox, filter: None }, script);
    // The walk that marks: onto the first row, space on it, then a
    // shift+↓ range over the two under it — three marks, the cursor left
    // standing on the last of them.
    let three = "click \"inbox\"\nwait 200\nkey down\nwait 300\ntype \" \"\nwait 300\nkey shift+down 2\nwait 400";
    let filtered =
        format!("{three}\nkey /\nwait 300\ntype \"vera\"\nwait 300\nkey enter\nwait 600");
    let all = format!("{three}\nclick \"mark all\"\nwait 500");
    Scene::new("inbox, marked", (520.0, 640.0))
        .note("The inbox with marked rows: actions under the filter, a dark bar on each mark, and a separate cursor.")
        .note("Filtering keeps marks. Hidden marks are listed above the visible rows.")
        .note("Live — space marks, shift+↓ ranges, all takes the rest; with nothing marked the list is the inbox scene's fresh.")
        .node("three", inbox(three))
        .about("space marked the cursor's row, shift+↓ the two under it")
        .node("filtered", inbox(&filtered))
        .about("the two the filter hides, kept in sight above the rows")
        .node("all", inbox(&all))
        .about("every matching row, loaded or not; select all is disabled")
        .node("phone", inbox(three))
        .sized((380.0, 720.0))
        .about("at phone width, actions move below the count")
        .edge("three", "filtered", "/ vera")
        .edge("three", "all", "⌘l / all")
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
        .note("A reply: TO and SUBJECT from the mail it answers, its letter quoted under the attribution line, the cursor above both.")
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

// ---- files --------------------------------------------------------------

fn entry(name: &str, is_dir: bool, size: u64) -> files::Entry {
    files::Entry {
        name: name.to_string(),
        is_dir,
        size,
        modified: at(9, 12),
    }
}

fn files_row() -> Scene<Setup> {
    let row = |e: files::Entry, selected: bool| {
        widget(live_id!(files_row_tpl), move |cx, w| {
            w.as_files_row().populate(cx, &e, selected, false);
        })
    };
    let marked = |e: files::Entry, selected: bool| {
        widget(live_id!(files_row_tpl), move |cx, w| {
            w.as_files_row().populate(cx, &e, selected, true);
        })
    };
    Scene::new("files row", (520.0, 32.0))
        .note("One entry of a directory: the name, the size, when it changed.")
        .note("A directory wears its slash and no size — the slash is the whole mark.")
        .node("file", row(entry("report-q3.pdf", false, 1_258_291), false))
        .node("directory", row(entry("2026", true, 0), false))
        .node("selected", row(entry("report-q3.pdf", false, 1_258_291), true))
        .about("the cursor's wash; focus stays in the list")
        .node("hidden", row(entry(".DS_Store", false, 6_144), false))
        .about("out of a listing unless @hidden asks")
        .node(
            "long name",
            row(
                entry(
                    "screenshot-2026-08-30-at-14.02.11-the-whole-workspace-at-once.png",
                    false,
                    421_888,
                ),
                false,
            ),
        )
        .about("one line, shortened to fit; columns keep their width")
        .node("marked", marked(entry("report-q3.pdf", false, 1_258_291), false))
        .about("a dark bar marks the row without changing its size")
        .node("marked, cursor", marked(entry("report-q3.pdf", false, 1_258_291), true))
        .about("cursor background and mark together")
        .edge("file", "selected", "↓ / click")
        .edge("file", "marked", "space")
        .edge("selected", "marked, cursor", "space")
}

fn files() -> Scene<Setup> {
    let dir = |d: &'static str, script: &str| panel_fake(move |_| Kind::Files { dir: d.into() }, script);
    Scene::new("files", (520.0, 640.0))
        .note("A directory as a column: path, filter, and rows. Header actions apply to the open directory.")
        .note("Live — enter a node: arrows walk, enter goes, / filters, ⌘n asks for a name, ⌘p and ⌘m hold.")
        .node("home", dir(files::HOME, ""))
        .about("the launcher's root; dot-files out")
        .node("downloads", dir("~/Downloads", ""))
        .about("one crumb up, a dotted link; the directory first")
        .node(
            "cursor",
            dir("~/Downloads", "click \"Downloads\"\nwait 200\nkey down 2\nwait 400"),
        )
        .about("the cursor; alone here — what it previews is in the files walk")
        .node(
            "filtered",
            dir("~/Downloads", "click \"filter\"\nwait 200\ntype \"@kind:image\"\nwait 400"),
        )
        .about("the inbox's grammar: @dir @hidden @kind: @size> @modified>")
        .node(
            "new dir",
            dir(
                "~/Downloads",
                "click \"Downloads\"\nwait 200\nkey cmd+n\nwait 300\ntype \"invoices\"\nwait 300",
            ),
        )
        .about("⌘n: a field above the rows; enter creates, esc puts it away")
        .node(
            "refused",
            dir(
                "~/Downloads",
                "click \"Downloads\"\nwait 200\nkey cmd+n\nwait 300\ntype \"2026\"\nwait 200\nkey enter\nwait 400",
            ),
        )
        .about("a name already here: the status line, in the one colour errors get")
        .node(
            "holding",
            dir("~/Downloads", "click \"Downloads\"\nwait 200\nkey cmd+m\nwait 500"),
        )
        .about("⌘m holds the directory shown: every files panel now offers move here")
        .node(
            "crumb up",
            dir(
                "~/Downloads",
                "click \"filter\"\nwait 200\ntype \"q3\"\nwait 300\nclick \"~\"\nwait 500",
            ),
        )
        .about("a crumb replaces the panel with ~ in place; the filter and the cursor start over")
        .node(
            "go to",
            dir("~/Downloads", "click \"Downloads\"\nwait 200\nkey cmd+g\nwait 300\ntype \"/t\"\nwait 400"),
        )
        .about("⌘g: the crumbs become a path field seeded with the directory; a second root restarts it (//tmp), each segment completes like a shell's tab")
        .node(
            "went",
            dir(
                "~/Downloads",
                "click \"Downloads\"\nwait 200\nkey cmd+g\nwait 300\ntype \"/tmp/\"\nwait 300\nkey enter\nwait 500",
            ),
        )
        .about("enter goes — beyond ~: the panel is /tmp now, the crumbs climb to /")
        .node("gone", dir("~/Downloads/2027", ""))
        .about("a directory that left under the panel; the crumbs still climb")
        .node("phone", dir("~/Downloads", ""))
        .sized((380.0, 720.0))
        .edge("home", "downloads", "click Downloads/")
        .edge("downloads", "cursor", "↓ ×2")
        .edge("downloads", "filtered", "/ @kind:image")
        .edge("filtered", "crumb up", "click ~")
        .edge("downloads", "new dir", "⌘n")
        .edge("new dir", "refused", "2026, enter")
        .edge("downloads", "holding", "⌘m")
        .edge("downloads", "go to", "⌘g, /t")
        .edge("go to", "went", "/tmp/, enter")
}

fn file_card() -> Scene<Setup> {
    let card = |p: &'static str, script: &str| panel_fake(move |_| Kind::File { path: p.into() }, script);
    Scene::new("file card", (520.0, 360.0))
        .note("A file as a card: name, kind and size, when it changed, the path selectable; under the rule, the preview.")
        .node("text", card("~/Downloads/README.txt", ""))
        .about("the first 64 KB, in the app's one face")
        .node("image", card("~/Downloads/2026/photo-lisbon.jpg", ""))
        .sized((520.0, 480.0))
        .about("fit to the width (the demo tree's pictures are the icon)")
        .node("other", card("~/Downloads/report-q3.pdf", ""))
        .sized((520.0, 220.0))
        .about("no preview: open shows it")
        .node(
            "held",
            card("~/Downloads/report-q3.pdf", "click \"report-q3\"\nwait 200\nkey cmd+p\nwait 500"),
        )
        .sized((520.0, 300.0))
        .about("⌘p holds a copy; the toast says where to go next")
        .edge("other", "held", "⌘p")
}

/// The same card, over a letter's own bytes. The demo seed gives
/// two of its letters a real multipart raw, so the id is looked up rather
/// than invented — a fixture would prove nothing about the walk that makes
/// these rows.
fn attachment_card() -> Scene<Setup> {
    let card = |name: &'static str| {
        panel_fake(
            move |store| {
                let (mail, at) = store
                    .conn()
                    .query_row(
                        "SELECT message, part FROM attachment WHERE name = ?1",
                        [name],
                        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u32)),
                    )
                    .unwrap_or((0, 0));
                Kind::Attachment { mail, at }
            },
            "",
        )
    };
    Scene::new("attachment card", (520.0, 340.0))
        .note("One part of a letter, on the file browser's own card: the same four lines and the same preview, over the mail's bytes rather than a path.")
        .note("Where a file card shows its path, a part shows its media type — it has no path until `open` writes it out.")
        .node("text", card("q3-budget.csv"))
        .about("the part read back out of the letter, previewed as text")
        .node("other", card("invoice-2026-08.pdf"))
        .sized((520.0, 220.0))
        .about("no preview: open writes it out and hands it to the OS")
}

fn files_walk() -> Scene<Setup> {
    let from_home = |script: &str| workspace_on(|_| Kind::Files { dir: files::HOME.into() }, script);
    // The walk by keys alone: ↓ previews beside the list, enter goes.
    let preview = "key down 3\nwait 700\n";
    let chain = "key down 3\nwait 700\nkey enter\nwait 500\nkey down 1\nwait 700\nkey enter\nwait 500\nkey down 3\nwait 700\n";
    Scene::new("files walk", (1440.0, 900.0))
        .note("The column walk: the cursor previews a directory or a card joined beside the list, enter goes, the next row replaces.")
        .note("A workspace that starts from the files root alone — no help, no inbox. Live: enter a node and walk it.")
        .node("root", from_home(""))
        .about("~ as the first column")
        .node("preview", from_home(preview))
        .about("↓ ×3: Downloads previews beside ~; focus stays in the list, so the walk goes on")
        .node(
            "re-aimed",
            from_home(&format!("{preview}key enter\nwait 500\nkey down 1\nwait 500\nkey down 1\nwait 700")),
        )
        .about("in Downloads, ↓ onto 2026/ then ↓ onto a file: the same joined panel goes from a column to a card")
        .node("chain", from_home(chain))
        .about("enter goes; ~ → Downloads → 2026 → a card, the list still driving")
        .node("replaced", from_home(&format!("{chain}key up 2\nwait 700")))
        .about("↑ ×2: the next row replaces the card in place")
        .node(
            "holding",
            from_home(&format!("{chain}key enter\nwait 400\nkey cmd+p\nwait 300\nkey cmd+left\nwait 300\nkey cmd+left\nwait 700")),
        )
        .about("enter into the card, ⌘p holds it, ⌘← twice: Downloads now offers copy here")
        .node(
            "pasted",
            from_home(&format!("{chain}key enter\nwait 400\nkey cmd+p\nwait 300\nkey cmd+left\nwait 300\nkey cmd+left\nwait 400\nkey cmd+h\nwait 700")),
        )
        .about("⌘h — copy here — performs into the directory shown; into the file's own directory it lands under a free name")
        .node(
            "deleted",
            from_home(&format!("{chain}key enter\nwait 400\nkey cmd+d\nwait 700")),
        )
        .about("⌘d on the card: to the trash, never rm — the row goes, and the panel that was showing it closes")
        .node(
            "undone",
            from_home(&format!("{chain}key enter\nwait 400\nkey cmd+d\nwait 500\nkey cmd+z\nwait 700")),
        )
        .about("⌘z is the move back out of the trash — the file, and the card that was on it")
        .edge("root", "preview", "↓")
        .edge("preview", "re-aimed", "enter, ↓, ↓")
        .edge("preview", "chain", "enter, ↓, enter, ↓")
        .edge("chain", "replaced", "↑")
        .edge("chain", "holding", "enter, ⌘p, ⌘← ⌘←")
        .edge("holding", "pasted", "⌘h")
        .edge("chain", "deleted", "enter, ⌘d")
        .edge("deleted", "undone", "⌘z")
}

fn small_panels() -> Scene<Setup> {
    Scene::new("small panels", (520.0, 420.0))
        .note("Settings and its form, a sender's card, the manual, the colophon.")
        .node("settings", panel(|_| Kind::Settings, ""))
        .node("add account", panel(|_| Kind::AddAccount, ""))
        .about("four fields and the one button")
        .node("device sync", panel(|_| Kind::Bucket, ""))
        .about("the bucket, and the key that opens it — typed, not pushed")
        .node("contact", panel(|s| Kind::Contact { email: sender_like(s, "Elena") }, ""))
        .sized((520.0, 260.0))
        .node("help", panel(|_| Kind::Help, ""))
        .sized((560.0, 760.0))
        .node("about", panel(|_| Kind::About, ""))
        .sized((420.0, 200.0))
        .edge("settings", "add account", "add an account")
        .edge("settings", "device sync", "point at a bucket")
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
        .node("mark", seeded(Kind::Mailbox { role: Role::Inbox, filter: None }))
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
