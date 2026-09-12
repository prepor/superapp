//! Two or three stores, a duplex stream between them, and virtual time.
//!
//! Every device here has a clock of its own that only moves when it is
//! moved, so *later* is something these tests decide rather than something
//! they wait for.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::protocol::{self, Error, Greeting, Side, Verdict};
use super::{Device, Op, Replicated};
use crate::app::{Schema, Step};
use crate::caps::ClockSource;
use crate::store::Db;

/// A note with a uid of its own, and a mark that can exist before the thing
/// it marks. `local` never travels; the check on `body` is a constraint an
/// op can fall foul of.
const TABLES: &str = "
CREATE TABLE t_note(
  id      INTEGER PRIMARY KEY AUTOINCREMENT,
  uid     TEXT NOT NULL UNIQUE DEFAULT (lower(hex(randomblob(16)))),
  title   TEXT NOT NULL DEFAULT '',
  body    TEXT NOT NULL DEFAULT '' CHECK(body <> 'poison'),
  seal    BLOB,
  deleted INTEGER NOT NULL DEFAULT 0,
  local   TEXT NOT NULL DEFAULT ''
);
CREATE TABLE t_loose(
  name  TEXT NOT NULL,
  value TEXT NOT NULL DEFAULT ''
);
CREATE UNIQUE INDEX t_loose_name ON t_loose(name);
CREATE TABLE t_mark(
  feed TEXT NOT NULL,
  guid TEXT NOT NULL,
  seen INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(feed, guid)
);
";

static SCHEMA: Schema = Schema {
    app: "sync-test",
    steps: &[Step::Sql(TABLES)],
};

static DECLARED: &[Replicated] = &[
    Replicated {
        table: "t_note",
        key: &["uid"],
        columns: &["title", "body", "seal", "deleted"],
    },
    Replicated {
        table: "t_mark",
        key: &["feed", "guid"],
        columns: &["seen"],
    },
];

/// What the note says, as one line, on whichever device is asked.
const NOTE: &str = "SELECT coalesce((SELECT title || '/' || body FROM t_note WHERE uid = 'n1'), 'gone')";

/// A device id: sixty-four of one character, so a failure names itself.
fn id(c: char) -> String {
    std::iter::repeat_n(c, 64).collect()
}

/// One store, standing at `at` on a clock of its own.
fn device(c: char, at: f64) -> (Arc<Db>, ClockSource) {
    let clock = ClockSource::virtual_from(at);
    let db = Db::open(
        None,
        &[&SCHEMA],
        Device::new(id(c))
            .named(format!("device {c}"))
            .clocked(clock.clone())
            .replicating(DECLARED.to_vec()),
    )
    .expect("a store");
    (db, clock)
}

async fn write(db: &Arc<Db>, sql: &'static str) {
    db.write_async(move |tx| tx.execute_batch(sql))
        .await
        .expect("the write");
}

async fn read(db: &Arc<Db>, sql: &'static str) -> String {
    db.read_on_writer(move |c| c.query_row(sql, [], |r| r.get::<_, String>(0)))
        .await
        .expect("the read")
}

async fn count(db: &Arc<Db>, sql: &'static str) -> i64 {
    db.read_on_writer(move |c| c.query_row(sql, [], |r| r.get::<_, i64>(0)))
        .await
        .expect("the count")
}

