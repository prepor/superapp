use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    db: Connection,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "workshop-snapshot-test-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let repo = root.join("repository");
        fs::create_dir(&repo).unwrap();
        command(&repo, &["init", "--initial-branch=main"]);
        command(&repo, &["config", "user.name", "Workshop tests"]);
        command(&repo, &["config", "user.email", "workshop@example.invalid"]);
        command(&repo, &["config", "commit.gpgsign", "false"]);
        command(&repo, &["config", "core.hooksPath", "/dev/null"]);
        let db = Connection::open(root.join("state.sqlite")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        // Execute the production migration SQL, not a parallel test schema.
        for step in super::super::schema::SCHEMA.steps {
            if let kernel::app::Step::Sql(sql) = step {
                db.execute_batch(sql).unwrap();
            }
        }
        db.execute("INSERT INTO workshop_project(id,name,path,base_ref,status) VALUES(1,'test',?1,'main','ready')", [repo.to_str().unwrap()]).unwrap();
        db.execute("INSERT INTO workshop_workspace(id,project_id,label,path,branch,base_ref,status,activity) VALUES(1,1,'test',?1,'feature','main','ready',0)", [repo.to_str().unwrap()]).unwrap();
        let fixture = Self { root, repo, db };
        fixture.write("a.txt", baseline());
        fixture.write("b.txt", "old b\n");
        fixture.commit("baseline");
        command(&fixture.repo, &["switch", "-c", "feature"]);
        fixture
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        fs::write(self.repo.join(path), content).unwrap();
    }
    fn commit(&self, message: &str) {
        command(&self.repo, &["add", "--all"]);
        command(&self.repo, &["commit", "-m", message]);
    }
    fn capture(&self) -> git::Snapshot {
        git::capture_snapshot(&self.repo, "main").unwrap()
    }
    fn put(&self, now: f64) -> i64 {
        put(&self.db, 1, self.capture(), now, false).unwrap()
    }
    fn current(&self) -> i64 {
        self.db
            .query_row(
                "SELECT snapshot_id FROM workshop_workspace WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
    fn state(&self, snapshot: i64, path: &str) -> (bool, bool, i64) {
        self.db.query_row("SELECT reviewed,needs_recheck,added+deleted FROM workshop_change WHERE snapshot_id=?1 AND path=?2", params![snapshot,path], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap()
    }
    fn review(&self, snapshot: i64, path: &str, actor: &str) {
        // Snapshot carry consumes the model's human-only derived coverage, never
        // arbitrary attributed audit rows. Agent audit entries leave it unset.
        let id = change_id(&self.db, snapshot, path).unwrap().unwrap();
        self.db.execute("INSERT INTO workshop_review(workspace_id,change_id,snapshot_id,path,fingerprint,actor,reviewed,created) SELECT workspace_id,id,snapshot_id,path,fingerprint,?2,1,10 FROM workshop_change WHERE id=?1", params![id,actor]).unwrap();
        if actor == "human" {
            self.db
                .execute(
                    "UPDATE workshop_change SET reviewed=1,needs_recheck=0 WHERE id=?",
                    [id],
                )
                .unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn baseline() -> String {
    (0..40).map(|number| format!("line {number}\n")).collect()
}
fn command(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn edited_reviewed_file_reopens_whole_file_while_other_marks_persist() {
    let fixture = Fixture::new();
    fixture.write("a.txt", baseline().replace("line 20\n", "feature line\n"));
    fixture.write("b.txt", "changed b\n");
    fixture.write("new.txt", "unreviewed\n");
    let before = fixture.put(1.0);
    fixture.review(before, "a.txt", "human");
    fixture.review(before, "b.txt", "human");
    fixture.write(
        "a.txt",
        baseline().replace("line 20\n", "feature line\nextra line\n"),
    );
    let after = fixture.put(2.0);
    assert_eq!(fixture.state(after, "a.txt"), (false, true, 3));
    assert_eq!(fixture.state(after, "b.txt"), (true, false, 2));
    assert_eq!(fixture.state(after, "new.txt"), (false, false, 1));
    assert_eq!(fixture.state(before, "a.txt"), (true, false, 2));
    fixture.write(
        "a.txt",
        baseline().replace("line 20\n", "feature line\nextra line\nthird line\n"),
    );
    let later = fixture.put(3.0);
    assert_eq!(fixture.state(later, "a.txt"), (false, true, 4));
}

#[test]
fn real_rebase_and_rename_preserve_unique_whole_file_marks_across_restart() {
    let fixture = Fixture::new();
    fixture.write("a.txt", baseline().replace("line 20\n", "feature line\n"));
    fixture.commit("feature");
    let before = fixture.put(1.0);
    fixture.review(before, "a.txt", "human");
    command(&fixture.repo, &["switch", "main"]);
    fixture.write("a.txt", format!("upstream heading\n{}", baseline()));
    fixture.commit("upstream");
    command(&fixture.repo, &["switch", "feature"]);
    command(&fixture.repo, &["rebase", "main"]);
    let rebased = fixture.put(2.0);
    assert_eq!(fixture.state(rebased, "a.txt"), (true, false, 2));
    command(&fixture.repo, &["mv", "a.txt", "renamed.txt"]);
    fixture.commit("rename");
    let reopened = Connection::open(fixture.root.join("state.sqlite")).unwrap();
    let renamed = put(&reopened, 1, fixture.capture(), 3.0, false).unwrap();
    assert_eq!(fixture.state(renamed, "renamed.txt"), (true, false, 2));
    assert_ne!(
        get(&fixture.db, before).unwrap().head,
        get(&fixture.db, rebased).unwrap().head
    );
}

#[test]
fn identical_poll_deduplicates_without_reordering_activity_or_losing_marks() {
    let fixture = Fixture::new();
    fixture.write("b.txt", "changed b\n");
    let before = fixture.put(5.0);
    fixture.review(before, "b.txt", "human");
    let mut again = fixture.capture();
    again.captured_at += 50_000;
    assert_eq!(put(&fixture.db, 1, again, 500.0, false).unwrap(), before);
    assert_eq!(fixture.state(before, "b.txt"), (true, false, 2));
    assert_eq!(
        fixture
            .db
            .query_row(
                "SELECT activity FROM workshop_workspace WHERE id=1",
                [],
                |row| row.get::<_, f64>(0)
            )
            .unwrap(),
        5.0
    );
    assert_eq!(
        fixture
            .db
            .query_row("SELECT count(*) FROM workshop_snapshot", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn historical_comparison_never_moves_current_or_imports_unrelated_coverage() {
    let fixture = Fixture::new();
    let baseline = fixture.capture();
    fixture.write("b.txt", "changed b\n");
    let current_snapshot = fixture.capture();
    let current = put(&fixture.db, 1, current_snapshot.clone(), 1.0, false).unwrap();
    fixture.review(current, "b.txt", "human");
    let step = git::compare_snapshots(&fixture.repo, &baseline, &current_snapshot).unwrap();
    let historical = put(&fixture.db, 1, step, 100.0, true).unwrap();
    assert_ne!(historical, current);
    assert_eq!(fixture.current(), current);
    assert_eq!(fixture.state(historical, "b.txt"), (false, false, 2));
    assert_eq!(fixture.state(current, "b.txt"), (true, false, 2));
    fixture.write("b.txt", "another b\n");
    let after = fixture.put(2.0);
    assert_eq!(fixture.state(after, "b.txt"), (false, true, 2));
    assert!(get(&fixture.db, historical).unwrap().files[0]
        .patch
        .contains("+changed b"));
}

#[test]
fn duplicate_file_correspondence_requires_rechecking_each_candidate() {
    let fixture = Fixture::new();
    fixture.write("first.txt", "identical\n");
    fixture.write("second.txt", "identical\n");
    let before = fixture.put(1.0);
    fixture.review(before, "first.txt", "human");
    fixture.write("third.txt", "new unrelated file\n");
    let after = fixture.put(2.0);
    assert_eq!(fixture.state(after, "first.txt"), (false, true, 1));
    assert_eq!(fixture.state(after, "second.txt"), (false, true, 1));
    assert_eq!(fixture.state(after, "third.txt"), (false, false, 1));
}

#[test]
fn agent_review_audit_rows_never_become_personal_coverage() {
    let fixture = Fixture::new();
    fixture.write("b.txt", "changed b\n");
    let before = fixture.put(1.0);
    fixture.review(before, "b.txt", "agent:claude");
    fixture.write("new.txt", "new\n");
    let after = fixture.put(2.0);
    assert_eq!(fixture.state(after, "b.txt"), (false, false, 2));
}

#[test]
fn conflict_resolution_reopens_even_if_resolved_text_matches_reviewed_tree() {
    let fixture = Fixture::new();
    fixture.write("b.txt", "changed b\n");
    let mut conflicted = fixture.capture();
    conflicted.conflicts = vec!["b.txt".to_owned()];
    let before = put(&fixture.db, 1, conflicted, 1.0, false).unwrap();
    fixture.review(before, "b.txt", "human");
    let resolved = fixture.put(2.0);
    assert_ne!(before, resolved);
    assert_eq!(fixture.state(resolved, "b.txt"), (false, true, 2));
}

#[test]
fn nontext_items_have_zero_changed_lines_and_deleted_changes_leave_denominator() {
    let fixture = Fixture::new();
    fixture.write("binary.bin", [0, 1, 2]);
    command(&fixture.repo, &["mv", "b.txt", "renamed.txt"]);
    let before = fixture.put(1.0);
    assert_eq!(fixture.state(before, "binary.bin"), (false, false, 0));
    assert_eq!(fixture.state(before, "renamed.txt"), (false, false, 0));
    fixture.review(before, "binary.bin", "human");
    fs::remove_file(fixture.repo.join("binary.bin")).unwrap();
    let after = fixture.put(2.0);
    assert_eq!(
        fixture
            .db
            .query_row(
                "SELECT count(*) FROM workshop_change WHERE snapshot_id=?",
                [after],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(fixture.state(before, "binary.bin"), (true, false, 0));
}

#[test]
fn duplicate_paths_rollback_atomically_and_missing_workspace_is_rejected() {
    let fixture = Fixture::new();
    fixture.write("b.txt", "changed b\n");
    let before = fixture.put(1.0);
    let mut invalid = fixture.capture();
    invalid.head = "b".repeat(40);
    invalid.files.push(invalid.files[0].clone());
    assert!(put(&fixture.db, 1, invalid, 2.0, false).is_err());
    assert_eq!(fixture.current(), before);
    assert_eq!(
        fixture
            .db
            .query_row("SELECT count(*) FROM workshop_snapshot", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(put(&fixture.db, 999, fixture.capture(), 3.0, false).is_err());
    // The savepoint must compose with the kernel writer's outer transaction.
    fixture.db.execute_batch("BEGIN").unwrap();
    fixture.write("new.txt", "new\n");
    let next = fixture.put(4.0);
    assert_ne!(next, before);
    fixture.db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(fixture.current(), before);
}
