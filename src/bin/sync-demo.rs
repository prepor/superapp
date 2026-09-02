//! A narrated two-device sync demo over the **real** components — the store,
//! the replication engine, and the object transport. No GUI, no emulator:
//! this is the mechanism the mac and android apps run, driven straight, so it
//! works in a headless environment where the window event loop does not.
//!
//! ```sh
//! cargo run --bin sync-demo
//! # …or against a real Cloudflare R2 bucket, which is the same walk over
//! # TLS and signed requests (CR-005 phase 4):
//! export SUPERAPP_R2_ACCESS_KEY_ID=… SUPERAPP_R2_SECRET_ACCESS_KEY=…
//! cargo run --bin sync-demo -- --bucket https://<account>.r2.cloudflarestorage.com/<bucket>
//! ```
//!
//! The real run puts its lineage under a fresh `sync-demo/<stamp>/` prefix and
//! deletes every object it made on the way out, so a bucket you use for
//! something else comes back as it was.

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use superapp::mail;
use superapp::object::{serve_conn, Cas, HttpBucket, Object, PutNew};
use superapp::r2::R2;
use superapp::repl::{self, Role};
use superapp::store::Store;

fn inbox(s: &Store) -> usize {
    mail::inbox(s).len()
}

fn archived(s: &Store, subject: &str) -> bool {
    // A mail is out of the inbox once archived.
    !mail::inbox(s).iter().any(|m| m.subject.contains(subject))
        && s.conn()
            .query_row(
                "SELECT COUNT(*) FROM message WHERE subject LIKE ?1",
                [format!("%{subject}%")],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0
}

fn step(n: &str) {
    println!("\n── {n} ──");
}

fn role(s: &repl::Status) -> String {
    format!("{:?}", s.role)
}

fn main() {
    // `--bucket URL` runs the whole walk against a real bucket instead of the
    // in-process stand-in. Everything below it is identical: the transport is
    // the only thing that changes.
    let mut args = std::env::args().skip(1);
    let mut url = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bucket" => url = args.next(),
            other => eprintln!("sync-demo: ignoring unknown argument {other:?}"),
        }
    }
    if let Some(url) = url {
        match real_bucket(&url) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("sync-demo: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    local_bucket();
}

/// The demo against a real R2 endpoint: a fresh lineage under its own prefix,
/// the same walk, then every object it wrote removed again.
fn real_bucket(url: &str) -> Result<(), String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let prefixed = format!("{}/sync-demo/{stamp}", url.trim_end_matches('/'));
    let bucket = R2::new(&prefixed, superapp::r2::creds(None)?)?;
    println!("bucket (real, signed, over TLS): {}", bucket.endpoint());

    // Whatever happens in there — a refusal, a failed assertion — the objects
    // this run made in someone's real bucket are this run's to remove. The
    // walk asserts by panicking, so catching one is the only way to get the
    // cleanup a chance to run.
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        contract(&bucket)?;
        walk(&bucket);
        Ok::<(), String>(())
    }));

    step("cleaning up: the lineage this run made");
    let swept = sweep(&bucket);
    match &swept {
        Ok(n) => println!("removed {n} objects under sync-demo/{stamp}/"),
        Err(e) => eprintln!("could not clean up sync-demo/{stamp}/: {e}"),
    }

    match ran {
        Ok(Ok(())) => swept.map(|_| ()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the walk did not finish — see the panic above".into()),
    }
}

/// Removes every object under this run's prefix; answers how many.
fn sweep(bucket: &R2) -> Result<usize, String> {
    let keys = bucket.list("")?;
    for key in &keys {
        bucket.delete(key)?;
    }
    Ok(keys.len())
}

/// The three verbs the lease rests on, against whatever bucket we were given
/// — run first, because a wrong key should say so here and not as a puzzling
/// bootstrap failure four steps later.
fn contract(bucket: &dyn Object) -> Result<(), String> {
    step("the transport: get, create-only put, compare-and-swap");
    let key = "contract-check";
    if bucket.get(key)?.is_some() {
        return Err(format!("{key} already exists — is this lineage fresh?"));
    }
    let PutNew::Created(_) = bucket.put_new(key, b"one")? else {
        return Err("a create-only put lost on an absent key".into());
    };
    assert_eq!(bucket.put_new(key, b"two")?, PutNew::Exists);
    let blob = bucket.get(key)?.ok_or("the object we just wrote is not there")?;
    assert_eq!(blob.bytes, b"one");
    assert_eq!(bucket.cas(key, b"x", "not-the-etag")?, Cas::Mismatch);
    let Cas::Ok(_) = bucket.cas(key, b"two", &blob.etag)? else {
        return Err("a compare-and-swap lost with the current etag".into());
    };
    assert_eq!(bucket.cas(key, b"z", &blob.etag)?, Cas::Mismatch);
    println!("404 → create → refuse → read → stale CAS refused → fresh CAS wins ✓");
    Ok(())
}

