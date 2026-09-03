//! The send pipeline: a compose panel's **draft** persists in the store;
//! *send* is an undoable action that files an **outbox** row with a deadline
//! (`send_after = now + delay`). This pass claims due rows —
//! `WHERE status='pending'`, so the race against undo has exactly one winner
//! — and files a [`mail::Submit`] job. The executor is what actually speaks
//! SMTP, appends to Sent, and records the outcome.
//!
//! The split matters on a crash: the outbox row and the job are both
//! durable, so a mail hit `send` and never left goes out late rather than
//! never. And because `Submit` declares itself **not idempotent**, a job
//! caught mid-flight by a crash is failed rather than retried — nobody
//! double-sends on a guess.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crate::effect::{Clock, Scope, Secrets, World};
use crate::mail;

/// One pass over the outbox: claim everything due and queue it. Answers how
/// many rows were claimed. Also reconciles rows whose job has given up, so
/// a permanent failure reaches the compose panel.
pub fn outbox_pass(w: &World) -> usize {
    let now = w.now();
    let due: Vec<i64> = {
        let Ok(mut stmt) = w.store().conn().prepare(
            "SELECT id FROM outbox WHERE status = 'pending' AND send_after <= ?1 ORDER BY id",
        ) else {
            return 0;
        };
        stmt.query_map([now], |r| r.get(0))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    };

    let mut claimed = 0;
    for id in due {
        // The submit job is encoded outside the write — the payload and the
        // clock need the `World`, which cannot cross to the writer thread
        // (CR-005 phase 0).
        let Ok(job) = w.prepare(&mail::Submit { outbox: id }) else {
            continue;
        };
        // The claim: one winner between this pass and a concurrent undo,
        // whose reversal only deletes the row while it is 'pending'.
        let won = w
            .store()
            .write(move |tx| {
                let n = tx.execute(
                    "UPDATE outbox SET status = 'sending' WHERE id = ?1 AND status = 'pending'",
                    [id],
                )?;
                if n == 1 {
                    job.insert(tx)?;
                }
                Ok(n)
            })
            .unwrap_or(0);
        claimed += won;
    }

    // A job that has given up leaves its outbox row stranded at 'sending'.
    // Derive the failure back onto the row rather than teaching the effect
    // machinery about outboxes.
    let _ = w.store().write(|tx| {
        tx.execute(
            "UPDATE outbox SET status = 'failed',
                    error = (SELECT e.error FROM effect e
                             WHERE e.kind = 'submit' AND e.status = 'failed'
                               AND e.payload ->> 'outbox' = outbox.id)
             WHERE status = 'sending'
               AND EXISTS (SELECT 1 FROM effect e
                           WHERE e.kind = 'submit' AND e.status = 'failed'
                             AND e.payload ->> 'outbox' = outbox.id)",
            [],
        )
        .map(|_| ())
    });

    claimed
}

/// A handle to wake the sender (a send action was just filed).
pub struct Sender {
    kick: mpsc::Sender<()>,
}

impl Sender {
    pub fn kick(&self) {
        let _ = self.kick.send(());
    }
}

