//! Two or three stores, a duplex stream between them, and virtual time.
//!
//! Every device here has a clock of its own that only moves when it is
//! moved, so *later* is something these tests decide rather than something
//! they wait for.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
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
CREATE TABLE t_stiff(
  uid  TEXT PRIMARY KEY,
  need TEXT NOT NULL,
  seen INTEGER NOT NULL DEFAULT 0
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

/// Two devices that both wake with a backlog. Each one's ops go out while
/// the other's are coming in: a side that sent its whole log before reading
/// a frame would fill the pipe and then wait for a reader that is itself
/// waiting to be read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_backlogs_cross_without_either_side_stopping() {
    let (a, _ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    for (db, mine) in [(&a, "a"), (&b, "b")] {
        db.write_async(move |tx| {
            tx.execute(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 3000)
                 INSERT INTO t_note(uid, title, body)
                      SELECT ?1 || x, 'note', replace(hex(zeroblob(256)), '0', 'x') FROM n",
                [mine],
            )
        })
        .await
        .expect("a backlog");
    }
    // Four cells a note, so twelve thousand ops go each way, and no frame
    // can hold more than a batch of them.
    assert_eq!(wrote(&a).await, 3 + 12_000);

    let _wire = connect(&a, &b);
    let crossed = async {
        loop {
            let here = count(&a, "SELECT count(*) FROM t_note").await;
            let there = count(&b, "SELECT count(*) FROM t_note").await;
            if (here, there) == (6000, 6000) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(60), crossed)
        .await
        .expect("both sides kept reading while their own backlog went out");
}

/// A backlog: four hundred notes, four cells each, with bodies long enough
/// that one frame of them is larger than any pipe here.
const BACKLOG: &str = "
WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n WHERE x < 400)
INSERT INTO t_note(uid, title, body)
     SELECT 'n' || x, 'note', replace(hex(zeroblob(256)), '0', 'x') FROM n";

/// The four bytes of length in front of a frame.
async fn length(wire: &mut DuplexStream) -> usize {
    let mut len = [0u8; 4];
    wire.read_exact(&mut len).await.expect("a length");
    u32::from_be_bytes(len) as usize
}

/// One whole frame off the wire, and one onto it. A far side that greets
/// and then stops reading is not something a second `run` would ever do, so
/// these tests speak for it.
async fn take(wire: &mut DuplexStream) -> serde_json::Value {
    let mut body = vec![0u8; length(wire).await];
    wire.read_exact(&mut body).await.expect("a frame");
    serde_json::from_slice(&body).expect("a frame")
}

async fn put(wire: &mut DuplexStream, frame: serde_json::Value) {
    let body = serde_json::to_vec(&frame).expect("the frame");
    let n = u32::try_from(body.len()).expect("the length");
    wire.write_all(&n.to_be_bytes()).await.expect("the length");
    wire.write_all(&body).await.expect("the frame");
}

/// A session that is cancelled takes its writer with it. The service ends a
/// session by dropping it — which is what **forget** does — and a writer
/// left behind, blocked inside a send on a full pipe, would go on writing
/// to a device this one has just dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_session_writes_nothing_more() {
    /// Smaller than one frame of the backlog, so the writer is inside a
    /// send that cannot finish.
    const PIPE: usize = 1024;

    let (a, _ca) = device('a', 100.0);
    write(&a, BACKLOG).await;
    let (mine, mut theirs) = tokio::io::duplex(PIPE);
    let session = tokio::spawn(protocol::run(a.clone(), mine, Side::Listen, known, a.ops_landed()));

    // The far side greets, and from then on reads nothing but the length in
    // front of the first frame of the backlog.
    assert_eq!(take(&mut theirs).await["frame"], "hello");
    let hello = serde_json::json!({
        "frame": "hello", "v": protocol::VERSION, "device": id('b'), "name": "device b"
    });
    put(&mut theirs, hello).await;
    assert_eq!(take(&mut theirs).await["frame"], "have");
    put(&mut theirs, serde_json::json!({ "frame": "have", "have": [] })).await;
    let frame = length(&mut theirs).await;
    assert!(frame > PIPE, "a frame of {frame} bytes does not fill a pipe of {PIPE}");

    // Cancelled: the handle answers once the session's future is dropped,
    // which is when its halves are aborted.
    session.abort();
    assert!(session.await.expect_err("the session was cancelled").is_cancelled());

    // What the pipe already held crosses, and then it is at an end —
    // nothing is left holding the stream open and writing on.
    let (mut got, mut buf) = (0, vec![0u8; 4096]);
    loop {
        let read = tokio::time::timeout(Duration::from_secs(5), theirs.read(&mut buf));
        let n = read.await.expect("the wire ended").expect("the read");
        if n == 0 {
            break;
        }
        got += n;
    }
    assert!(got <= PIPE, "{got} bytes of a {frame}-byte frame crossed after the session ended");
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

