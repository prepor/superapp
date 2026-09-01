//! The send pipeline: a compose panel's **draft** persists in the store;
//! *send* is an undoable action that files an **outbox** row with a
//! deadline (`send_after = now + delay`). This thread claims due rows —
//! `WHERE status='pending'`, so the race against undo has exactly one
//! winner — submits over SMTP (lettre, rustls), appends the sent mail to
//! the account's Sent folder over IMAP, and records the outcome. A send
//! past its deadline is irreversible: the undo guard marks its action
//! `expired` and the history walk skips it transparently.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use rusqlite::Connection;

/// SMTP, abstracted for headless tests. Returns the formatted RFC 822
/// bytes (for the Sent append).
pub trait Mailer {
    #[allow(clippy::too_many_arguments)]
    fn send(
        &mut self,
        host: &str,
        email: &str,
        pass: &str,
        to: &str,
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
    ) -> Result<Vec<u8>, String>;
}

/// lettre over rustls, port 465 (fastmail-style submission).
pub struct Smtp;

impl Mailer for Smtp {
    fn send(
        &mut self,
        host: &str,
        email: &str,
        pass: &str,
        to: &str,
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        use lettre::message::header;
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{Message, SmtpTransport, Transport};
        let s = |e: &dyn std::fmt::Display| format!("{e}");
        let mut b = Message::builder()
            .from(email.parse().map_err(|e| s(&e))?)
            .to(to.parse().map_err(|e| s(&e))?)
            .subject(subject);
        if let Some(mid) = in_reply_to {
            b = b
                .header(header::InReplyTo::from(mid.to_string()))
                .header(header::References::from(mid.to_string()));
        }
        let msg = b.body(body.to_string()).map_err(|e| s(&e))?;
        let raw = msg.formatted();
        let t = SmtpTransport::relay(host)
            .map_err(|e| s(&e))?
            .credentials(Credentials::new(email.to_string(), pass.to_string()))
            .build();
        t.send(&msg).map_err(|e| s(&e))?;
        Ok(raw)
    }
}

/// Appends the sent bytes to the account's Sent folder — best effort: the
/// mail *was* sent; a failed filing only annotates the outcome.
type Appender<'a> = &'a mut dyn FnMut(i64, &str, &[u8]) -> Result<(), String>;