/// Waits until a store says this, and says what it said instead if it never
/// does.
async fn eventually(db: &Arc<Db>, sql: &'static str, want: &str) {
    let mut said = String::new();
    for _ in 0..400 {
        said = read(db, sql).await;
        if said == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("waited for {want:?} and got {said:?}");
}

/// Both ends of one connection, each running until the wire is dropped —
/// which is how these tests cut a link.
type Session = JoinHandle<Result<protocol::Exchange, Error>>;

struct Wire(Session, Session);

impl Drop for Wire {
    fn drop(&mut self) {
        self.0.abort();
        self.1.abort();
    }
}

/// The roster the service will keep: everyone is known.
fn known(_: &Greeting) -> Verdict {
    Verdict::Known
}

fn connect(a: &Arc<Db>, b: &Arc<Db>) -> Wire {
    wire(a, b, known, known, Side::Dial(None))
}

fn wire(
    a: &Arc<Db>,
    b: &Arc<Db>,
    listens: impl Fn(&Greeting) -> Verdict + Send + 'static,
    dials: impl Fn(&Greeting) -> Verdict + Send + 'static,
    side: Side,
) -> Wire {
    let (one, two) = tokio::io::duplex(64 * 1024);
    Wire(
        tokio::spawn(protocol::run(a.clone(), one, Side::Listen, listens, a.ops_landed())),
        tokio::spawn(protocol::run(b.clone(), two, side, dials, b.ops_landed())),
    )
}

/// A note, made on one device.
const MADE: &str = "INSERT INTO t_note(uid, title, body) VALUES('n1', 'one', 'first')";

// -- convergence ---------------------------------------------------------------

/// Two devices edit one note while apart. The later edit is the one both
/// keep, whichever device is asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_note_edited_on_both_devices_keeps_the_later_edit() {
    let (a, _ca) = device('a', 100.0);
    let (b, cb) = device('b', 100.0);
    write(&a, MADE).await;
    {
        let _wire = connect(&a, &b);
        eventually(&b, NOTE, "one/first").await;
    }
    // Apart: each writes its own body, and B's clock is the later one.
    write(&a, "UPDATE t_note SET body = 'from a' WHERE uid = 'n1'").await;
    cb.advance(10.0);
    write(&b, "UPDATE t_note SET body = 'from b' WHERE uid = 'n1'").await;
    let _wire = connect(&a, &b);
    eventually(&a, NOTE, "one/from b").await;
    eventually(&b, NOTE, "one/from b").await;
}

/// A deletion on one device against an edit on the other. The newer of the
/// two wins, and it wins the same way on both — the row is what the cells
/// newer than the tombstone say, and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tombstone_and_an_edit_settle_the_same_way_on_both() {
    let (a, ca) = device('a', 100.0);
    let (b, cb) = device('b', 100.0);
    write(&a, MADE).await;
    {
        let _wire = connect(&a, &b);
        eventually(&b, NOTE, "one/first").await;
    }
    // The edit is later than the deletion, so the note comes back — with
    // the cell that outlived the tombstone, and its defaults.
    ca.advance(100.0);
    write(&a, "DELETE FROM t_note WHERE uid = 'n1'").await;
    cb.advance(200.0);
    write(&b, "UPDATE t_note SET body = 'later' WHERE uid = 'n1'").await;
    {
        let _wire = connect(&a, &b);
        eventually(&a, NOTE, "/later").await;
        eventually(&b, NOTE, "/later").await;
    }
    // And the other way round: a deletion after everything sweeps the row.
    ca.advance(1000.0);
    write(&a, "DELETE FROM t_note WHERE uid = 'n1'").await;
    let _wire = connect(&a, &b);
    eventually(&a, NOTE, "gone").await;
    eventually(&b, NOTE, "gone").await;
}

/// A third device that has never met the first still receives its ops,
/// because every side answers a `Have` with every origin it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_third_device_is_carried_by_the_second() {
    let (a, _ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    let (c, _cc) = device('c', 100.0);
    write(&a, MADE).await;
    {
        let _wire = connect(&a, &b);
        eventually(&b, NOTE, "one/first").await;
    }
    let _wire = connect(&b, &c);
    eventually(&c, NOTE, "one/first").await;
    // Including the first device's own row in the roster.
    assert_eq!(
        count(&c, "SELECT count(*) FROM sync_peer").await,
        3,
        "every device's name reached the third"
    );
}

