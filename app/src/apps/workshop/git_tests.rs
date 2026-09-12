use super::*;

#[test]
fn desktop_command_path_preserves_configured_precedence_and_adds_mac_cli_locations() {
    let path = expanded_path(
        Some(OsStr::new("/custom/bin:/usr/bin")),
        Some(OsStr::new("/Users/example")),
    )
    .unwrap();
    let paths: Vec<_> = std::env::split_paths(&path).collect();
    assert_eq!(paths[0], PathBuf::from("/custom/bin"));
    assert_eq!(paths[1], PathBuf::from("/usr/bin"));
    assert!(paths.contains(&PathBuf::from("/opt/homebrew/bin")));
    assert!(paths.contains(&PathBuf::from("/usr/local/bin")));
    assert!(paths.contains(&PathBuf::from("/Users/example/.local/bin")));
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.as_path() == Path::new("/usr/bin"))
            .count(),
        1
    );
    assert!(std::env::split_paths(&expanded_path(None, None).unwrap())
        .any(|path| path == Path::new("/usr/bin")));
}

struct Repository {
    root: PathBuf,
    path: PathBuf,
}

impl Repository {
    fn new() -> Self {
        let temporary = TemporaryIndex::new().unwrap();
        let root = temporary.directory.clone();
        std::mem::forget(temporary);
        let path = root.join("repository");
        fs::create_dir(&path).unwrap();
        git(&path, ["init", "--initial-branch=main"]).unwrap();
        git(&path, ["config", "user.name", "Workshop tests"]).unwrap();
        git(&path, ["config", "user.email", "workshop@example.invalid"]).unwrap();
        git(&path, ["config", "commit.gpgsign", "false"]).unwrap();
        git(&path, ["config", "core.hooksPath", "/dev/null"]).unwrap();
        let repository = Self { root, path };
        repository.write("anchor.txt", "initial\n");
        repository.commit("initial");
        repository
    }

    fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
        if let Some(parent) = self.path.join(path).parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(self.path.join(path), contents).unwrap();
    }

    fn commit(&self, message: &str) {
        git(&self.path, ["add", "--all"]).unwrap();
        git(&self.path, ["commit", "-m", message]).unwrap();
    }

    fn snapshot(&self) -> Snapshot {
        capture_snapshot(&self.path, "main").unwrap()
    }

    fn branch(&self) {
        git(&self.path, ["switch", "-c", "feature"]).unwrap();
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn snapshot_includes_staged_unstaged_untracked_and_preserves_real_index() {
    let repo = Repository::new();
    repo.write("second.txt", "before\n");
    repo.commit("second file");
    repo.branch();
    repo.write("anchor.txt", "staged\n");
    git(&repo.path, ["add", "anchor.txt"]).unwrap();
    repo.write("anchor.txt", "unstaged after staging\n");
    repo.write("second.txt", "changed\n");
    repo.write("new.txt", "new line\nanother\n");
    let index_before = git(&repo.path, ["ls-files", "--stage", "-z"]).unwrap();
    let status_before = git(&repo.path, ["status", "--porcelain=v1", "-z"]).unwrap();
    let snapshot = repo.snapshot();
    assert!(snapshot.dirty);
    assert_eq!(snapshot.files.len(), 3);
    assert_eq!(snapshot.added(), 4);
    assert_eq!(snapshot.deleted(), 2);
    assert!(snapshot
        .files
        .iter()
        .find(|file| file.path == "anchor.txt")
        .unwrap()
        .patch
        .contains("+unstaged after staging"));
    assert_eq!(
        git(&repo.path, ["ls-files", "--stage", "-z"]).unwrap(),
        index_before
    );
    assert_eq!(
        git(&repo.path, ["status", "--porcelain=v1", "-z"]).unwrap(),
        status_before
    );
    assert_eq!(
        git_text(
            &repo.path,
            [
                "rev-parse",
                &format!("refs/workshop/snapshots/{}", snapshot.tree_oid)
            ]
        )
        .unwrap(),
        snapshot.tree_oid
    );
    let encoded = serde_json::to_string(&snapshot).unwrap();
    let decoded: Snapshot = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.files[0].patch, snapshot.files[0].patch);
}