/// Every column but the key needs a default or has to take NULL, whether
/// it travels or not: a row arrives one cell at a time, so the insert that
/// makes it names the key, that one column, and nothing else.
#[test]
fn a_required_column_is_refused_even_when_it_travels() {
    let said = refusal(Replicated {
        table: "t_stiff",
        key: &["uid"],
        columns: &["seen"],
    });
    assert!(said.contains("need is required"), "{said}");

    // Declaring it does not excuse it. The first cell of the row to arrive
    // may be any of them, and SQLite checks NOT NULL before it looks for
    // the conflict that would have turned the insert into an update.
    let said = refusal(Replicated {
        table: "t_stiff",
        key: &["uid"],
        columns: &["need"],
    });
    assert!(said.contains("need is required"), "{said}");
}

// -- the roster ----------------------------------------------------------------

/// A device renamed or forgotten on one device is renamed or forgotten on
/// every other: `sync_peer` replicates like any app's table, and a row
/// another device made lands whole on the first apply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_renamed_or_forgotten_reaches_the_other_devices() {
    let (a, _ca) = device('a', 100.0);
    let (b, _cb) = device('b', 100.0);
    a.add_peer(&id('c'), "tablet").await.expect("the roster");
    b.apply_ops(a.ops_since(vec![], 1000).await.expect("what a holds"))
        .await
        .expect("the apply");
    let roster = "SELECT coalesce((SELECT name || '/' || removed FROM sync_peer
                                    WHERE device = (SELECT device FROM sync_peer
                                                     WHERE name = 'tablet')), 'gone')";
    assert_eq!(read(&b, roster).await, "tablet/0", "the whole row, at once");

    // And an update, whose ops carry only the two columns that moved.
    a.write_async(|tx| {
        tx.execute(
            "UPDATE sync_peer SET name = 'the tablet', removed = 1 WHERE name = 'tablet'",
            [],
        )
    })
    .await
    .expect("the rename");
    let have = b.have().await.expect("what b holds");
    let applied = b
        .apply_ops(a.ops_since(have, 1000).await.expect("the rest"))
        .await
        .expect("the apply");
    assert_eq!(applied.refused, 0, "{:?}", applied.refusal);
    assert_eq!(
        read(&b, "SELECT name || '/' || removed FROM sync_peer WHERE name LIKE 'the %'").await,
        "the tablet/1"
    );
}