/// One pass over the outbox: claim everything due, submit, record. Returns
/// how many rows were claimed. Failures keep the row (`failed`) — still
/// cancellable by undo; successes make the action irreversible.
pub fn run_outbox_pass(
    conn: &Connection,
    now: f64,
    secrets_dir: &std::path::Path,
    mailer: &mut dyn Mailer,
    append: Appender,
) -> usize {
    struct Due {
        id: i64,
        account: i64,
        email: String,
        smtp: String,
        to: String,
        subject: String,
        body: String,
        reply_mid: Option<String>,
    }
    let due: Vec<Due> = {
        let Ok(mut stmt) = conn.prepare(
            "SELECT o.id, o.account, a.email, COALESCE(a.smtp_host, ''),
                    d.to_addr, d.subject, d.body,
                    (SELECT message_id FROM message WHERE id = d.re_message)
             FROM outbox o
             JOIN account a ON a.id = o.account
             JOIN draft d ON d.panel = o.id
             WHERE o.status = 'pending' AND o.send_after <= ?1",
        ) else {
            return 0;
        };
        stmt.query_map([now], |r| {
            Ok(Due {
                id: r.get(0)?,
                account: r.get(1)?,
                email: r.get(2)?,
                smtp: r.get(3)?,
                to: r.get(4)?,
                subject: r.get(5)?,
                body: r.get(6)?,
                reply_mid: r.get(7)?,
            })
        })
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default()
    };
    let mut claimed = 0;
    for d in due {
        // The claim: one winner between this pass and a concurrent undo
        // (whose changeset deletes the row only while it is 'pending').
        let won = conn
            .execute(
                "UPDATE outbox SET status='sending' WHERE id=?1 AND status='pending'",
                [d.id],
            )
            .unwrap_or(0)
            == 1;
        if !won {
            continue;
        }
        claimed += 1;
        let outcome: Result<(), String> = (|| {
            if d.smtp.is_empty() {
                return Err("account has no smtp host".into());
            }
            let pass = crate::secret::get(secrets_dir, &d.email)
                .ok_or("no password in the keychain")?;
            let raw = mailer.send(
                &d.smtp,
                &d.email,
                &pass,
                &d.to,
                &d.subject,
                &d.body,
                d.reply_mid.as_deref(),
            )?;
            if let Err(e) = append(d.account, &d.email, &raw) {
                let _ = conn.execute(
                    "UPDATE outbox SET error=?1 WHERE id=?2",
                    rusqlite::params![format!("sent; filing to Sent failed: {e}"), d.id],
                );
            }
            Ok(())
        })();
        let _ = match outcome {
            Ok(()) => conn.execute("UPDATE outbox SET status='sent' WHERE id=?1", [d.id]),
            Err(e) => conn.execute(
                "UPDATE outbox SET status='failed', error=?1 WHERE id=?2",
                rusqlite::params![e, d.id],
            ),
        };
    }
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

/// The sender thread: sleep until the next deadline (or a kick), run a
/// pass, notify the UI. IMAP filing connects per batch, lazily.
pub fn spawn(db: PathBuf, notify: impl Fn() + Send + 'static) -> Sender {
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("sender".into())
        .spawn(move || {
            let Ok(conn) = Connection::open(&db) else {
                return;
            };
            let _ = conn.busy_timeout(Duration::from_millis(5000));
            let dir = db.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
            loop {
                let now = crate::store::now();
                let mut append = |account: i64, email: &str, raw: &[u8]| -> Result<(), String> {
                    let (host, sent): (String, String) = conn
                        .query_row(
                            "SELECT COALESCE(a.imap_host,''),
                                    COALESCE((SELECT name FROM folder
                                              WHERE account=a.id AND role='sent'), 'Sent')
                             FROM account a WHERE a.id=?1",
                            [account],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .map_err(|e| e.to_string())?;
                    if host.is_empty() {
                        return Err("no imap host".into());
                    }
                    let pass = crate::secret::get(&dir, email)
                        .ok_or("no password in the keychain")?;
                    crate::sync::imap_transport::append(&host, email, &pass, &sent, raw)
                };
                let did = run_outbox_pass(&conn, now, &dir, &mut Smtp, &mut append);
                if did > 0 {
                    notify();
                }
                // Sleep until the next deadline, capped — kicks cut it short.
                let next: Option<f64> = conn
                    .query_row(
                        "SELECT MIN(send_after) FROM outbox WHERE status='pending'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(None);
                let wait = next
                    .map(|t| (t - crate::store::now()).clamp(0.2, 30.0))
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
    use crate::store::Store;

    #[derive(Default)]
    struct FakeMailer {
        sent: Vec<(String, String)>, // (to, subject)
        fail: bool,
    }

    impl Mailer for FakeMailer {
        fn send(
            &mut self,
            _h: &str,
            _e: &str,
            _p: &str,
            to: &str,
            subject: &str,
            _b: &str,
            _r: Option<&str>,
        ) -> Result<Vec<u8>, String> {
            if self.fail {
                return Err("smtp down".into());
            }
            self.sent.push((to.into(), subject.into()));
            Ok(b"raw".to_vec())
        }
    }

    fn world(smtp: &str) -> (Store, std::path::PathBuf) {
        let s = Store::open(None).unwrap();
        let dir = std::env::temp_dir().join(format!("superapp-send-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::secret::set(&dir, "t@t", "pw");
        s.write(|c| {
            c.execute(
                "INSERT INTO account(label, email, imap_host, smtp_host) VALUES('t','t@t','', ?1)",
                [smtp],
            )?;
            c.execute(
                "INSERT INTO draft(panel, account, to_addr, subject, body) VALUES(9, 1, 'x@y', 'Hi', 'Body')",
                [],
            )?;
            c.execute(
                "INSERT INTO outbox(id, account, send_after) VALUES(9, 1, 100.0)",
                [],
            )
            .map(|_| ())
        })
        .unwrap();
        (s, dir)
    }

    fn status(s: &Store) -> (String, Option<String>) {
        s.conn()
            .query_row("SELECT status, error FROM outbox WHERE id=9", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    }

    /// Not due → untouched; due → claimed once, sent, filed to Sent.
    #[test]
    fn deadline_claim_and_send() {
        let (s, dir) = world("smtp.t");
        let mut m = FakeMailer::default();
        let filed = std::cell::Cell::new(0);
        let mut append = |_a: i64, _e: &str, _r: &[u8]| {
            filed.set(filed.get() + 1);
            Ok(())
        };
        assert_eq!(run_outbox_pass(s.conn(), 99.0, &dir, &mut m, &mut append), 0);
        assert_eq!(status(&s).0, "pending", "not due yet");
        assert_eq!(run_outbox_pass(s.conn(), 100.5, &dir, &mut m, &mut append), 1);
        assert_eq!(status(&s).0, "sent");
        assert_eq!(m.sent, vec![("x@y".to_string(), "Hi".to_string())]);
        assert_eq!(filed.get(), 1, "appended to Sent");
        // Idempotent: a second pass claims nothing.
        assert_eq!(run_outbox_pass(s.conn(), 101.0, &dir, &mut m, &mut append), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SMTP failure keeps the row as failed (still cancellable by undo);
    /// an account without smtp fails honestly.
    #[test]
    fn failures_stay_cancellable() {
        let (s, dir) = world("smtp.t");
        let mut m = FakeMailer {
            fail: true,
            ..Default::default()
        };
        let mut append = |_: i64, _: &str, _: &[u8]| Ok(());
        run_outbox_pass(s.conn(), 200.0, &dir, &mut m, &mut append);
        let (st, err) = status(&s);
        assert_eq!(st, "failed");
        assert_eq!(err.as_deref(), Some("smtp down"));

        let (s2, dir2) = world("");
        let mut m2 = FakeMailer::default();
        run_outbox_pass(s2.conn(), 200.0, &dir2, &mut m2, &mut append);
        assert_eq!(status(&s2).1.as_deref(), Some("account has no smtp host"));
        assert!(m2.sent.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The undo guard: pending and failed sends are still cancellable;
    /// sending/sent are locked — and the walk marks them expired.
    #[test]
    fn sent_actions_expire_under_the_guard() {
        let (s, dir) = world("smtp.t");
        s.set_undo_guard(crate::mail::send_locked);
        // File the send as a real action so the DAG sees it.
        s.act("send", "send “Hi”", Some("outbox:9"), 1.0, |c| {
            // The row already exists in this fixture; change it for real so
            // the session records a delta (identical updates consolidate
            // to nothing and would create no node).
            c.execute("UPDATE outbox SET send_after=120.0 WHERE id=9", [])
                .map(|_| ())
        })
        .unwrap();
        // Deliver it — the action becomes irreversible.
        let mut m = FakeMailer::default();
        let mut append = |_: i64, _: &str, _: &[u8]| Ok(());
        run_outbox_pass(s.conn(), 150.0, &dir, &mut m, &mut append);
        assert_eq!(s.undo().unwrap(), None, "nothing undoable remains");
        let (nodes, head) = s.history().unwrap();
        assert_eq!(nodes[0].state, "expired");
        assert_eq!(head, 0, "walked past the expired node");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