/// A cut link and a reconnect: the exchange starts again from `Have`, and
/// nothing arrives twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnect_resumes_and_repeats_nothing() {
    let (a, ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    write(&a, MADE).await;
    {
        let _wire = connect(&a, &b);
        eventually(&b, NOTE, "one/first").await;
    }
    ca.advance(10.0);
    write(&a, "UPDATE t_note SET body = 'again' WHERE uid = 'n1'").await;
    let _wire = connect(&a, &b);
    eventually(&b, NOTE, "one/again").await;
    let held = count(&b, "SELECT count(*) FROM sync_op").await;
    let mine = count(
        &b,
        "SELECT count(*) FROM sync_op WHERE origin = (SELECT device FROM sync_self)",
    )
    .await;
    assert_eq!(
        held,
        3 + 3 + 4 + 1,
        "each device's name, each cell of the note, and the edit — once"
    );
    assert_eq!(mine, 3, "and nothing of the second device's own came back");
}

/// Applying a peer's ops writes no ops of this device's own: the connection
/// carries them one way, and nothing echoes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_is_applied_is_never_logged_again() {
    let (a, _ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    let before = count(&b, "SELECT next_seq FROM sync_self").await;
    write(&a, MADE).await;
    let _wire = connect(&a, &b);
    eventually(&b, NOTE, "one/first").await;
    assert_eq!(
        count(&b, "SELECT next_seq FROM sync_self").await,
        before,
        "the receiver wrote no ops of its own"
    );
}

// -- what one odd op may not do ------------------------------------------------

/// A JSON key, the way an op carries one.
fn key(of: &[&str]) -> String {
    serde_json::Value::Array(of.iter().map(|v| serde_json::Value::from(*v)).collect()).to_string()
}

fn op(origin: &str, seq: i64, hlc: i64, tbl: &str, key: String, col: &str, val: &str) -> Op {
    Op {
        origin: origin.to_string(),
        seq,
        hlc,
        tbl: tbl.to_string(),
        key,
        col: col.to_string(),
        val: serde_json::Value::from(val),
    }
}

/// An op for a table this build does not declare is kept in the log and not
/// applied — and the ops behind it still land.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_op_this_build_cannot_place_is_kept_and_stalls_nothing() {
    let (a, _ca) = device('a', 100.0);
    let z = id('z');
    let applied = a
        .apply_ops(vec![
            op(&z, 1, 1 << 16, "t_diary", key(&["d1"]), "body", "from a newer build"),
            op(&z, 2, 2 << 16, "t_note", key(&["n1"]), "body", "placed"),
            op(&z, 3, 3 << 16, "t_note", key(&["n1"]), "colour", "a column this build has not got"),
        ])
        .await
        .expect("the apply");
    assert_eq!(applied.kept, 3);
    assert_eq!(applied.applied, 1);
    assert_eq!(applied.skipped, 2);
    assert_eq!(applied.unknown, 2);
    assert_eq!(read(&a, NOTE).await, "/placed");
    assert_eq!(
        count(&a, "SELECT coalesce(max(seq), 0) FROM sync_have WHERE origin <> (SELECT device FROM sync_self)").await,
        3,
        "the run is contiguous, whatever this build could do with it"
    );
}

/// An op the schema refuses is kept, skipped, and reported once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_op_a_constraint_refuses_is_counted_and_passed_over() {
    let (a, _ca) = device('a', 100.0);
    let z = id('z');
    let applied = a
        .apply_ops(vec![
            op(&z, 1, 1 << 16, "t_note", key(&["n1"]), "body", "poison"),
            op(&z, 2, 2 << 16, "t_note", key(&["n1"]), "title", "still here"),
        ])
        .await
        .expect("the apply");
    assert_eq!(applied.applied, 1);
    assert_eq!(applied.refused, 1);
    assert!(applied.refusal.is_some(), "and says so, once");
    assert_eq!(read(&a, NOTE).await, "still here/");
}

