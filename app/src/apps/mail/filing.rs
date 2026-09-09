//! Filing resolves both undo state and the following conversation on the
//! writer, against the same transaction that moves the letters.

use kernel::history::Intent;
use kernel::richtable::{SqlCursor, SqlLanding};
use rusqlite::{Connection, OptionalExtension};

use super::effects::{Filed, PutBack};
use super::model::{self, MailId, Role, ThreadHead};
use super::panels::mailbox::To;

pub enum Scope {
    Mailbox { role: Role, threads: Vec<i64> },
    Message(MailId),
}

pub struct Outcome {
    pub changed: bool,
    pub claims: Vec<Box<dyn Intent>>,
    pub landing: Option<SqlLanding<ThreadHead>>,
    pub notice: Option<(String, bool)>,
}

fn unchanged(message: String, error: bool) -> Outcome {
    Outcome {
        changed: false,
        claims: Vec::new(),
        landing: None,
        notice: Some((message, error)),
    }
}

pub fn file(
    conn: &Connection,
    scope: Scope,
    to: To,
    cursor: Option<SqlCursor<ThreadHead, i64>>,
) -> rusqlite::Result<Outcome> {
    let ids = match scope {
        Scope::Mailbox { role, threads } => {
            let mut statement = conn.prepare_cached(
                "SELECT m.id FROM message m JOIN folder f ON f.id=m.folder
                 WHERE m.thread=?1 AND f.role=?2 ORDER BY m.date,m.id",
            )?;
            let mut ids = Vec::new();
            for thread in threads {
                ids.extend(
                    statement
                        .query_map(rusqlite::params![thread, role.as_str()], |row| {
                            row.get::<_, i64>(0)
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
            ids
        }
        Scope::Message(mail) => {
            if let To::Role(role) = to {
                let folders: Option<(i64, Option<i64>)> = conn.query_row(
                    "SELECT m.folder,(SELECT f.id FROM folder f WHERE f.account=m.account AND f.role=?2)
                     FROM message m WHERE m.id=?1", rusqlite::params![mail, role],
                    |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
                match folders {
                    Some((_, None)) | None => {
                        return Ok(unchanged(
                            format!("this account has no {role} folder"),
                            true,
                        ))
                    }
                    Some((from, Some(target))) if from == target => {
                        return Ok(unchanged(format!("already in the {role}"), true))
                    }
                    _ => {}
                }
            }
            conn.prepare_cached(
                "SELECT m.id FROM message m JOIN folder f ON f.id=m.folder
                 WHERE m.id=?1 OR (f.role IN ('inbox','archive','sent','spam','trash') AND m.thread=(SELECT thread FROM message WHERE id=?1)
                   AND f.role=(SELECT f.role FROM message a JOIN folder f ON f.id=a.folder WHERE a.id=?1))
                 ORDER BY m.date,m.id")?
                .query_map([mail], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        }
    };

    let mut claims: Vec<Box<dyn Intent>> = Vec::new();
    for mail in ids {
        let (from, was_trashed): (i64, Option<i64>) = conn.query_row(
            "SELECT folder,(SELECT folder FROM trashed WHERE message=?1) FROM message WHERE id=?1",
            [mail],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        match to {
            To::Role(role) => {
                if model::file_tx(conn, mail, role)? {
                    claims.push(Box::new(Filed {
                        mail,
                        from_folder: from,
                        role,
                        was_trashed,
                    }));
                }
            }
            To::Back => {
                let target = model::put_back_target_in(conn, mail);
                if model::put_back_tx(conn, mail)? {
                    claims.push(Box::new(PutBack {
                        mail,
                        trash: from,
                        to: target.expect("a successful restoration has a target"),
                    }));
                }
            }
        }
    }
    if claims.is_empty() {
        return Ok(unchanged(to.nothing_said(), false));
    }
    let landing = cursor.map(|cursor| cursor.read(conn)).transpose()?;
    Ok(Outcome {
        changed: true,
        claims,
        landing,
        notice: None,
    })
}