/// The sender thread: sleep until the next deadline (or a kick), claim what
/// is due, run the queue, notify the UI. It builds its own [`World`] — its
/// own reader over the shared writer (CR-005 phase 0), its own `Real`
/// outside.
///
/// # Panics
///
/// If the thread cannot be spawned.
pub fn spawn(
    db: Arc<crate::store::Db>,
    secrets: Secrets,
    clock: Clock,
    notify: impl Fn() + Send + 'static,
) -> Sender {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("sender".into())
        .spawn(move || {
            // Its own reader over the *one* writer (CR-005 phase 0).
            let Ok(store) = crate::store::Store::with_db(db) else {
                return;
            };
            let w = World::new(
                std::rc::Rc::new(store),
                Box::new(crate::effect::Real::new(secrets, clock)),
                mail::registry(),
            );
            loop {
                let did = outbox_pass(&w);
                // Only what needs no session. This thread has none: a
                // `move` claimed here would fail with "not connected" and
                // wait out a backoff the account's worker never asked for.
                let ran = w.run_effects_in(Scope::Sessionless);
                if did > 0 || ran > 0 {
                    notify();
                }
                // Sleep until the next deadline, capped — kicks cut it short.
                let next: Option<f64> = w
                    .store()
                    .conn()
                    .query_row(
                        "SELECT MIN(send_after) FROM outbox WHERE status = 'pending'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(None);
                let wait = next
                    .map(|t| (t - w.now()).clamp(0.2, 30.0))
                    .unwrap_or(30.0);
                match rx.recv_timeout(Duration::from_secs_f64(wait)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("spawn sender");
    Sender { kick: tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::World;
    use crate::history::Intent;

    /// A world with one account, one draft and one queued send. No temp
    /// directory, no keychain, no PID — the fixture this replaces shared a
    /// `temp_dir()/superapp-send-{pid}` with every other test in the
    /// process and deleted it on the way out.
    fn world(smtp: &str) -> World {
        let w = World::fake(mail::registry());
        let smtp = smtp.to_string();
        w.store()
            .write(move |c| {
                c.execute(
                    "INSERT INTO account(label, email, imap_host, smtp_host)
                     VALUES('t','t@t','',?1)",
                    [smtp],
                )?;
                c.execute(
                    "INSERT INTO draft(panel, account, to_addr, subject, body)
                     VALUES(9, 1, 'x@y', 'Hi', 'Body')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO outbox(id, account, send_after) VALUES(9, 1, 100.0)",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        w.with_fake(|f| f.keychain("t@t", "pw"));
        w
    }

    fn outbox(w: &World) -> (String, Option<String>) {
        w.store()
            .conn()
            .query_row("SELECT status, error FROM outbox WHERE id=9", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    }

    /// Not due → untouched. Due → claimed once, queued, submitted.
    #[test]
    fn a_due_send_becomes_a_job_and_leaves() {
        let w = world("smtp.t");
        w.with_fake(|f| f.clock = 99.0);
        assert_eq!(outbox_pass(&w), 0, "not due yet");
        assert_eq!(outbox(&w).0, "pending");
        assert!(w.jobs().is_empty(), "nothing queued before the deadline");

        w.with_fake(|f| f.clock = 100.5);
        assert_eq!(outbox_pass(&w), 1);
        let j = &w.jobs()[0];
        assert_eq!((j.kind.as_str(), j.status.as_str()), ("submit", "pending"));
        assert_eq!(j.entity.as_deref(), Some("outbox:9"));
        assert!(w.with_fake(|f| f.server(1).submitted.is_empty()), "not yet");

        assert_eq!(w.run_effects(), 1);
        assert_eq!(outbox(&w).0, "sent");
        assert_eq!(w.jobs()[0].status, "done");
        let sent = w.with_fake(|f| f.server(1).submitted.clone());
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].to.as_str(), sent[0].subject.as_str()), ("x@y", "Hi"));

        // Idempotent at the pass level: a second sweep claims nothing.
        assert_eq!(outbox_pass(&w), 0);
        assert_eq!(w.run_effects(), 0);
    }

    /// The regression this scoping exists for: the sender holds no IMAP
    /// session, so it leaves an account's push job for the worker that
    /// does. It used to claim the move, fail it with "not connected", and
    /// leave it sitting out a backoff — pending, pending, done.
    #[test]
    fn the_sender_does_not_steal_an_accounts_push() {
        let w = world("smtp.t");
        let mv = w
            .enqueue(&mail::Move {
                account: 1,
                message: 1,
                to_folder: 2,
                from: "INBOX".into(),
                to: "Trash".into(),
                uid: 5,
            })
            .unwrap();
        w.with_fake(|f| f.clock = 200.0);

        assert_eq!(outbox_pass(&w), 1);
        assert_eq!(w.run_effects_in(Scope::Sessionless), 1, "the submit, not the move");

        let job = |id: i64| w.jobs().into_iter().find(|j| j.id == id).unwrap();
        let m = job(mv);
        assert_eq!(m.status, "pending", "untouched");
        assert_eq!(m.attempts, 0, "no attempt burned");
        assert!(m.error.is_none(), "and nothing to explain to a human");
        assert_eq!(outbox(&w).0, "sent", "while the send still went");

        // The account's own worker is what takes it.
        assert_eq!(w.run_effects_in(Scope::Account(1)), 1);
    }

    /// A draft that carries files (CR-010) reads them **at submit time**,
    /// through the outside — so what goes out is the file as it stands,
    /// and a file that is no longer there fails the send rather than
    /// sending nothing under its name.
    #[test]
    fn a_send_carries_the_files_the_draft_named() {
        let w = world("smtp.t");
        w.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO draft_attachment(panel, path, name, size, added)
                     VALUES(9, '~/Downloads/README.txt', 'README.txt', 640, 0)",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        w.with_fake(|f| f.clock = 200.0);
        assert_eq!(outbox_pass(&w), 1);
        assert_eq!(w.run_effects(), 1);
        assert_eq!(outbox(&w).0, "sent");

        let sent = w.with_fake(|f| f.server(1).submitted.clone());
        assert_eq!(sent.len(), 1);
        let parts = &sent[0].attachments;
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].name.as_str(), parts[0].mime.as_str()), ("README.txt", "text/plain"));
        // The demo tree's own reading of that file — read now, not copied
        // into the store when it was attached.
        assert!(String::from_utf8_lossy(&parts[0].bytes).starts_with("superapp 0.1.0"));

        // A file the outside cannot read fails the send with a sentence
        // naming it, rather than quietly sending the letter without it.
        w.store()
            .write(|c| {
                c.execute("UPDATE outbox SET status='pending', send_after=0 WHERE id=9", [])?;
                c.execute(
                    "UPDATE draft_attachment SET path='~/Downloads/gone.txt', name='gone.txt'",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        outbox_pass(&w);
        for _ in 0..8 {
            w.with_fake(|f| f.clock += 3600.0);
            w.run_effects();
        }
        let e = w.jobs().last().and_then(|j| j.error.clone()).unwrap_or_default();
        assert!(e.contains("cannot attach “gone.txt”"), "{e}");
        assert_eq!(
            w.with_fake(|f| f.server(1).submitted.len()),
            1,
            "the second letter never left"
        );
    }

    /// Undo inside the window deletes the pending row, so the pass never
    /// claims it and the mail never leaves.
    #[test]
    fn undo_inside_the_window_stops_the_send() {
        let w = world("smtp.t");
        w.store()
            .write(|c| c.execute("DELETE FROM outbox WHERE id=9 AND status='pending'", []))
            .unwrap();
        w.with_fake(|f| f.clock = 200.0);
        assert_eq!(outbox_pass(&w), 0);
        w.run_effects();
        assert!(w.jobs().is_empty(), "no job was ever filed");
        assert!(w.with_fake(|f| f.server(1).submitted.is_empty()));
    }

    /// An account with no SMTP host fails honestly, and the failure reaches
    /// the outbox row rather than dying inside the executor.
    #[test]
    fn a_send_that_cannot_work_fails_visibly() {
        let w = world("");
        w.with_fake(|f| f.clock = 200.0);
        outbox_pass(&w);
        // Exhaust the retries; the world is not going to grow an smtp host.
        for _ in 0..8 {
            w.with_fake(|f| f.clock += 3600.0);
            w.run_effects();
        }
        assert_eq!(w.jobs()[0].status, "failed");
        assert_eq!(
            w.jobs()[0].error.as_deref(),
            Some("account has no smtp host")
        );
        outbox_pass(&w); // reconciles the failure back onto the row
        let (status, error) = outbox(&w);
        assert_eq!(status, "failed");
        assert_eq!(error.as_deref(), Some("account has no smtp host"));
        assert!(w.with_fake(|f| f.server(1).submitted.is_empty()));
    }

    /// Exhausts the executor on a send that cannot work, so the row is
    /// `failed` and the problems panel would offer retry and reopen.
    fn give_up(w: &World) {
        outbox_pass(w);
        for _ in 0..8 {
            w.with_fake(|f| f.clock += 3600.0);
            w.run_effects();
        }
        outbox_pass(w);
        assert_eq!(outbox(w).0, "failed");
    }

    /// A retry files the row again — and the job that failed last time
    /// stands down, or the pass would fail the fresh filing on sight with
    /// the old error.
    #[test]
    fn a_retry_is_not_failed_by_its_own_history() {
        let w = world("");
        w.with_fake(|f| f.clock = 200.0);
        give_up(&w);

        let now = w.now();
        w.store()
            .write(move |c| mail::file_send_tx(c, 9, now + 1.0))
            .unwrap();
        assert_eq!(outbox(&w), ("pending".into(), None));
        assert!(
            w.jobs().iter().all(|j| j.status == "obsolete"),
            "the old failure is history: {:?}",
            w.jobs()
        );

        w.with_fake(|f| f.clock += 2.0);
        assert_eq!(outbox_pass(&w), 1, "claimed again");
        assert_eq!(outbox(&w).0, "sending", "a fresh attempt, not the stale verdict");
        let live: Vec<_> = w.jobs().into_iter().filter(|j| j.status != "obsolete").collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].attempts, 0);
    }

    /// Undoing a retry puts the failure back — the row and the job that
    /// stood down — rather than deleting the row as undoing a send does:
    /// there is no compose to reopen, so a deleted row would strand the
    /// draft.
    #[test]
    fn a_retry_undone_puts_the_failure_back() {
        let w = world("");
        w.with_fake(|f| f.clock = 200.0);
        give_up(&w);
        let error = outbox(&w).1.unwrap();
        let attempts_before = w.jobs()[0].attempts;

        let intent = mail::Retried {
            outbox: 9,
            error: error.clone(),
            delay: 1.0,
        };
        intent.reapply(&w).unwrap(); // the retry itself: the same filing
        assert_eq!(outbox(&w), ("pending".into(), None));
        assert!(w.jobs().iter().all(|j| j.status == "obsolete"));
        assert!(intent.blocked(&w).is_none(), "still in the window");

        intent.reverse(&w).unwrap();
        assert_eq!(outbox(&w), ("failed".into(), Some(error)));
        let live: Vec<_> = w.jobs().into_iter().filter(|j| j.status != "obsolete").collect();
        assert_eq!(live.len(), 1, "the old job stands again");
        assert_eq!((live[0].status.as_str(), live[0].attempts), ("failed", attempts_before));
        assert!(mail::draft(w.store(), 9).is_some(), "the draft is where the row finds it");

        // Once the executor has the retried row, it is a send like any other.
        intent.reapply(&w).unwrap();
        w.with_fake(|f| f.clock += 2.0);
        outbox_pass(&w);
        assert_eq!(outbox(&w).0, "sending");
        assert_eq!(intent.blocked(&w).as_deref(), Some("already sent"));
    }

    /// Reopening a failed send moves the draft under the compose panel that
    /// shows it and clears the row; giving it back restores both, with the
    /// error the row carried.
    #[test]
    fn a_reopened_send_comes_back_on_undo() {
        let w = world("");
        w.with_fake(|f| f.clock = 200.0);
        give_up(&w);
        let error = outbox(&w).1.unwrap();

        let now = w.now();
        w.store()
            .write(move |c| mail::reopen_send_tx(c, 9, 42, now))
            .unwrap();
        let draft_of = |panel: i64| mail::draft(w.store(), panel);
        assert!(draft_of(9).is_none(), "the draft moved");
        assert_eq!(draft_of(42).unwrap().body, "Body");
        let rows: i64 = w
            .store()
            .conn()
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "the failed row is gone");

        let minted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(42));
        let intent = mail::Reopened {
            old: 9,
            new: minted,
            error: error.clone(),
        };
        assert!(intent.blocked(&w).is_none());
        intent.reverse(&w).unwrap();
        assert!(draft_of(42).is_none());
        assert_eq!(draft_of(9).unwrap().body, "Body");
        assert_eq!(outbox(&w), ("failed".into(), Some(error)));

        intent.reapply(&w).unwrap();
        assert!(draft_of(9).is_none());
        assert_eq!(draft_of(42).unwrap().to, "x@y");

        // Once the reopened draft has gone out, there is nothing to put back.
        w.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO outbox(id, account, send_after, status) VALUES(42, 1, 0, 'sent')",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        assert_eq!(intent.blocked(&w).as_deref(), Some("already sent"));
    }

    /// A send caught mid-flight by a crash is never retried on a guess —
    /// the whole reason `Submit::idempotent` is `false`.
    #[test]
    fn an_interrupted_send_is_never_guessed_at() {
        let w = world("smtp.t");
        w.with_fake(|f| f.clock = 200.0);
        outbox_pass(&w);
        w.store()
            .write(|c| {
                c.execute("UPDATE effect SET status='processing'", [])
                    .map(|_| ())
            })
            .unwrap();

        w.store().write(|tx| crate::store::sweep_effects(tx)).unwrap();

        let j = &w.jobs()[0];
        assert_eq!(j.status, "failed");
        assert_eq!(j.error.as_deref(), Some("interrupted; outcome unknown"));
        assert_eq!(w.run_effects(), 0, "not retried");
        assert!(
            w.with_fake(|f| f.server(1).submitted.is_empty()),
            "nobody double-sent"
        );
    }

    /// A Gmail account sends on its bearer token: the submission carries
    /// the grant, not a password — and there is no password to carry.
    #[test]
    fn a_google_account_sends_on_its_token() {
        let w = World::fake(mail::registry());
        w.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO account(label, email, imap_host, smtp_host, auth)
                     VALUES('g','g@gmail.com','imap.gmail.com','smtp.gmail.com','google')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO draft(panel, account, to_addr, subject, body)
                     VALUES(9, 1, 'x@y', 'Hi', 'Body')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO outbox(id, account, send_after) VALUES(9, 1, 100.0)",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        w.with_fake(|f| {
            f.grant("g@gmail.com", "ya29.token");
            f.clock = 200.0;
        });

        assert_eq!(outbox_pass(&w), 1);
        assert_eq!(w.run_effects(), 1);
        assert_eq!(outbox(&w).0, "sent");
        let sent = w.with_fake(|f| f.server(1).submitted.clone());
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, "x@y");

        // And it is filed exactly once: Gmail's SMTP puts its own copy in
        // Sent Mail, so the APPEND every other account gets is skipped —
        // one letter, not two.
        assert!(
            w.with_fake(|f| !f.server(1).folders.contains_key("Sent")),
            "gmail's sent copy is the server's, not ours"
        );
        assert_eq!(w.jobs()[0].status, "done");

        // Revoke the grant and the next send fails where it should: at
        // the token, before anything leaves.
        w.with_fake(|f| f.revoke("g@gmail.com"));
        let e = w
            .outside(|o| mail::creds_for(o, "g@gmail.com", "smtp.gmail.com", true))
            .unwrap_err();
        assert!(e.contains("invalid_grant"), "{e}");
    }
}
