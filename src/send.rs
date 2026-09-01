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

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use crate::effect::{Clock, Secrets, World};
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
        // The claim: one winner between this pass and a concurrent undo,
        // whose reversal only deletes the row while it is 'pending'.
        let won = w
            .store()
            .write(|tx| {
                let n = tx.execute(
                    "UPDATE outbox SET status = 'sending' WHERE id = ?1 AND status = 'pending'",
                    [id],
                )?;
                if n == 1 {
                    w.enqueue_in(tx, &mail::Submit { outbox: id })?;
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
/// own store connection, its own `Real` outside.
///
/// # Panics
///
/// If the thread cannot be spawned.
pub fn spawn(
    db: PathBuf,
    secrets: Secrets,
    clock: Clock,
    notify: impl Fn() + Send + 'static,
) -> Sender {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("sender".into())
        .spawn(move || {
            let Ok(store) = crate::store::Store::open(Some(&db)) else {
                return;
            };
            let w = World::new(
                std::rc::Rc::new(store),
                Box::new(crate::effect::Real::new(secrets, clock)),
                mail::registry(),
            );
            loop {
                let did = outbox_pass(&w);
                let ran = w.run_effects();
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

    /// A world with one account, one draft and one queued send. No temp
    /// directory, no keychain, no PID — the fixture this replaces shared a
    /// `temp_dir()/superapp-send-{pid}` with every other test in the
    /// process and deleted it on the way out.
    fn world(smtp: &str) -> World {
        let w = World::fake(mail::registry());
        w.store()
            .write(|c| {
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

        crate::store::sweep_effects(w.store().conn()).unwrap();

        let j = &w.jobs()[0];
        assert_eq!(j.status, "failed");
        assert_eq!(j.error.as_deref(), Some("interrupted; outcome unknown"));
        assert_eq!(w.run_effects(), 0, "not retried");
        assert!(
            w.with_fake(|f| f.server(1).submitted.is_empty()),
            "nobody double-sent"
        );
    }
}