/// The demo against an in-process `bucketd` — no cloud account needed.
fn local_bucket() {
    // A real HTTP bucket, in-process (the same handler bucketd serves).
    let dir = std::env::temp_dir().join(format!("superapp-sync-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let (sdir, sstop) = (dir.clone(), stop.clone());
    let server = std::thread::spawn(move || {
        while !sstop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    s.set_nonblocking(false).ok();
                    let _ = serve_conn(&sdir, &mut s);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(_) => break,
            }
        }
    });

    let bucket = HttpBucket::new(&format!("http://{addr}"));
    println!("bucket (a local stand-in for R2/S3): http://{addr}");
    contract(&bucket).expect("the local bucket keeps the contract");
    walk(&bucket);

    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole lease lifecycle, over whichever bucket it is handed.
fn walk(bucket: &dyn Object) {
    let a = Store::open(None).unwrap();
    let b = Store::open(None).unwrap();
    println!("device A: {}", &a.device()[..12]);
    println!("device B: {}", &b.device()[..12]);

    step("A seeds the demo world and bootstraps the lineage");
    mail::seed_if_empty(&a).unwrap();
    let sa = repl::poll(&a, bucket);
    repl::poll(&a, bucket); // publish
    println!(
        "A role={}  inbox={}  writable={}",
        role(&sa),
        inbox(&a),
        a.is_writable()
    );
    assert_eq!(sa.role, Role::Holder);

    step("B joins: installs A's snapshot, becomes a read-only follower");
    let sb = repl::poll(&b, bucket);
    println!(
        "B role={}  inbox={}  writable={}  (the locked screen)",
        role(&sb),
        inbox(&b),
        b.is_writable()
    );
    assert!(matches!(sb.role, Role::Follower { .. }));
    assert_eq!(inbox(&b), inbox(&a), "B synced A's whole inbox");

    step("A follower's write is refused at the gate");
    let refused = b
        .write(|tx| tx.execute("UPDATE message SET unread=0 WHERE id=1", []).map(|_| ()))
        .is_err();
    println!("B write refused: {refused}");
    assert!(refused);

    step("A archives a mail; it syncs to B");
    a.write(|tx| mail::archive_tx(tx, 1)).unwrap();
    repl::poll(&a, bucket); // publish the archive
    repl::poll(&b, bucket); // B materializes it
    println!(
        "A inbox={}  B inbox={}  B sees 'Q3 infra' archived={}",
        inbox(&a),
        inbox(&b),
        archived(&b, "Q3 infra")
    );
    assert!(archived(&b, "Q3 infra"));

    step("A hands the lease back; B acquires it");
    let ra = repl::release(&a, bucket).unwrap();
    println!("A role after release={}  writable={}", role(&ra), a.is_writable());
    let free = repl::poll(&b, bucket);
    println!("B sees role={} — the lease is free", role(&free));
    let held = repl::acquire(&b, bucket).unwrap();
    println!("B role after acquire={}  writable={}", role(&held), b.is_writable());
    assert_eq!(held.role, Role::Holder);

    step("A now follows B — a clean handoff, no override");
    let af = repl::poll(&a, bucket);
    println!("A role={}", role(&af));
    assert!(matches!(af.role, Role::Follower { .. }));

    step("B writes; it flows back to A");
    b.write(|tx| {
        tx.execute("INSERT INTO account(label,email) VALUES('B-was-here','b@x')", [])
            .map(|_| ())
    })
    .unwrap();
    repl::poll(&b, bucket);
    repl::poll(&a, bucket);
    let on_a: i64 = a
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM account WHERE label='B-was-here'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    println!("A now has B's write: {}", on_a == 1);
    assert_eq!(on_a, 1);

    step("override: A takes the lease from B, which never released → B stranded");
    // The divergence has to be real: B captures a write it never publishes.
    // A holder overridden with nothing outstanding published everything it
    // wrote and follows cleanly — only unpublished work strands a device.
    b.write(|tx| {
        tx.execute("INSERT INTO account(label,email) VALUES('never-published','b2@x')", [])
            .map(|_| ())
    })
    .unwrap();
    let ov = repl::override_lease(&a, bucket).unwrap();
    println!("A override role={}  epoch={}", role(&ov), ov.epoch);
    let sb2 = repl::poll(&b, bucket);
    println!("B role={} — read-only, recovery by hand", role(&sb2));
    assert!(matches!(sb2.role, Role::Stranded { .. }));

    println!("\n✓ full lease lifecycle: bootstrap, install, sync both ways,");
    println!("  follower read-only, release+acquire handoff, and an override that strands.");
}
