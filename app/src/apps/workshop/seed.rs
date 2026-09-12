use kernel::store::Store;
use rusqlite::params;
use serde_json::json;
pub fn seed(store: &Store) -> rusqlite::Result<()> {
    store.write(|c|{
 c.execute("INSERT OR IGNORE INTO workshop_project(id,name,path,base_ref,status) VALUES(1,'superapp','/sample/projects/superapp','origin/main','ready'),(2,'reader','/sample/projects/reader','origin/main','ready')",[])?;
 c.execute("INSERT OR IGNORE INTO workshop_workspace(id,project_id,label,path,branch,base_ref,status,activity,pr_json) VALUES(1,1,'zurich','/sample/workspaces/zurich','review/file-anchors','origin/main','ready',100,'null'),(2,2,'oslo','/sample/workspaces/oslo','workshop/oslo','origin/main','ready',80,'null')",[])?;
 c.execute("INSERT OR IGNORE INTO workshop_chat(id,workspace_id,ordinal,provider,model,status,last_used,unread) VALUES(1,1,1,'codex','default','ready',100,1),(2,1,2,'claude','default','ready',90,1)",[])?;
 c.execute("INSERT OR IGNORE INTO workshop_message(id,chat_id,role,body,created) VALUES(1,1,'You','Keep my file review across agent changes and rebases.',90),(2,1,'Codex','Unchanged files retain review through rebase. Editing a reviewed file reopens the whole file.',100)",[])?;
 if c.query_row("SELECT count(*) FROM workshop_snapshot",[],|r|r.get::<_,i64>(0))?==0{
  let files=vec![file("src/review/anchors.rs",23),file("src/review/progress.rs",15),file("src/workspace.rs",8),file("tests/review.rs",21)];
  let snapshot=super::git::Snapshot{head:"sample-head".into(),base_ref:"origin/main".into(),base_oid:"sample-base".into(),tree_oid:"sample-tree".into(),files,dirty:false,conflicts:vec![],warnings:vec![],captured_at:0};
  let before=super::git::Snapshot{head:"sample-base".into(),base_ref:"origin/main".into(),base_oid:"sample-base".into(),tree_oid:"sample-base".into(),files:vec![],dirty:false,conflicts:vec![],warnings:vec![],captured_at:0};
  let before_id=super::snapshots::put(c,1,before,90.0,true)?;
  let id=super::snapshots::put(c,1,snapshot.clone(),100.0,false)?;
  let diff_id=super::snapshots::put(c,1,snapshot,100.0,true)?;
  c.execute("INSERT INTO workshop_step(workspace_id,chat_id,before_id,after_id,diff_id,status,created) VALUES(1,1,?1,?2,?3,'done',100)",params![before_id,id,diff_id])?;
  let step=c.last_insert_rowid();c.execute("UPDATE workshop_message SET step_id=? WHERE id=2",[step])?;
  c.execute("UPDATE workshop_change SET reviewed=1 WHERE snapshot_id=? AND path!='tests/review.rs'",[id])?;
  let pr=json!({"number":148,"url":"https://github.com/example/project/pull/148","title":"Preserve file review","state":"OPEN","draft":false,"head":"sample-head","head_branch":"review/file-anchors","base_branch":"main","mergeable":"MERGEABLE","merge_state":"CLEAN","review_decision":"APPROVED","auto_merge":false,"checks":[],"observed_at":0});
  c.execute("UPDATE workshop_workspace SET pr_json=? WHERE id=1",[pr.to_string()])?;
 }
 Ok(())
})
}
fn file(path: &str, count: usize) -> super::git::FileDiff {
    let lines: Vec<_> = (0..count)
        .map(|i| super::git::DiffLine {
            kind: "add".into(),
            text: if i == 0 {
                "pub fn preserve_file_review() {".into()
            } else if i + 1 == count {
                "}".into()
            } else {
                format!("    check_complete_diff({i});")
            },
            old_line: None,
            new_line: Some(i + 1),
        })
        .collect();
    let patch = format!(
        "diff --git a/{path} b/{path}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{count} @@\n{}",
        lines
            .iter()
            .map(|l| format!("+{}\n", l.text))
            .collect::<String>()
    );
    super::git::FileDiff {
        path: path.into(),
        old_path: None,
        status: "A".into(),
        old_blob: String::new(),
        new_blob: format!("sample-{path}"),
        old_mode: "000000".into(),
        new_mode: "100644".into(),
        patch,
        hunks: vec![super::git::DiffHunk {
            header: format!("@@ -0,0 +1,{count} @@"),
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: count,
            lines,
        }],
        added: count,
        deleted: 0,
        binary: false,
        non_text: false,
        fingerprint: path.into(),
    }
}