/// The roster as the first build of this wrote it: `added` required, with
/// no default, so no op could write a peer's row. The shape is corrected
/// by presence at the next open, and what was in it stays.
#[test]
fn a_roster_of_the_old_shape_is_rebuilt_at_open() {
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().join("store.db");
    drop(Db::open(Some(&path), &[&SCHEMA], Device::new(id('a'))).expect("a store"));
    {
        let conn = crate::store::bare(&path).expect("the file");
        conn.execute_batch(
            "ALTER TABLE sync_peer RENAME TO sync_peer_was;
             CREATE TABLE sync_peer(
               device  TEXT PRIMARY KEY,
               name    TEXT NOT NULL DEFAULT '',
               added   REAL NOT NULL,
               removed INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO sync_peer SELECT * FROM sync_peer_was;
             DROP TABLE sync_peer_was;",
        )
        .expect("the old shape");
    }
    let db = Db::open(Some(&path), &[&SCHEMA], Device::new(id('a'))).expect("the second open");
    let required: i64 = crate::runtime::block_on(db.read_on_writer(|c| {
        c.query_row(
            "SELECT count(*) FROM pragma_table_info('sync_peer')
              WHERE name = 'added' AND \"notnull\" = 1 AND dflt_value IS NULL",
            [],
            |r| r.get(0),
        )
    }))
    .expect("the shape");
    assert_eq!(required, 0, "added takes a default now");
    assert_eq!(
        crate::runtime::block_on(count(&db, "SELECT count(*) FROM sync_peer")),
        1,
        "and this device is still in it"
    );
}

// -- what was here before the log was -------------------------------------------

/// One store on disk, opened again — which is where a backfill happens.
fn store(path: &std::path::Path, c: char, clock: &ClockSource, declaring: &[Replicated]) -> Arc<Db> {
    Db::open(
        Some(path),
        &[&SCHEMA],
        Device::new(id(c))
            .named(format!("device {c}"))
            .clocked(clock.clone())
            .replicating(declaring.to_vec()),
    )
    .expect("a store")
}

/// This device's own count, which is how many ops it has ever written.
async fn wrote(db: &Arc<Db>) -> i64 {
    count(db, "SELECT next_seq - 1 FROM sync_self").await
}

/// A note that was in the store before device sync was: nothing described
/// it, and nothing won a cell of it, so a device paired afterwards would
/// never be told it existed. Every open files the missing cells at
/// `hlc = 0`, and the second open files nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_that_predate_the_log_reach_a_device_paired_later() {
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().join("store.db");
    let clock = ClockSource::virtual_from(100.0);
    {
        // The store as an older build left it: the row is there and the
        // log is not.
        let old = store(&path, 'a', &clock, DECLARED);
        write(&old, MADE).await;
        drop(old);
        let conn = crate::store::bare(&path).expect("the file");
        conn.execute_batch(
            "DROP TABLE sync_op; DROP TABLE sync_cell; DROP TABLE sync_have;
             DROP TABLE sync_self; DROP TABLE sync_peer; DROP TABLE sync_link;",
        )
        .expect("the log, gone");
    }
    let a = store(&path, 'a', &clock, DECLARED);
    // Its own row in the roster, then a cell of the note for each column
    // that travels.
    assert_eq!(wrote(&a).await, 3 + 4);
    assert_eq!(
        count(&a, "SELECT count(*) FROM sync_op WHERE hlc = 0 AND tbl = 't_note'").await,
        4,
        "under every real op there could be"
    );

    let (b, _cb) = device('b', 100.0);
    let _wire = connect(&a, &b);
    eventually(&b, NOTE, "one/first").await;

    // And a second open files nothing: the first left a winner behind for
    // every cell it touched.
    drop(_wire);
    let at = wrote(&a).await;
    drop(a);
    let a = store(&path, 'a', &clock, DECLARED);
    assert_eq!(wrote(&a).await, at, "the backfill runs once and says so once");
}

/// A build that declares one more column of a table it already replicated
/// backfills that column, and only that column.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_column_a_later_build_declares_is_backfilled_too() {
    static THIN: &[Replicated] = &[Replicated {
        table: "t_note",
        key: &["uid"],
        columns: &["title"],
    }];
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().join("store.db");
    let clock = ClockSource::virtual_from(100.0);
    let a = store(&path, 'a', &clock, THIN);
    write(&a, MADE).await;
    let at = wrote(&a).await;
    drop(a);

    let a = store(&path, 'a', &clock, DECLARED);
    assert_eq!(wrote(&a).await, at + 3, "body, seal and deleted, and no second title");
    let (b, _cb) = device('b', 100.0);
    let _wire = connect(&a, &b);
    eventually(&b, NOTE, "one/first").await;
}

/// Two devices that once shared a lineage hold the same note and no op for
/// it. Each backfills its own copy at `hlc = 0`; an edit made on one of
/// them since is above both, so it is what they settle on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_beats_a_backfill_of_the_same_row() {
    let dir = tempfile::tempdir().expect("a directory");
    let (clock, mut stores) = (ClockSource::virtual_from(100.0), Vec::new());
    for c in ['a', 'b'] {
        let path = dir.path().join(format!("{c}.db"));
        {
            // Written before anything was declared, so no op describes it:
            // the one lineage both devices came out of.
            let before = Db::open(
                Some(&path),
                &[&SCHEMA],
                Device::new(id(c)).clocked(clock.clone()),
            )
            .expect("a store");
            write(&before, MADE).await;
        }
        stores.push(store(&path, c, &clock, DECLARED));
    }
    let (a, b) = (stores.remove(0), stores.remove(0));
    assert_eq!(
        count(&a, "SELECT count(*) FROM sync_op WHERE hlc > 0 AND tbl = 't_note'").await,
        0,
        "nothing about the note has a clock yet"
    );
    clock.advance(10.0);
    write(&a, "UPDATE t_note SET body = 'from a' WHERE uid = 'n1'").await;
    let _wire = connect(&a, &b);
    eventually(&b, NOTE, "one/from a").await;
    eventually(&a, NOTE, "one/from a").await;
}