/// A write that moves a key column is refused: a row's key is the same on
/// every device, and nothing of that transaction commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_column_may_not_change() {
    let (a, _ca) = device('a', 100.0);
    write(&a, MADE).await;
    let refused = a
        .write_async(|tx| {
            tx.execute_batch(
                "UPDATE t_note SET body = 'moved' WHERE uid = 'n1';
                 UPDATE t_note SET uid = 'n2' WHERE uid = 'n1';",
            )
        })
        .await;
    let said = refused.expect_err("a key column may not change").to_string();
    assert!(said.contains("t_note") && said.contains("key column"), "{said}");
    assert_eq!(read(&a, NOTE).await, "one/first", "and nothing of it stayed");
}

/// A write that moves nothing a device shares writes no op: a fetch
/// worker's bookkeeping stays on the machine that did the fetching.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_that_shares_nothing_logs_nothing() {
    let (a, _ca) = device('a', 100.0);
    write(&a, MADE).await;
    let before = count(&a, "SELECT next_seq FROM sync_self").await;
    write(&a, "UPDATE t_note SET local = 'fetched here' WHERE uid = 'n1'").await;
    assert_eq!(count(&a, "SELECT next_seq FROM sync_self").await, before);
}

/// A key a column defaulted is the key that travels, and a blob is a blob
/// on the other side: the note made here without a uid is the same note
/// there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_defaulted_key_and_a_blob_cross_unchanged() {
    let (a, _ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    write(&a, "INSERT INTO t_note(title, seal) VALUES('made here', x'00ff10')").await;
    let seal = "SELECT coalesce((SELECT uid || '/' || hex(seal) FROM t_note), 'gone')";
    let want = read(&a, seal).await;
    assert!(want.ends_with("/00FF10"), "{want}");
    let _wire = connect(&a, &b);
    eventually(&b, seal, &want).await;
}

/// A mark can arrive before there is anything to mark: the row is written
/// under its own key, whatever this device has fetched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mark_lands_before_the_thing_it_marks() {
    let (a, _ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    write(&a, "INSERT INTO t_mark(feed, guid, seen) VALUES('https://f', 'later', 1)").await;
    let _wire = connect(&a, &b);
    eventually(
        &b,
        "SELECT coalesce((SELECT CAST(seen AS TEXT) FROM t_mark
                           WHERE feed = 'https://f' AND guid = 'later'), 'nothing')",
        "1",
    )
    .await;
}

// -- who may talk --------------------------------------------------------------

/// What a listener makes of a dialer, as the service will: a device in
/// `sync_peer` and not removed proceeds, a stranger with the right secret
/// is added, anything else is refused.
fn listens(
    peers: Vec<(String, bool)>,
    secret: Option<[u8; 16]>,
) -> impl Fn(&Greeting) -> Verdict + Send + 'static {
    move |greeting: &Greeting| match peers.iter().find(|(device, _)| *device == greeting.device) {
        Some((_, removed)) if !*removed => Verdict::Known,
        Some(_) => Verdict::Refused,
        None if secret.is_some() && greeting.pairing == secret => Verdict::Pair,
        None => Verdict::Refused,
    }
}

/// What a dialer makes of whoever answered. A *pair with* adds it: the
/// ticket named that endpoint, and the transport proved the answerer holds
/// its key, so there is nothing further to show.
fn dials(peers: Vec<(String, bool)>, pairing: bool) -> impl Fn(&Greeting) -> Verdict + Send + 'static {
    move |greeting: &Greeting| match peers.iter().find(|(device, _)| *device == greeting.device) {
        Some((_, removed)) if !*removed => Verdict::Known,
        Some(_) => Verdict::Refused,
        None if pairing => Verdict::Pair,
        None => Verdict::Refused,
    }
}