#[test]
fn steps_retain_before_after_content_after_edits_and_gc() {
    let repo = Repository::new();
    repo.branch();
    let before = repo.snapshot();
    repo.write("anchor.txt", "first agent output\n");
    let after = repo.snapshot();
    repo.write("anchor.txt", "later editor output\n");
    git(&repo.path, ["gc", "--prune=now"]).unwrap();
    let step = compare_snapshots(&repo.path, &before, &after).unwrap();
    assert_eq!(step.files.len(), 1);
    assert!(step.files[0].patch.contains("+first agent output"));
    assert!(!step.files[0].patch.contains("later editor"));
    assert_eq!((step.added(), step.deleted()), (1, 1));
}

#[test]
fn complete_file_review_survives_real_rebase_line_shift_and_rename() {
    let repo = Repository::new();
    let baseline = (1..=40)
        .map(|number| format!("line {number}\n"))
        .collect::<String>();
    repo.write("anchor.txt", &baseline);
    repo.commit("long file");
    repo.branch();
    let changed = baseline.replace("line 25\n", "feature line 25\n");
    repo.write("anchor.txt", &changed);
    repo.commit("feature edit");
    let reviewed = repo.snapshot();
    git(&repo.path, ["switch", "main"]).unwrap();
    repo.write("anchor.txt", format!("new upstream heading\n{baseline}"));
    repo.commit("upstream line shift");
    git(&repo.path, ["switch", "feature"]).unwrap();
    git(&repo.path, ["rebase", "main"]).unwrap();
    let rebased = repo.snapshot();
    assert_ne!(reviewed.head, rebased.head);
    assert_ne!(
        reviewed.files[0].hunks[0].new_start,
        rebased.files[0].hunks[0].new_start
    );
    assert!(correspond_files(&reviewed.files, &rebased.files)[0].preserved);
    git(&repo.path, ["mv", "anchor.txt", "renamed.txt"]).unwrap();
    repo.commit("rename file");
    let renamed = repo.snapshot();
    assert_eq!(renamed.files.len(), 1);
    assert!(renamed.files[0].status.starts_with('R'));
    let correspondence = correspond_files(&rebased.files, &renamed.files);
    assert!(correspondence[0].preserved);
    assert_eq!(
        correspondence[0].previous_path.as_deref(),
        Some("anchor.txt")
    );
    repo.write(
        "renamed.txt",
        format!(
            "new upstream heading\n{}",
            changed.replace("line 35", "later agent line 35")
        ),
    );
    let edited = repo.snapshot();
    let correspondence = correspond_files(&renamed.files, &edited.files);
    assert!(!correspondence[0].preserved);
    assert_eq!(edited.files[0].changed_lines(), 4);
}

#[test]
fn relevant_context_change_does_not_preserve_a_review() {
    let repo = Repository::new();
    repo.write("anchor.txt", "one\ntwo\nthree\nfour\nfive\nsix\nseven\n");
    repo.commit("context");
    repo.branch();
    repo.write("anchor.txt", "one\ntwo\nthree\nfeature\nfive\nsix\nseven\n");
    repo.commit("feature");
    let reviewed = repo.snapshot();
    git(&repo.path, ["switch", "main"]).unwrap();
    repo.write(
        "anchor.txt",
        "one changed\ntwo\nthree\nfour\nfive\nsix\nseven\n",
    );
    repo.commit("upstream relevant context");
    git(&repo.path, ["switch", "feature"]).unwrap();
    git(&repo.path, ["rebase", "main"]).unwrap();
    let changed_context = repo.snapshot();
    assert_eq!(
        reviewed.files[0].changed_lines(),
        changed_context.files[0].changed_lines()
    );
    assert!(!correspond_files(&reviewed.files, &changed_context.files)[0].preserved);
}

