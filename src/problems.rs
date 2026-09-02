//! Standing problems: background conditions that are wrong *right now* — an
//! account whose last sync failed, a send the sender gave up on, a device
//! sync that cannot reach its bucket. A toast announces the arrival; this is
//! what remains once it has faded.
//!
//! Nothing here is stored. The list is **derived** from rows that already
//! exist (the account's status line, the failed outbox rows, the cached
//! lease status), so it can never disagree with them and there is nothing to
//! clear: fix the condition and the problem is gone.

use std::rc::Rc;

use crate::mail;
use crate::repl;
use crate::store::{Store, Q};

/// Where a problem comes from — and so what can be done about it.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// An account whose last sync pass failed.
    Account { id: i64, email: String },
    /// A send whose last attempt failed. While the executor is still
    /// retrying it there is nothing to do but watch; once it has *given up*
    /// the draft — still there, keyed by the same id — can be retried or
    /// reopened.
    Send {
        outbox: i64,
        subject: String,
        re: i64,
        given_up: bool,
    },
    /// Device sync: the bucket could not be reached this pass.
    Sync,
}

/// One standing problem, reduced to what a row draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub source: Source,
    /// What it concerns, in one line: the address, the mail, the sync.
    pub label: String,
    /// What is wrong — the error, for a human. Drawn in the one colour.
    pub line: String,
    /// The muted line under it: last success, the recipient, the backlog.
    pub detail: String,
}

impl Problem {
    /// A stable identity for "have we announced this one": the same key
    /// while the condition stands, whatever its error text does.
    #[must_use]
    pub fn key(&self) -> String {
        match &self.source {
            Source::Account { id, .. } => format!("account:{id}"),
            Source::Send { outbox, .. } => format!("outbox:{outbox}"),
            Source::Sync => "sync".into(),
        }
    }
}

/// A send is a problem from its first failed attempt, not from the sixth:
/// a row the executor is still backing off on has been wrong for as long as
/// its error says. The live job is the newest non-obsolete submit for the
/// row (a retry obsoletes the old one).
static Q_FAILING_SENDS: Q = Q {
    id: "failing_sends",
    sql: "SELECT o.id, o.status, COALESCE(o.error, e.error, 'send failed'),
                 COALESCE(d.subject, ''), COALESCE(d.to_addr, ''),
                 COALESCE(d.re_message, 0), COALESCE(e.attempts, 0),
                 COALESCE(e.not_before, 0)
          FROM outbox o
          LEFT JOIN draft d ON d.panel = o.id
          LEFT JOIN effect e ON e.id = (SELECT MAX(id) FROM effect
                                        WHERE kind = 'submit' AND status != 'obsolete'
                                          AND payload ->> 'outbox' = o.id)
          WHERE o.status = 'failed'
             OR (o.status = 'sending' AND e.status = 'pending' AND e.error IS NOT NULL)
          ORDER BY o.id",
    describe: "every send whose last attempt failed — still retrying, or given up",
};

/// Every standing problem, accounts first, then sends, then device sync.
/// `repl` is the lease status the shell last heard (`None` when replication
/// is off — a plain local store has no bucket to lose).
#[must_use]
pub fn list(store: &Store, repl: Option<&repl::Status>) -> Vec<Problem> {
    let mut v = Vec::new();
    for a in mail::accounts(store).iter() {
        let Some(status) = a.status.as_deref() else {
            continue;
        };
        let Some(err) = status.strip_prefix("error") else {
            continue;
        };
        let line = err.trim_start_matches([':', ' ']).trim().to_string();
        v.push(Problem {
            source: Source::Account {
                id: a.id,
                email: a.email.clone(),
            },
            label: a.email.clone(),
            line: if line.is_empty() {
                "sync failed".into()
            } else {
                line
            },
            detail: match a.synced {
                Some(t) => format!("last synced {}", mail::fmt_date(t)),
                None => "never synced".into(),
            },
        });
    }
    type SendRow = (i64, String, String, String, String, i64, i64, f64);
    let sends: Rc<Vec<SendRow>> = store.rows(&Q_FAILING_SENDS, &[], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
        ))
    });
    for (outbox, status, error, subject, to, re, attempts, next) in sends.iter() {
        let subject = if subject.is_empty() {
            "(no subject)".to_string()
        } else {
            subject.clone()
        };
        let given_up = status == "failed";
        let to = if to.is_empty() {
            "no recipient".to_string()
        } else {
            format!("to {to}")
        };
        v.push(Problem {
            source: Source::Send {
                outbox: *outbox,
                subject: subject.clone(),
                re: *re,
                given_up,
            },
            label: format!("send “{subject}”"),
            line: error.clone(),
            detail: if given_up {
                format!("{to} — gave up after {attempts} attempts")
            } else {
                format!(
                    "{to} — attempt {attempts} of {}, next at {}",
                    crate::effect::MAX_ATTEMPTS,
                    mail::fmt_date(*next)
                )
            },
        });
    }
    if let Some(s) = repl {
        if matches!(s.role, repl::Role::Offline) {
            v.push(Problem {
                source: Source::Sync,
                label: "device sync".into(),
                line: "the bucket is unreachable".into(),
                detail: match s.unpublished {
                    0 => "nothing waiting to publish".into(),
                    1 => "1 frame waiting to publish".into(),
                    n => format!("{n} frames waiting to publish"),
                },
            });
        }
    }
    v
}

