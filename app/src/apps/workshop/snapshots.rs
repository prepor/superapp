//! Immutable comparison storage and conservative, whole-file human review carry.

use super::git;
#[cfg(test)]
use rusqlite::OptionalExtension;
use rusqlite::{params, Connection};
use std::collections::HashMap;

fn decode(json: &str) -> rusqlite::Result<git::Snapshot> {
    serde_json::from_str(json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

/// Read an immutable Git comparison from its stored JSON.
pub fn get(c: &Connection, id: i64) -> rusqlite::Result<git::Snapshot> {
    let json: String = c.query_row(
        "SELECT json FROM workshop_snapshot WHERE id=?",
        [id],
        |row| row.get(0),
    )?;
    decode(&json)
}

/// The savepoint also makes direct tool/test callers atomic; this nests inside
/// the store writer's existing transaction without committing that transaction.
pub fn put(
    c: &Connection,
    workspace: i64,
    snapshot: git::Snapshot,
    now: f64,
    historical: bool,
) -> rusqlite::Result<i64> {
    c.execute_batch("SAVEPOINT workshop_snapshot_put")?;
    let result = put_inner(c, workspace, snapshot, now, historical);
    match result {
        Ok(id) => {
            c.execute_batch("RELEASE workshop_snapshot_put")?;
            Ok(id)
        }
        Err(error) => {
            let _ =
                c.execute_batch("ROLLBACK TO workshop_snapshot_put; RELEASE workshop_snapshot_put");
            Err(error)
        }
    }
}

fn same_observation(before: &git::Snapshot, after: &git::Snapshot) -> bool {
    before.head == after.head
        && before.base_ref == after.base_ref
        && before.base_oid == after.base_oid
        && before.tree_oid == after.tree_oid
        && before.conflicts == after.conflicts
        && before.dirty == after.dirty
        && before.warnings == after.warnings
}

#[derive(Clone, Copy, Default)]
struct Coverage {
    reviewed: bool,
    recheck: bool,
}

fn put_inner(
    c: &Connection,
    workspace: i64,
    snapshot: git::Snapshot,
    now: f64,
    historical: bool,
) -> rusqlite::Result<i64> {
    // A missing workspace is an error, not an orphan comparison. This query also
    // observes the serialized writer's latest snapshot, including concurrent runs.
    let current: Option<i64> = c.query_row(
        "SELECT snapshot_id FROM workshop_workspace WHERE id=?",
        [workspace],
        |row| row.get(0),
    )?;
    let previous = current.map(|id| get(c, id)).transpose()?;
    if !historical
        && previous
            .as_ref()
            .is_some_and(|previous| same_observation(previous, &snapshot))
    {
        return Ok(current.expect("a previous snapshot has an ID"));
    }
    let mut coverage = HashMap::new();
    if !historical {
        if let Some(current) = current {
            let mut statement = c.prepare(
                "SELECT path,reviewed,needs_recheck FROM workshop_change WHERE snapshot_id=?",
            )?;
            for row in statement.query_map([current], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    Coverage {
                        reviewed: row.get(1)?,
                        recheck: row.get(2)?,
                    },
                ))
            })? {
                let (path, state) = row?;
                coverage.insert(path, state);
            }
        }
    }
    let correspondence = previous
        .as_ref()
        .filter(|_| !historical)
        .map(|previous| git::correspond_files(&previous.files, &snapshot.files))
        .unwrap_or_default();
    let encoded = serde_json::to_string(&snapshot)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    c.execute("INSERT INTO workshop_snapshot(workspace_id,head,base_oid,tree_oid,created,json,historical) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![workspace,snapshot.head,snapshot.base_oid,snapshot.tree_oid,now,encoded,historical])?;
    let id = c.last_insert_rowid();
    let mut insert = c.prepare("INSERT INTO workshop_change(workspace_id,snapshot_id,path,old_path,patch,added,deleted,reviewed,needs_recheck,binary,status,fingerprint) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)")?;
    for file in &snapshot.files {
        let mut state = Coverage::default();
        if let Some(mapping) = correspondence
            .iter()
            .find(|mapping| mapping.current_path == file.path)
        {
            let prior = mapping
                .previous_path
                .as_ref()
                .and_then(|path| coverage.get(path))
                .copied()
                .unwrap_or_default();
            let conflicted = snapshot
                .conflicts
                .iter()
                .any(|path| path == &file.path || Some(path) == file.old_path.as_ref())
                || previous.as_ref().is_some_and(|previous| {
                    previous.conflicts.iter().any(|path| {
                        Some(path) == mapping.previous_path.as_ref()
                            || path == &file.path
                            || Some(path) == file.old_path.as_ref()
                    })
                });
            if mapping.preserved && !conflicted {
                state = prior;
            } else {
                // Ambiguous renamed matches may have no single previous_path.
                // Any reviewed candidate requires a recheck; never choose one.
                let ambiguous_reviewed = previous.as_ref().is_some_and(|previous| {
                    previous
                        .files
                        .iter()
                        .filter(|old| old.fingerprint == file.fingerprint)
                        .any(|old| {
                            coverage
                                .get(&old.path)
                                .is_some_and(|coverage| coverage.reviewed || coverage.recheck)
                        })
                });
                state.recheck = prior.reviewed || prior.recheck || ambiguous_reviewed;
            }
        }
        insert.execute(params![
            workspace,
            id,
            file.path,
            file.old_path,
            file.patch,
            file.added as i64,
            file.deleted as i64,
            state.reviewed,
            state.recheck,
            file.binary,
            file.status,
            file.fingerprint
        ])?;
    }
    if !historical {
        let activity_changed = previous.as_ref().is_none_or(|previous| {
            previous.tree_oid != snapshot.tree_oid || previous.head != snapshot.head
        });
        c.execute("UPDATE workshop_workspace SET snapshot_id=?2,activity=CASE WHEN ?3 THEN ?4 ELSE activity END WHERE id=?1", params![workspace,id,activity_changed,now])?;
    }
    Ok(id)
}

/// Current change ID for a precise snapshot/path locator, used by tools that
/// present the same complete-file action as the native diff panel.
#[cfg(test)]
pub fn change_id(c: &Connection, snapshot: i64, path: &str) -> rusqlite::Result<Option<i64>> {
    c.query_row(
        "SELECT id FROM workshop_change WHERE snapshot_id=?1 AND path=?2",
        params![snapshot, path],
        |row| row.get(0),
    )
    .optional()
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