#[test]
fn duplicate_complete_diffs_are_ambiguous_even_at_the_same_path() {
    let repo = Repository::new();
    repo.write("duplicate.txt", "initial\n");
    repo.commit("duplicate base");
    repo.branch();
    repo.write("anchor.txt", "same edit\n");
    repo.write("duplicate.txt", "same edit\n");
    let before = repo.snapshot();
    let after = repo.snapshot();
    assert_eq!(before.files[0].fingerprint, before.files[1].fingerprint);
    assert!(correspond_files(&before.files, &after.files)
        .iter()
        .all(|file| !file.preserved));
    repo.write("duplicate.txt", "different edit\n");
    let edited = repo.snapshot();
    assert!(correspond_files(&before.files, &edited.files)
        .iter()
        .all(|file| !file.preserved));
}

#[test]
fn identical_context_blocks_inside_a_file_cannot_move_review_to_another_block() {
    let repo = Repository::new();
    let lines: Vec<_> = (0..27)
        .map(|number| format!("repeated line {}\n", number % 9))
        .collect();
    repo.write("anchor.txt", lines.concat());
    repo.commit("repeated blocks");
    repo.branch();
    let mut first = lines.clone();
    first[4] = "changed line\n".to_owned();
    repo.write("anchor.txt", first.concat());
    let reviewed = repo.snapshot();
    let unchanged = repo.snapshot();
    assert!(correspond_files(&reviewed.files, &unchanged.files)[0].preserved);
    let mut second = lines;
    second[13] = "changed line\n".to_owned();
    repo.write("anchor.txt", second.concat());
    let relocated = repo.snapshot();
    assert_eq!(
        reviewed.files[0].hunks[0]
            .lines
            .iter()
            .map(|line| &line.text)
            .collect::<Vec<_>>(),
        relocated.files[0].hunks[0]
            .lines
            .iter()
            .map(|line| &line.text)
            .collect::<Vec<_>>()
    );
    assert!(!correspond_files(&reviewed.files, &relocated.files)[0].preserved);
}

