//! Apply recorded row changes once, with strict baseline and constraint checks.
use rusqlite::fallible_streaming_iterator::FallibleStreamingIterator;
use rusqlite::session::ChangesetIter;
use rusqlite::Connection;
use std::collections::HashMap;

fn replay_error(why: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
        Some(why),
    )
}

/// A DELETE trigger can remove another replicated row before SQLite reaches
/// that row's recorded indirect DELETE. Prove that every such before-image
/// exists and matches *before* applying anything. Only then is a missing
/// indirect delete during this same transaction a repeated trigger effect.
/// Missing or modified rows at the starting boundary remain real conflicts.
/// Table layouts are checked as well: SQLite otherwise silently skips changes
/// naming a missing table or a different primary key layout.
fn validate_changeset(conn: &Connection, changeset: &[u8]) -> rusqlite::Result<()> {
    // The streaming C iterator retains the address of this trait-object
    // reference. Keep the reference itself alive until the iterator drops.
    let input: &mut dyn std::io::Read = &mut &changeset[..];
    let mut iter = ChangesetIter::start_strm(&input)?;
    let mut tables: HashMap<String, Vec<(String, bool)>> = HashMap::new();
    while let Some(item) = iter.next()? {
        let op = item.op()?;
        if !tables.contains_key(op.table_name()) {
            let columns = conn
                .prepare("SELECT name, pk != 0 FROM pragma_table_xinfo(?1) ORDER BY cid")?
                .query_map([op.table_name()], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            tables.insert(op.table_name().to_string(), columns);
        }
        let columns = &tables[op.table_name()];
        let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
        let count = op.number_of_columns() as usize;
        let keys = item.pk()?;
        if columns.len() != count
            || keys.len() != count
            || columns
                .iter()
                .zip(keys)
                .any(|((_, primary), key)| *primary != (*key != 0))
        {
            return Err(replay_error(format!(
                "replay: incompatible table {}",
                op.table_name()
            )));
        }
        if !op.indirect() || op.code() != rusqlite::hooks::Action::SQLITE_DELETE {
            continue;
        }
        let before = (0..count)
            .map(|i| item.old_value(i))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let predicate = columns
            .iter()
            .zip(keys)
            .filter(|(_, key)| **key != 0)
            .map(|((column, _), _)| format!("{} IS ?", quote(column)))
            .collect::<Vec<_>>()
            .join(" AND ");
        if predicate.is_empty() {
            return Err(replay_error(format!(
                "replay: no primary key for {}",
                op.table_name()
            )));
        }
        let params = before
            .iter()
            .zip(keys)
            .enumerate()
            .filter(|(_, (_, key))| **key != 0)
            .map(|(i, (value, _))| {
                rusqlite::types::Value::try_from(*value).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(i, value.data_type(), Box::new(error))
                })
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT * FROM {} WHERE {predicate}",
            quote(op.table_name())
        ))?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
        let matches = match rows.next()? {
            Some(row) => (0..count)
                .map(|i| Ok(row.get_ref(i)? == before[i]))
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .all(|same| same),
            None => false,
        };
        if !matches {
            return Err(replay_error(format!(
                "replay: indirect DELETE before-image differs in {}",
                op.table_name()
            )));
        }
    }
    Ok(())
}

/// Apply captured FK actions once while keeping ordinary triggers enabled
/// for derived FTS indexes. rusqlite does not yet expose apply_v2 flags.
/// See https://www.sqlite.org/session/c_changesetapply_fknoaction.html.
pub(super) fn apply_changeset(conn: &Connection, changeset: &[u8]) -> rusqlite::Result<()> {
    use rusqlite::ffi;
    use std::ffi::{c_int, c_void, CStr};

    validate_changeset(conn, changeset)?;
    let length = c_int::try_from(changeset.len())
        .map_err(|_| replay_error("replay: changeset is too large".into()))?;
    // All indirect DELETE before-images were checked under this transaction's
    // write lock. The callback only tolerates those same rows disappearing as
    // a consequence of earlier trigger execution in this changeset.
    unsafe extern "C" fn conflict(
        context: *mut c_void,
        kind: c_int,
        iter: *mut ffi::sqlite3_changeset_iter,
    ) -> c_int {
        let detail = unsafe { &mut *context.cast::<Option<String>>() };
        if kind == ffi::SQLITE_CHANGESET_FOREIGN_KEY {
            *detail = Some("replay: foreign key constraint failed".into());
            return ffi::SQLITE_CHANGESET_ABORT;
        }
        let (mut table, mut columns, mut op, mut indirect) = (std::ptr::null(), 0, 0, 0);
        let rc = unsafe {
            ffi::sqlite3changeset_op(iter, &mut table, &mut columns, &mut op, &mut indirect)
        };
        if rc == ffi::SQLITE_OK {
            if kind == ffi::SQLITE_CHANGESET_NOTFOUND && op == ffi::SQLITE_DELETE && indirect != 0 {
                return ffi::SQLITE_CHANGESET_OMIT;
            }
            let name = unsafe { CStr::from_ptr(table) }.to_string_lossy();
            *detail = Some(format!(
                "replay: conflict {kind} in {name} (operation {op}, indirect {indirect})"
            ));
        }
        ffi::SQLITE_CHANGESET_ABORT
    }
    let mut detail: Option<String> = None;
    // SAFETY: the connection is exclusively used by its writer thread; all
    // pointers remain alive until this synchronous call returns. SQLite does
    // not modify the input despite the historical mutable C signature. The
    // callback cannot unwind and rebase output is disabled.
    let rc = unsafe {
        ffi::sqlite3changeset_apply_v2(
            conn.handle(),
            length,
            changeset.as_ptr().cast_mut().cast(),
            None,
            Some(conflict),
            (&mut detail as *mut Option<String>).cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            ffi::SQLITE_CHANGESETAPPLY_FKNOACTION,
        )
    };
    if rc == ffi::SQLITE_OK {
        return Ok(());
    }
    Err(rusqlite::Error::SqliteFailure(
        ffi::Error::new(rc),
        detail.or_else(|| {
            // SAFETY: SQLite's error string belongs to the still-live connection.
            Some(
                unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(conn.handle())) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }),
    ))
}