async fn peers(db: &Arc<Db>) -> Vec<(String, bool)> {
    db.read_on_writer(|c| {
        c.prepare("SELECT device, removed FROM sync_peer")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    })
    .await
    .expect("the roster")
}

/// A stranger with the right secret is added to the roster and the exchange
/// begins; the wrong secret closes the connection before any `Have`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_takes_the_secret_and_nothing_else() {
    let secret = [7u8; 16];
    let (a, _ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    write(&a, MADE).await;

    // The wrong secret: the listener refuses, and nothing of the store
    // crosses.
    {
        let listens = listens(peers(&a).await, Some(secret));
        let dials = dials(peers(&b).await, true);
        let wire = wire(&a, &b, listens, dials, Side::Dial(Some([9u8; 16])));
        let refusal = (&mut { wire }.0).await.expect("the listener ended");
        assert!(matches!(refusal, Err(Error::Refused)), "{refusal:?}");
    }
    assert_eq!(read(&b, NOTE).await, "gone", "a stranger learns nothing");
    assert_eq!(count(&b, "SELECT count(*) FROM sync_peer").await, 1);

    // The right one: the dialer joins the roster on both ends, and the
    // exchange follows.
    let listens = listens(peers(&a).await, Some(secret));
    let dials = dials(peers(&b).await, true);
    let _wire = wire(&a, &b, listens, dials, Side::Dial(Some(secret)));
    eventually(&b, NOTE, "one/first").await;
    assert_eq!(count(&a, "SELECT count(*) FROM sync_peer WHERE removed = 0").await, 2);
}

/// A device that was forgotten is refused from the moment that op lands,
/// whichever end it dials.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forgotten_device_is_refused() {
    let (a, _ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    write(&a, MADE).await;
    a.add_peer(&id('b'), "device b").await.expect("the roster");
    b.add_peer(&id('a'), "device a").await.expect("the roster");
    a.write_async(|tx| tx.execute("UPDATE sync_peer SET removed = 1 WHERE name = 'device b'", []))
        .await
        .expect("forget it");
    // The forgotten device still believes they are paired; the refusal is
    // the other end's.
    let listens = listens(peers(&a).await, None);
    let dials = dials(peers(&b).await, false);
    let wire = wire(&a, &b, listens, dials, Side::Dial(None));
    let refusal = (&mut { wire }.0).await.expect("the listener ended");
    assert!(matches!(refusal, Err(Error::Refused)), "{refusal:?}");
    assert_eq!(read(&b, NOTE).await, "gone");
}

// -- the open check ------------------------------------------------------------

fn refusal(decl: Replicated) -> String {
    match Db::open(None, &[&SCHEMA], Device::fake().replicating(vec![decl])) {
        Ok(_) => panic!("that declaration should not have opened"),
        Err(e) => e.to_string(),
    }
}

/// A declaration that does not fit the schema is refused in one line, the
/// way a kernel version mismatch is.
#[test]
fn a_declaration_the_schema_cannot_keep_is_refused_in_one_line() {
    let said = refusal(Replicated {
        table: "t_note",
        key: &["title"],
        columns: &["body"],
    });
    assert!(said.contains("t_note") && said.contains("no unique index"), "{said}");

    let said = refusal(Replicated {
        table: "t_diary",
        key: &["uid"],
        columns: &["body"],
    });
    assert!(said.contains("no such table"), "{said}");

    let said = refusal(Replicated {
        table: "t_note",
        key: &["uid"],
        columns: &["uid"],
    });
    assert!(said.contains("both its key"), "{said}");

    // A table SQLite records nothing for could never replicate at all.
    let said = refusal(Replicated {
        table: "t_loose",
        key: &["name"],
        columns: &["value"],
    });
    assert!(said.contains("no primary key"), "{said}");

    // And what does fit opens.
    Db::open(None, &[&SCHEMA], Device::fake().replicating(DECLARED.to_vec())).expect("the real one");
}