#[test]
fn binary_deletions_pure_renames_and_modes_are_truthful() {
    let repo = Repository::new();
    repo.write("remove.txt", "gone\n");
    repo.write("rename.txt", "unchanged\n");
    repo.write("binary.bin", [0, 1, 2]);
    repo.commit("files");
    repo.branch();
    fs::remove_file(repo.path.join("remove.txt")).unwrap();
    fs::rename(repo.path.join("rename.txt"), repo.path.join("renamed.txt")).unwrap();
    repo.write("binary.bin", [0, 3, 4]);
    let snapshot = repo.snapshot();
    let deleted = snapshot
        .files
        .iter()
        .find(|file| file.path == "remove.txt")
        .unwrap();
    assert_eq!(deleted.status, "D");
    assert_eq!(deleted.changed_lines(), 1);
    let renamed = snapshot
        .files
        .iter()
        .find(|file| file.path == "renamed.txt")
        .unwrap();
    assert!(renamed.non_text);
    assert_eq!(renamed.changed_lines(), 0);
    let binary = snapshot
        .files
        .iter()
        .find(|file| file.path == "binary.bin")
        .unwrap();
    assert!(binary.binary && binary.non_text);
    assert_eq!(binary.changed_lines(), 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            repo.path.join("anchor.txt"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let changed = repo.snapshot();
        let mode = changed
            .files
            .iter()
            .find(|file| file.path == "anchor.txt")
            .unwrap();
        assert!(mode.non_text);
        assert_eq!(mode.changed_lines(), 0);
        assert_eq!(mode.new_mode, "100755");
    }
}

#[test]
fn no_newline_metadata_is_part_of_review_identity_not_line_count() {
    let repo = Repository::new();
    repo.branch();
    repo.write("anchor.txt", "initial");
    let snapshot = repo.snapshot();
    assert_eq!(snapshot.files[0].changed_lines(), 2);
    assert!(snapshot.files[0].hunks[0]
        .lines
        .iter()
        .any(|line| line.kind == "note"));
    repo.write("anchor.txt", "modified");
    let edited = repo.snapshot();
    assert!(!correspond_files(&snapshot.files, &edited.files)[0].preserved);
}

#[test]
fn filenames_with_pathspec_characters_are_not_patterns() {
    let repo = Repository::new();
    repo.branch();
    repo.write("[literal]*.txt", "literal\n");
    repo.write("-flag.txt", "flag\n");
    repo.write("nested/space and\ttab.txt", "tab\n");
    repo.write("newline\n@@ -1 +1 @@.txt", "newline\n");
    let snapshot = capture_snapshot(repo.path.join("nested"), "main").unwrap();
    assert_eq!(snapshot.files.len(), 4);
    assert!(snapshot
        .files
        .iter()
        .all(|file| file.added == 1 && file.deleted == 0));
}

#[test]
fn ignored_files_follow_git_rules_and_sparse_checkout_fails_explicitly() {
    let repo = Repository::new();
    repo.write(".gitignore", "ignored.txt\n");
    repo.commit("ignore");
    repo.branch();
    repo.write("ignored.txt", "private\n");
    repo.write("visible.txt", "visible\n");
    assert_eq!(repo.snapshot().files.len(), 1);
    git(&repo.path, ["config", "core.sparseCheckout", "true"]).unwrap();
    assert!(capture_snapshot(&repo.path, "main")
        .unwrap_err()
        .contains("sparse checkout"));
}

#[test]
fn conflict_snapshot_keeps_index_and_reports_conflicts() {
    let repo = Repository::new();
    repo.branch();
    repo.write("anchor.txt", "feature\n");
    repo.commit("feature");
    git(&repo.path, ["switch", "main"]).unwrap();
    repo.write("anchor.txt", "upstream\n");
    repo.commit("upstream");
    git(&repo.path, ["switch", "feature"]).unwrap();
    assert!(git(&repo.path, ["merge", "main"]).is_err());
    let unmerged = git(&repo.path, ["ls-files", "--unmerged", "-z"]).unwrap();
    let snapshot = repo.snapshot();
    assert_eq!(snapshot.conflicts, vec!["anchor.txt"]);
    assert!(snapshot.files[0].patch.contains("<<<<<<<"));
    assert_eq!(
        git(&repo.path, ["ls-files", "--unmerged", "-z"]).unwrap(),
        unmerged
    );
}

#[test]
fn worktree_creation_allocates_cities_and_preserves_source_checkout() {
    let repo = Repository::new();
    repo.write("anchor.txt", "source uncommitted\n");
    let source_head = commit(&repo.path, "HEAD").unwrap();
    let root = repo.root.join("worktrees");
    let first = create_worktree(&repo.path, &root).unwrap();
    let second = create_worktree(&repo.path, &root).unwrap();
    assert_ne!(first.label, second.label);
    assert_eq!(first.label, "zurich");
    assert_eq!(second.label, "basel");
    assert_eq!(commit(Path::new(&first.path), "HEAD").unwrap(), source_head);
    assert_eq!(
        fs::read_to_string(Path::new(&first.path).join("anchor.txt")).unwrap(),
        "initial\n"
    );
    assert_eq!(
        fs::read_to_string(repo.path.join("anchor.txt")).unwrap(),
        "source uncommitted\n"
    );
    assert!(create_worktree_named(&repo.path, &root, "zurich", "main").is_err());
    assert!(create_worktree_named(&repo.path, &root, "../outside", "main").is_err());
    assert!(create_worktree_named(&repo.path, &root, "safe", "--help").is_err());
    let named = create_worktree_named(&repo.path, &root, "oslo", "main").unwrap();
    assert_eq!(named.label, "oslo");
}

#[cfg(unix)]
#[test]
fn failing_worktree_hook_does_not_create_more_checkouts_as_collision_retries() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repository::new();
    let hooks = repo.root.join("hooks");
    fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("post-checkout");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &repo.path,
        ["config", "core.hooksPath", hooks.to_str().unwrap()],
    )
    .unwrap();
    let worktrees = repo.root.join("worktrees");
    assert!(create_worktree(&repo.path, &worktrees).is_err());
    assert_eq!(fs::read_dir(&worktrees).unwrap().count(), 1);
    assert!(worktrees.join("zurich").join("anchor.txt").exists());
}