/// The mark's text: how many stand.
#[must_use]
pub fn count_line(n: usize) -> String {
    if n == 1 {
        "1 problem".into()
    } else {
        format!("{n} problems")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::World;

    fn world() -> World {
        let w = World::fake(mail::registry());
        w.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO account(label, email, imap_host, smtp_host)
                     VALUES('t','t@t','imap.t','')",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        w
    }

    fn status(role: repl::Role, unpublished: i64) -> repl::Status {
        repl::Status {
            role,
            epoch: 1,
            unpublished,
            device: "dev".into(),
        }
    }

    #[test]
    fn a_healthy_world_has_none() {
        let w = world();
        assert!(list(w.store(), None).is_empty());
        let ok = status(repl::Role::Holder, 3);
        assert!(
            list(w.store(), Some(&ok)).is_empty(),
            "holding is not a problem"
        );
    }

    #[test]
    fn a_failed_sync_is_the_accounts_status_line() {
        let w = world();
        w.store()
            .write(|c| {
                c.execute(
                    "UPDATE account SET status='error: no route to host', synced=NULL",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        let v = list(w.store(), None);
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0].source,
            Source::Account {
                id: 1,
                email: "t@t".into()
            }
        );
        assert_eq!(v[0].line, "no route to host");
        assert_eq!(v[0].detail, "never synced");
        assert_eq!(v[0].key(), "account:1");

        // It clears itself when the next pass succeeds: nothing to reset.
        w.store()
            .write(|c| {
                c.execute("UPDATE account SET status='ok · Sep 01 12:00'", [])
                    .map(|_| ())
            })
            .unwrap();
        assert!(list(w.store(), None).is_empty());
    }

    #[test]
    fn a_failed_send_carries_its_draft() {
        let w = world();
        w.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO draft(panel, account, to_addr, subject, body)
                     VALUES(9, 1, 'x@y', 'Hi', 'Body')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO outbox(id, account, send_after, status, error)
                     VALUES(9, 1, 0, 'failed', 'account has no smtp host')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO effect(kind, payload, entity, status, idempotent, error,
                                        attempts, created, updated)
                     VALUES('submit', '{\"outbox\":9}', 'outbox:9', 'failed', 0,
                            'account has no smtp host', 6, 0, 0)",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        let v = list(w.store(), None);
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0].source,
            Source::Send {
                outbox: 9,
                subject: "Hi".into(),
                re: 0,
                given_up: true,
            }
        );
        assert_eq!(v[0].label, "send “Hi”");
        assert_eq!(v[0].line, "account has no smtp host");
        assert_eq!(v[0].detail, "to x@y — gave up after 6 attempts");
        assert_eq!(v[0].key(), "outbox:9");
    }

    /// A send is wrong from its first failed attempt — the executor's
    /// backoff would otherwise hide it for the better part of an hour.
    #[test]
    fn a_send_still_being_retried_is_listed_without_actions() {
        let w = world();
        w.store()
            .write(|c| {
                c.execute(
                    "INSERT INTO draft(panel, account, to_addr, subject, body)
                     VALUES(9, 1, 'x@y', '', 'Body')",
                    [],
                )?;
                c.execute(
                    "INSERT INTO outbox(id, account, send_after, status)
                     VALUES(9, 1, 0, 'sending')",
                    [],
                )?;
                // An older attempt a retry obsoleted, and the live one.
                c.execute(
                    "INSERT INTO effect(kind, payload, entity, status, idempotent, error,
                                        attempts, not_before, created, updated)
                     VALUES('submit', '{\"outbox\":9}', 'outbox:9', 'obsolete', 0,
                            'stale', 6, 0, 0, 0)",
                    [],
                )?;
                c.execute(
                    "INSERT INTO effect(kind, payload, entity, status, idempotent, error,
                                        attempts, not_before, created, updated)
                     VALUES('submit', '{\"outbox\":9}', 'outbox:9', 'pending', 0,
                            'connection refused', 2, 1756728300.0, 0, 0)",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        let v = list(w.store(), None);
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0].source,
            Source::Send {
                outbox: 9,
                subject: "(no subject)".into(),
                re: 0,
                given_up: false,
            }
        );
        assert_eq!(v[0].line, "connection refused");
        assert_eq!(v[0].detail, "to x@y — attempt 2 of 6, next at sep 01 12:05");

        // A send that is merely waiting for its window is not a problem.
        w.store()
            .write(|c| {
                c.execute(
                    "UPDATE effect SET error = NULL WHERE status = 'pending'",
                    [],
                )
                .map(|_| ())
            })
            .unwrap();
        assert!(list(w.store(), None).is_empty());
    }

    #[test]
    fn an_unreachable_bucket_counts_its_backlog() {
        let w = world();
        let off = status(repl::Role::Offline, 2);
        let v = list(w.store(), Some(&off));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].source, Source::Sync);
        assert_eq!(v[0].detail, "2 frames waiting to publish");
        // A follower behind the locked screen is not listed twice over.
        let locked = status(
            repl::Role::Follower {
                holder: "other".into(),
            },
            0,
        );
        assert!(list(w.store(), Some(&locked)).is_empty());
    }

    #[test]
    fn the_mark_counts() {
        assert_eq!(count_line(1), "1 problem");
        assert_eq!(count_line(3), "3 problems");
    }
}