#[test]
fn discovers_remote_default_before_main_and_local_fallback() {
    let repo = Repository::new();
    assert_eq!(discover_repository(&repo.path).unwrap().base_ref, "main");
    let head = commit(&repo.path, "HEAD").unwrap();
    git(
        &repo.path,
        ["update-ref", "refs/remotes/origin/main", &head],
    )
    .unwrap();
    assert_eq!(
        discover_repository(&repo.path).unwrap().base_ref,
        "origin/main"
    );
    git(
        &repo.path,
        ["update-ref", "refs/remotes/origin/trunk", &head],
    )
    .unwrap();
    git(
        &repo.path,
        [
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    )
    .unwrap();
    assert_eq!(
        discover_repository(&repo.path).unwrap().base_ref,
        "origin/trunk"
    );
    assert!(capture_snapshot(&repo.path, "--output=unexpected").is_err());
}

#[test]
fn current_branch_follows_terminal_renames_and_reports_detached_head() {
    let repo = Repository::new();
    repo.branch();
    assert_eq!(current_branch(&repo.path).unwrap(), "feature");
    git(&repo.path, ["branch", "-m", "renamed-by-terminal"]).unwrap();
    assert_eq!(current_branch(&repo.path).unwrap(), "renamed-by-terminal");
    git(&repo.path, ["checkout", "--detach", "HEAD"]).unwrap();
    assert_eq!(current_branch(&repo.path).unwrap(), "(detached)");
}

#[test]
fn push_uses_exact_committed_head_and_reports_uncommitted_files() {
    let repo = Repository::new();
    let remote = repo.root.join("remote.git");
    fs::create_dir(&remote).unwrap();
    git(&remote, ["init", "--bare"]).unwrap();
    git(
        &repo.path,
        ["remote", "add", "origin", remote.to_str().unwrap()],
    )
    .unwrap();
    repo.branch();
    let old_head = commit(&repo.path, "HEAD").unwrap();
    repo.write("anchor.txt", "committed feature\n");
    repo.commit("feature");
    repo.write("anchor.txt", "uncommitted feature\n");
    assert!(push(&repo.path, &old_head)
        .unwrap_err()
        .contains("head changed"));
    let head = commit(&repo.path, "HEAD").unwrap();
    let result = push(&repo.path, &head).unwrap();
    assert!(result.dirty);
    assert_eq!(result.head, head);
    assert_eq!(commit(&remote, "refs/heads/feature").unwrap(), head);
    assert_eq!(
        git_text(&repo.path, ["rev-parse", "--abbrev-ref", "@{upstream}"]).unwrap(),
        "origin/feature"
    );
    assert_eq!(
        fs::read_to_string(repo.path.join("anchor.txt")).unwrap(),
        "uncommitted feature\n"
    );
}

#[test]
fn github_missing_data_is_unknown_and_status_contexts_are_supported() {
    let value = json!({
        "number": 42, "headRefOid": "a".repeat(40),
        "statusCheckRollup": [
            {"__typename":"CheckRun", "name":"tests", "status":"IN_PROGRESS", "conclusion":null},
            {"__typename":"StatusContext", "context":"legacy", "state":"FAILURE", "targetUrl":"https://example.invalid/check"}
        ]
    });
    let pr = parse_pull_request(&value).unwrap();
    assert_eq!(pr.mergeable, "UNKNOWN");
    assert_eq!(pr.merge_state, "UNKNOWN");
    assert_eq!(pr.state, "UNKNOWN");
    assert_eq!(pr.checks[0].conclusion, "UNKNOWN");
    assert_eq!(pr.checks[1].name, "legacy");
    assert_eq!(pr.checks[1].conclusion, "FAILURE");
    assert!(!pr.auto_merge);
}
