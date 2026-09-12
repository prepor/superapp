//! Desktop Git and GitHub operations. Call these blocking functions on a worker.
//!
//! Snapshots never use or update the person's index. Their trees are retained in
//! `refs/workshop/snapshots/` and the serialized patches remain readable offline.
//! Snapshotting is a bounded stabilization attempt, not an atomic filesystem lock;
//! overlapping agent runs must be attributed to a shared interval by the caller.

use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryInfo {
    pub path: String,
    pub name: String,
    pub base_ref: String,
    pub head: String,
    pub branch: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreatedWorktree {
    pub label: String,
    pub path: String,
    pub branch: String,
    pub base_ref: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub head: String,
    pub base_ref: String,
    pub base_oid: String,
    pub tree_oid: String,
    pub files: Vec<FileDiff>,
    /// Includes staged, unstaged, and ordinary (non-ignored) untracked files.
    pub dirty: bool,
    pub conflicts: Vec<String>,
    /// Explicit warnings for repository boundaries whose interior is not a diff.
    pub warnings: Vec<String>,
    pub captured_at: u64,
}

#[cfg(test)]
impl Snapshot {
    pub fn added(&self) -> usize {
        self.files.iter().map(|file| file.added).sum()
    }

    pub fn deleted(&self) -> usize {
        self.files.iter().map(|file| file.deleted).sum()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    /// Git status code: A, M, D, R, T (including optional similarity score).
    pub status: String,
    pub old_blob: String,
    pub new_blob: String,
    pub old_mode: String,
    pub new_mode: String,
    pub patch: String,
    pub hunks: Vec<DiffHunk>,
    pub added: usize,
    pub deleted: usize,
    pub binary: bool,
    /// Pure rename/mode changes and binary/submodule changes do not invent lines.
    pub non_text: bool,
    /// Complete diff plus context, excluding only path and hunk coordinates.
    pub fingerprint: String,
}

#[cfg(test)]
impl FileDiff {
    pub fn changed_lines(&self) -> usize {
        self.added + self.deleted
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffLine {
    /// context, add, delete, or note (the no-newline marker).
    pub kind: String,
    pub text: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileCorrespondence {
    pub current_path: String,
    pub previous_path: Option<String>,
    /// Only true for a unique complete diff match on both sides.
    pub preserved: bool,
    pub reason: String,
}

fn git_command(path: &Path) -> Command {
    let mut command = Command::new("git");
    executable_path(&mut command);
    command
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(path)
        .args([
            "-c",
            "core.quotepath=false",
            "-c",
            "diff.external=",
            "-c",
            "core.fsmonitor=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(name);
    }
    command
}

fn finish(mut command: Command, description: &str) -> Result<Vec<u8>> {
    let output = command
        .output()
        .map_err(|error| format!("{description}: {error}"))?;
    checked_output(output, description)
}

fn checked_output(output: Output, description: &str) -> Result<Vec<u8>> {
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let detail = if error.is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        } else {
            error
        };
        return Err(format!("{description}: {detail}"));
    }
    Ok(output.stdout)
}

fn git<I, S>(path: &Path, args: I) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = git_command(path);
    command.args(args);
    finish(command, "Git")
}

fn git_text<I, S>(path: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    String::from_utf8(git(path, args)?)
        .map(|value| value.trim_end_matches('\n').to_owned())
        .map_err(|_| "Git returned text that is not UTF-8".to_owned())
}

fn commit(path: &Path, reference: &str) -> Result<String> {
    if reference.is_empty() || reference.starts_with('-') || reference.contains('\0') {
        return Err("Invalid Git reference".to_owned());
    }
    git_text(
        path,
        [
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn discover_repository(path: impl AsRef<Path>) -> Result<RepositoryInfo> {
    let path =
        fs::canonicalize(path.as_ref()).map_err(|error| format!("Open repository: {error}"))?;
    let top = git_text(&path, ["rev-parse", "--show-toplevel"])?;
    let path = fs::canonicalize(top).map_err(|error| format!("Repository path: {error}"))?;
    let head = commit(&path, "HEAD")?;
    let branch = current_branch(&path)?;
    let remote_default = git_text(
        &path,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    );
    let base_ref = remote_default
        .ok()
        .filter(|reference| commit(&path, reference).is_ok())
        .or_else(|| {
            commit(&path, "origin/main")
                .ok()
                .map(|_| "origin/main".to_owned())
        })
        .unwrap_or_else(|| {
            if branch == "(detached)" {
                head.clone()
            } else {
                branch.clone()
            }
        });
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or("Repository name is not UTF-8")?
        .to_owned();
    Ok(RepositoryInfo {
        path: path
            .to_str()
            .ok_or("Repository path is not UTF-8")?
            .to_owned(),
        name,
        base_ref,
        head,
        branch,
    })
}

/// Branch names are current Git state, not the durable workspace identity.
pub fn current_branch(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let mut command = git_command(path);
    command.args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let output = command
        .output()
        .map_err(|error| format!("Read current Git branch: {error}"))?;
    if output.status.code() == Some(1) && commit(path, "HEAD").is_ok() {
        return Ok("(detached)".to_owned());
    }
    String::from_utf8(checked_output(output, "Read current Git branch")?)
        .map(|branch| branch.trim_end_matches('\n').to_owned())
        .map_err(|_| "The Git branch name is not UTF-8".to_owned())
}

/// Creates a branch and worktree; never checks out or alters the source checkout.
#[cfg(test)]
pub fn create_worktree(
    repository: impl AsRef<Path>,
    root: impl AsRef<Path>,
) -> Result<CreatedWorktree> {
    let repository = discover_repository(repository)?;
    let source = Path::new(&repository.path);
    fs::create_dir_all(root.as_ref())
        .map_err(|error| format!("Create worktree directory: {error}"))?;
    let root =
        fs::canonicalize(root.as_ref()).map_err(|error| format!("Worktree directory: {error}"))?;
    let start = commit(source, &repository.base_ref)?;
    const CITIES: [&str; 20] = [
        "zurich", "basel", "bern", "geneva", "lausanne", "lucerne", "lugano", "davos", "oslo",
        "porto", "kyoto", "riga", "vienna", "turin", "lyon", "tallinn", "utrecht", "verona",
        "bergen", "delft",
    ];
    for suffix in 1..1000 {
        for city in CITIES {
            let label = if suffix == 1 {
                city.to_owned()
            } else {
                format!("{city}-v{suffix}")
            };
            let path = root.join(&label);
            let branch = format!("workshop/{label}");
            if path.exists()
                || git(
                    source,
                    [
                        "show-ref",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{branch}"),
                    ],
                )
                .is_ok()
            {
                continue;
            }
            let result = git(
                source,
                [
                    OsStr::new("worktree"),
                    OsStr::new("add"),
                    OsStr::new("-b"),
                    OsStr::new(&branch),
                    OsStr::new("--"),
                    path.as_os_str(),
                    OsStr::new(&start),
                ],
            );
            match result {
                Ok(_) => {
                    return Ok(CreatedWorktree {
                        label,
                        path: path
                            .to_str()
                            .ok_or("Worktree path is not UTF-8")?
                            .to_owned(),
                        branch,
                        base_ref: repository.base_ref,
                    })
                }
                Err(error) => return Err(error),
            }
        }
    }
    Err("Could not allocate an unused workspace label".to_owned())
}

/// Finish the UI's already-reserved workspace. A label is a path component and
/// a branch suffix, not a free-form path or revision. Existing paths/branches
/// always produce an error; this function never removes a failed checkout.
pub fn create_worktree_named(
    repository: &Path,
    root: &Path,
    label: &str,
    base_ref: &str,
) -> Result<CreatedWorktree> {
    if label.is_empty()
        || label.len() > 80
        || label.starts_with('-')
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("Invalid generated workspace label".to_owned());
    }
    let repository = discover_repository(repository)?;
    let source = Path::new(&repository.path);
    let start = commit(source, base_ref)?;
    fs::create_dir_all(root).map_err(|error| format!("Create worktree directory: {error}"))?;
    let root = fs::canonicalize(root).map_err(|error| format!("Worktree directory: {error}"))?;
    let path = root.join(label);
    let branch = format!("workshop/{label}");
    if path.exists()
        || git(
            source,
            [
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_ok()
    {
        return Err(format!(
            "Workspace {label} already exists; choose another generated label"
        ));
    }
    git(
        source,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(&branch),
            OsStr::new("--"),
            path.as_os_str(),
            OsStr::new(&start),
        ],
    )?;
    Ok(CreatedWorktree {
        label: label.to_owned(),
        path: path
            .to_str()
            .ok_or("Worktree path is not UTF-8")?
            .to_owned(),
        branch,
        base_ref: base_ref.to_owned(),
    })
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TemporaryIndex {
    directory: PathBuf,
    index: PathBuf,
}

impl TemporaryIndex {
    fn new() -> Result<Self> {
        for _ in 0..100 {
            let directory = std::env::temp_dir().join(format!(
                "superapp-workshop-{}-{}-{}",
                std::process::id(),
                now(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        index: directory.join("index"),
                        directory,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("Create temporary Git index: {error}")),
            }
        }
        Err("Could not allocate temporary Git index".to_owned())
    }

    fn git(&self, path: &Path, args: &[&str]) -> Result<String> {
        let mut command = git_command(path);
        command.env("GIT_INDEX_FILE", &self.index).args(args);
        let result = finish(command, "Capture worktree")?;
        String::from_utf8(result)
            .map(|text| text.trim_end_matches('\n').to_owned())
            .map_err(|_| "Git returned invalid UTF-8".to_owned())
    }
}

impl Drop for TemporaryIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub fn capture_snapshot(path: impl AsRef<Path>, base_ref: &str) -> Result<Snapshot> {
    let repository = discover_repository(path)?;
    let path = Path::new(&repository.path);
    if git_text(path, ["config", "--bool", "core.sparseCheckout"])
        .ok()
        .as_deref()
        == Some("true")
    {
        return Err("Review snapshots currently require a complete checkout; this repository uses sparse checkout".to_owned());
    }
    let head = commit(path, "HEAD")?;
    let base = commit(path, base_ref)?;
    let base_oid = git_text(path, ["merge-base", &base, &head])?;
    let index = TemporaryIndex::new()?;
    index.git(path, &["read-tree", &head])?;
    index.git(path, &["add", "--all", "--", "."])?;
    let mut tree_oid = index.git(path, &["write-tree"])?;
    let mut stable = false;
    for _ in 0..3 {
        index.git(path, &["add", "--all", "--", "."])?;
        let second = index.git(path, &["write-tree"])?;
        if second == tree_oid {
            stable = true;
            break;
        }
        tree_oid = second;
    }
    if !stable || commit(path, "HEAD")? != head {
        return Err("The workspace changed while capturing changes; refresh when the current Git operation finishes".to_owned());
    }
    git(
        path,
        [
            "update-ref",
            &format!("refs/workshop/snapshots/{tree_oid}"),
            &tree_oid,
        ],
    )?;
    let dirty = !git(
        path,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?
    .is_empty();
    let conflicts = git(path, ["diff", "--name-only", "--diff-filter=U", "-z", "--"])?
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            String::from_utf8(part.to_vec())
                .map_err(|_| "A conflicted path is not UTF-8".to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    let files = diff_trees(path, &base_oid, &tree_oid)?;
    let warnings = submodule_warnings(path)?;
    Ok(Snapshot {
        head,
        base_ref: base_ref.to_owned(),
        base_oid,
        tree_oid,
        files,
        dirty,
        conflicts,
        warnings,
        captured_at: now(),
    })
}

fn submodule_warnings(path: &Path) -> Result<Vec<String>> {
    let status = git(
        path,
        ["status", "--porcelain=v2", "-z", "--untracked-files=no"],
    )?;
    let mut warnings = Vec::new();
    for record in status
        .split(|byte| *byte == 0)
        .filter(|record| record.starts_with(b"1 "))
    {
        let text = String::from_utf8_lossy(record);
        let fields: Vec<_> = text.splitn(9, ' ').collect();
        if fields.len() == 9 && fields[2].starts_with('S') && fields[2] != "S..." {
            warnings.push(format!("Submodule {} has changes; its nested working files require a separate project review", fields[8]));
        }
    }
    Ok(warnings)
}

/// A turn's exact before/after tree comparison; neither later edits nor rebases
/// alter its patch. The caller stores both snapshots before dispatching a run.
pub fn compare_snapshots(
    path: impl AsRef<Path>,
    before: &Snapshot,
    after: &Snapshot,
) -> Result<Snapshot> {
    let files = diff_trees(path.as_ref(), &before.tree_oid, &after.tree_oid)?;
    Ok(Snapshot {
        head: after.head.clone(),
        base_ref: format!("snapshot:{}", before.tree_oid),
        base_oid: before.tree_oid.clone(),
        tree_oid: after.tree_oid.clone(),
        files,
        dirty: after.dirty,
        conflicts: after.conflicts.clone(),
        warnings: after.warnings.clone(),
        captured_at: after.captured_at,
    })
}

fn diff_trees(path: &Path, before: &str, after: &str) -> Result<Vec<FileDiff>> {
    // Inputs are object IDs, not revisions supplied as positional flags.
    validate_oid(before)?;
    validate_oid(after)?;
    let raw = git(
        path,
        [
            "diff-tree",
            "--no-commit-id",
            "-r",
            "--raw",
            "-z",
            "--no-abbrev",
            "--find-renames",
            before,
            after,
            "--",
        ],
    )?;
    let records: Vec<_> = raw.split(|byte| *byte == 0).collect();
    let mut cursor = 0;
    let mut files = Vec::new();
    while cursor < records.len() && !records[cursor].is_empty() {
        let header =
            std::str::from_utf8(records[cursor]).map_err(|_| "Invalid raw Git diff".to_owned())?;
        let columns: Vec<_> = header.trim_start_matches(':').split(' ').collect();
        if columns.len() != 5 {
            return Err("Invalid raw Git diff header".to_owned());
        }
        cursor += 1;
        let first = records.get(cursor).ok_or("Missing Git diff path")?;
        let first = std::str::from_utf8(first)
            .map_err(|_| {
                "A changed filename is not UTF-8; review cannot safely identify it".to_owned()
            })?
            .to_owned();
        cursor += 1;
        let (old_path, file_path) = if columns[4].starts_with('R') || columns[4].starts_with('C') {
            let second = records.get(cursor).ok_or("Missing renamed Git path")?;
            cursor += 1;
            (
                Some(first),
                std::str::from_utf8(second)
                    .map_err(|_| "A renamed filename is not UTF-8".to_owned())?
                    .to_owned(),
            )
        } else {
            (None, first)
        };
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--unified=3",
            before,
            after,
            "--",
        ];
        // Object-to-object diff with both paths could include an unrelated change
        // when rename paths are reused. Blob-to-blob patch below avoids that.
        args.push(&file_path);
        let binary;
        let patch;
        let mut ambiguous_context = false;
        if columns[0] == "160000" || columns[1] == "160000" {
            patch = git_text(path, args)?;
            binary = false;
        } else {
            let old = blob(path, columns[2])?;
            let new = blob(path, columns[3])?;
            binary = old.contains(&0)
                || new.contains(&0)
                || std::str::from_utf8(&old).is_err()
                || std::str::from_utf8(&new).is_err();
            if binary {
                patch = format!("Binary file changed: {file_path}\n");
            } else {
                patch = text_patch(&file_path, old_path.as_deref(), &old, &new);
                ambiguous_context = context_is_ambiguous(&parse_hunks(&patch)?, &old, &new);
            }
        }
        let hunks = parse_hunks(&patch)?;
        let added = if columns[0] == "160000" || columns[1] == "160000" {
            0
        } else {
            hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind == "add")
                .count()
        };
        let deleted = if columns[0] == "160000" || columns[1] == "160000" {
            0
        } else {
            hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind == "delete")
                .count()
        };
        let mode_changed =
            columns[0] != columns[1] && columns[0] != "000000" && columns[1] != "000000";
        let mut file = FileDiff {
            path: file_path,
            old_path,
            status: columns[4].to_owned(),
            old_blob: columns[2].to_owned(),
            new_blob: columns[3].to_owned(),
            old_mode: columns[0].to_owned(),
            new_mode: columns[1].to_owned(),
            patch,
            hunks,
            added,
            deleted,
            binary,
            non_text: binary
                || added + deleted == 0
                || mode_changed
                || columns[0] == "160000"
                || columns[1] == "160000",
            fingerprint: String::new(),
        };
        file.fingerprint = fingerprint(&file, ambiguous_context);
        files.push(file);
    }
    Ok(files)
}

fn validate_oid(oid: &str) -> Result<()> {
    if ![40, 64].contains(&oid.len()) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Expected a complete Git object ID".to_owned());
    }
    Ok(())
}

fn blob(path: &Path, oid: &str) -> Result<Vec<u8>> {
    validate_oid(oid)?;
    if oid.bytes().all(|byte| byte == b'0') {
        return Ok(Vec::new());
    }
    git(path, ["cat-file", "blob", oid])
}

fn text_patch(path: &str, old_path: Option<&str>, old: &[u8], new: &[u8]) -> String {
    let old = std::str::from_utf8(old).expect("text checked before diff");
    let new = std::str::from_utf8(new).expect("text checked before diff");
    let old: Vec<_> = old.split_inclusive('\n').collect();
    let new: Vec<_> = new.split_inclusive('\n').collect();
    let operations = similar::capture_diff_slices(similar::Algorithm::Myers, &old, &new);
    let groups = similar::group_diff_ops(operations, 3);
    if groups.is_empty() {
        return String::new();
    }
    let mut patch = format!(
        "--- a/{}\n+++ b/{}\n",
        old_path.unwrap_or(path).escape_debug(),
        path.escape_debug()
    );
    for group in groups {
        let first = group.first().expect("diff group is nonempty");
        let last = group.last().expect("diff group is nonempty");
        let old_count = last.old_range().end - first.old_range().start;
        let new_count = last.new_range().end - first.new_range().start;
        let old_start = first.old_range().start + usize::from(old_count > 0);
        let new_start = first.new_range().start + usize::from(new_count > 0);
        patch.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        for change in group
            .iter()
            .flat_map(|operation| operation.iter_changes(&old, &new))
        {
            patch.push(match change.tag() {
                similar::ChangeTag::Equal => ' ',
                similar::ChangeTag::Insert => '+',
                similar::ChangeTag::Delete => '-',
            });
            patch.push_str(change.value());
            if !change.value().ends_with('\n') {
                patch.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    patch
}

fn parse_hunks(patch: &str) -> Result<Vec<DiffHunk>> {
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let (mut old, mut new) = (0, 0);
    for line in patch.split_terminator('\n') {
        if line.starts_with("@@ ") {
            let mut parts = line.split(' ');
            parts.next();
            let old_range = parts.next().ok_or("Missing old hunk range")?;
            let new_range = parts.next().ok_or("Missing new hunk range")?;
            let (old_start, old_count) = parse_range(old_range)?;
            let (new_start, new_count) = parse_range(new_range)?;
            old = old_start;
            new = new_start;
            hunks.push(DiffHunk {
                header: line.to_owned(),
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            });
        } else if let Some(hunk) = hunks.last_mut() {
            let (kind, old_line, new_line) = match line.as_bytes().first() {
                Some(b' ') => {
                    let pair = ("context", Some(old), Some(new));
                    old += 1;
                    new += 1;
                    pair
                }
                Some(b'+') => {
                    let pair = ("add", None, Some(new));
                    new += 1;
                    pair
                }
                Some(b'-') => {
                    let pair = ("delete", Some(old), None);
                    old += 1;
                    pair
                }
                Some(b'\\') => ("note", None, None),
                _ => continue,
            };
            hunk.lines.push(DiffLine {
                kind: kind.to_owned(),
                text: line[1..].to_owned(),
                old_line,
                new_line,
            });
        }
    }
    Ok(hunks)
}

fn parse_range(range: &str) -> Result<(usize, usize)> {
    let range = range.get(1..).ok_or("Invalid hunk range")?;
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    Ok((
        start.parse().map_err(|_| "Invalid hunk start")?,
        count.parse().map_err(|_| "Invalid hunk count")?,
    ))
}

fn context_is_ambiguous(hunks: &[DiffHunk], old: &[u8], new: &[u8]) -> bool {
    let old: Vec<_> = std::str::from_utf8(old)
        .expect("text diff")
        .split_terminator('\n')
        .collect();
    let new: Vec<_> = std::str::from_utf8(new)
        .expect("text diff")
        .split_terminator('\n')
        .collect();
    for hunk in hunks {
        let old_anchor: Vec<_> = hunk
            .lines
            .iter()
            .filter(|line| matches!(line.kind.as_str(), "context" | "delete"))
            .map(|line| line.text.as_str())
            .collect();
        let new_anchor: Vec<_> = hunk
            .lines
            .iter()
            .filter(|line| matches!(line.kind.as_str(), "context" | "add"))
            .map(|line| line.text.as_str())
            .collect();
        if (!old_anchor.is_empty()
            && old
                .windows(old_anchor.len())
                .filter(|window| *window == old_anchor)
                .take(2)
                .count()
                > 1)
            || (!new_anchor.is_empty()
                && new
                    .windows(new_anchor.len())
                    .filter(|window| *window == new_anchor)
                    .take(2)
                    .count()
                    > 1)
        {
            return true;
        }
    }
    false
}

fn fingerprint(file: &FileDiff, ambiguous_context: bool) -> String {
    let mut identity =
        format!("workshop-file-v1\0{}\0{}\0", file.old_mode, file.new_mode).into_bytes();
    if file.binary
        || file.hunks.is_empty()
        || file.old_mode == "160000"
        || file.new_mode == "160000"
    {
        identity.extend_from_slice(format!("{}\0{}", file.old_blob, file.new_blob).as_bytes());
    } else {
        // Hunk boundaries and every changed/context/no-newline byte participate.
        // Coordinates and paths are deliberately locators, never identities.
        for hunk in &file.hunks {
            identity.extend_from_slice(b"\0hunk\0");
            for line in &hunk.lines {
                identity.extend_from_slice(line.kind.as_bytes());
                identity.push(0);
                identity.extend_from_slice(line.text.as_bytes());
                identity.push(b'\n');
            }
        }
        if ambiguous_context {
            // A repeated context block cannot safely follow a rebase/relocation.
            // Retain only exact content identity (a pure path rename is safe).
            identity.extend_from_slice(
                format!("\0ambiguous\0{}\0{}", file.old_blob, file.new_blob).as_bytes(),
            );
        }
    }
    digest(&SHA256, &identity)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn correspond_files(previous: &[FileDiff], current: &[FileDiff]) -> Vec<FileCorrespondence> {
    let mut previous_by_identity: HashMap<&str, Vec<&FileDiff>> = HashMap::new();
    let mut current_counts: HashMap<&str, usize> = HashMap::new();
    for file in previous {
        previous_by_identity
            .entry(&file.fingerprint)
            .or_default()
            .push(file);
    }
    for file in current {
        *current_counts.entry(&file.fingerprint).or_default() += 1;
    }
    current
        .iter()
        .map(|file| {
            let matches = previous_by_identity.get(file.fingerprint.as_str());
            let unique = matches.is_some_and(|files| files.len() == 1)
                && current_counts[file.fingerprint.as_str()] == 1;
            let previous_path = if unique {
                Some(matches.unwrap()[0].path.clone())
            } else {
                let same_path = previous.iter().find(|old| old.path == file.path);
                let locators: Vec<_> = previous
                    .iter()
                    .filter(|old| {
                        file.old_path.as_ref().is_some_and(|path| {
                            path == &old.path || Some(path) == old.old_path.as_ref()
                        })
                    })
                    .collect();
                same_path
                    .or_else(|| {
                        if locators.len() == 1 {
                            Some(locators[0])
                        } else {
                            None
                        }
                    })
                    .map(|old| old.path.clone())
            };
            let reason = if unique {
                "Complete file diff and context match uniquely"
            } else if matches.is_some() {
                "Ambiguous identical file diffs require review"
            } else if previous_path.is_some() {
                "The file changed; review the complete file again"
            } else {
                "New file change"
            };
            FileCorrespondence {
                current_path: file.path.clone(),
                previous_path,
                preserved: unique,
                reason: reason.to_owned(),
            }
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitHubCheck {
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitHubPullRequest {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub head: String,
    pub head_branch: String,
    pub base_branch: String,
    pub mergeable: String,
    pub merge_state: String,
    pub review_decision: String,
    pub auto_merge: bool,
    pub checks: Vec<GitHubCheck>,
    pub observed_at: u64,
}

fn gh_command(path: &Path) -> Command {
    let mut command = Command::new("gh");
    executable_path(&mut command);
    command
        .current_dir(path)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env_remove("GH_REPO");
    command
}

fn executable_path(command: &mut Command) {
    if let Some(path) = expanded_path(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").as_deref(),
    ) {
        command.env("PATH", path);
    }
}

fn expanded_path(inherited: Option<&OsStr>, home: Option<&OsStr>) -> Option<OsString> {
    let mut paths: Vec<_> = inherited
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    for path in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ] {
        let path = PathBuf::from(path);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    if let Some(home) = home {
        let path = PathBuf::from(home).join(".local/bin");
        if !paths.contains(&path) && std::env::join_paths([&path]).is_ok() {
            paths.push(path);
        }
    }
    std::env::join_paths(paths).ok()
}

fn gh_json(path: &Path, args: &[&str]) -> Result<Value> {
    let mut command = gh_command(path);
    command.args(args);
    serde_json::from_slice(&finish(command, "GitHub")?)
        .map_err(|error| format!("Read GitHub response: {error}"))
}

pub fn pull_request(path: impl AsRef<Path>) -> Result<Option<GitHubPullRequest>> {
    pull_request_number(path.as_ref(), None)
}

fn pull_request_number(path: &Path, number: Option<u64>) -> Result<Option<GitHubPullRequest>> {
    let number_arg = number.map(|number| number.to_string());
    let mut args = vec!["pr", "view"];
    if let Some(number) = &number_arg {
        args.push(number);
    }
    args.extend(["--json", "number,url,title,state,isDraft,headRefName,headRefOid,baseRefName,mergeable,mergeStateStatus,reviewDecision,statusCheckRollup,autoMergeRequest"]);
    let value = match gh_json(path, &args) {
        Ok(value) => value,
        Err(error) if error.contains("no pull requests found for branch") => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(parse_pull_request(&value)?))
}

fn value_text(value: &Value, key: &str, fallback: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

fn parse_pull_request(value: &Value) -> Result<GitHubPullRequest> {
    let checks = value
        .get("statusCheckRollup")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|check| {
            let context = check.get("__typename").and_then(Value::as_str) == Some("StatusContext");
            GitHubCheck {
                name: value_text(
                    check,
                    if context { "context" } else { "name" },
                    "Unknown check",
                ),
                status: value_text(check, if context { "state" } else { "status" }, "UNKNOWN"),
                conclusion: value_text(
                    check,
                    if context { "state" } else { "conclusion" },
                    "UNKNOWN",
                ),
                url: value_text(check, if context { "targetUrl" } else { "detailsUrl" }, ""),
            }
        })
        .collect();
    let head = value_text(value, "headRefOid", "");
    validate_oid(&head)?;
    Ok(GitHubPullRequest {
        number: value
            .get("number")
            .and_then(Value::as_u64)
            .ok_or("GitHub response has no PR number")?,
        url: value_text(value, "url", ""),
        title: value_text(value, "title", ""),
        state: value_text(value, "state", "UNKNOWN"),
        draft: value
            .get("isDraft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        head,
        head_branch: value_text(value, "headRefName", ""),
        base_branch: value_text(value, "baseRefName", ""),
        mergeable: value_text(value, "mergeable", "UNKNOWN"),
        merge_state: value_text(value, "mergeStateStatus", "UNKNOWN"),
        review_decision: value_text(value, "reviewDecision", "UNKNOWN"),
        auto_merge: value
            .get("autoMergeRequest")
            .is_some_and(|request| !request.is_null()),
        checks,
        observed_at: now(),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushResult {
    pub head: String,
    pub branch: String,
    pub dirty: bool,
    pub output: String,
}

/// Pushes exactly the expected committed head. Uncommitted files stay local and
/// are reported in the result; no commit/staging or force push is implicit.
pub fn push(path: impl AsRef<Path>, expected_head: &str) -> Result<PushResult> {
    let path = path.as_ref();
    validate_oid(expected_head)?;
    if commit(path, "HEAD")? != expected_head {
        return Err("Workspace head changed; refresh before pushing".to_owned());
    }
    let branch = git_text(path, ["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map_err(|_| "Cannot push a detached workspace; check out its branch first".to_owned())?;
    git(path, ["check-ref-format", &format!("refs/heads/{branch}")])?;
    let output = git_text(
        path,
        [
            "push",
            "--porcelain",
            "origin",
            &format!("{expected_head}:refs/heads/{branch}"),
        ],
    )?;
    // Using the OID closes a local-HEAD race; configure upstream separately,
    // because an OID push cannot infer which local branch owns that upstream.
    git(
        path,
        ["config", &format!("branch.{branch}.remote"), "origin"],
    )?;
    git(
        path,
        [
            "config",
            &format!("branch.{branch}.merge"),
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let dirty = !git(
        path,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?
    .is_empty();
    Ok(PushResult {
        head: expected_head.to_owned(),
        branch,
        dirty,
        output,
    })
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

fn validate_pr_head(path: &Path, number: u64, expected_head: &str) -> Result<GitHubPullRequest> {
    validate_oid(expected_head)?;
    let pr = pull_request_number(path, Some(number))?.ok_or("Pull request no longer exists")?;
    if pr.head != expected_head {
        return Err("GitHub head changed; refresh the pull request before continuing".to_owned());
    }
    if pr.state != "OPEN" {
        return Err(format!("Pull request is {}", pr.state));
    }
    Ok(pr)
}

/// Personal review marks are deliberately not a gate. GitHub enforces actual
/// repository rules, and --match-head-commit closes the head-change race.
pub fn merge_pull_request(
    path: impl AsRef<Path>,
    number: u64,
    expected_head: &str,
    method: MergeMethod,
    auto: bool,
) -> Result<String> {
    let path = path.as_ref();
    let pr = validate_pr_head(path, number, expected_head)?;
    if pr.draft {
        return Err("A draft pull request must be made ready before merging".to_owned());
    }
    if commit(path, "HEAD")? != expected_head {
        return Err("Local and GitHub heads differ; refresh and push before merging".to_owned());
    }
    if !git(
        path,
        ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err("Uncommitted workspace changes are not part of this pull request; commit and push them before merging".to_owned());
    }
    let mut command = gh_command(path);
    command.args([
        "pr",
        "merge",
        &number.to_string(),
        "--match-head-commit",
        expected_head,
        match method {
            MergeMethod::Merge => "--merge",
            MergeMethod::Squash => "--squash",
            MergeMethod::Rebase => "--rebase",
        },
    ]);
    if auto {
        command.arg("--auto");
    }
    Ok(
        String::from_utf8_lossy(&finish(command, "Merge GitHub pull request")?)
            .trim()
            .to_owned(),
    )
}

pub fn cancel_auto_merge(path: impl AsRef<Path>, number: u64, expected_head: &str) -> Result<()> {
    let path = path.as_ref();
    validate_pr_head(path, number, expected_head)?;
    let mut command = gh_command(path);
    command.args([
        "pr",
        "merge",
        &number.to_string(),
        "--disable-auto",
        "--match-head-commit",
        expected_head,
    ]);
    finish(command, "Disable GitHub auto-merge")?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublishedComment {
    pub id: u64,
    pub url: String,
    pub body: String,
    pub path: Option<String>,
}

fn repo_slug(path: &Path) -> Result<String> {
    let repo = gh_json(path, &["repo", "view", "--json", "nameWithOwner"])?;
    let slug = repo
        .get("nameWithOwner")
        .and_then(Value::as_str)
        .ok_or("GitHub repository identity is missing")?;
    if slug.split('/').count() != 2
        || !slug
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._/".contains(character))
    {
        return Err("GitHub repository identity is invalid".to_owned());
    }
    Ok(slug.to_owned())
}

fn gh_post(path: &Path, endpoint: &str, payload: Value) -> Result<Value> {
    let mut command = gh_command(path);
    command
        .args(["api", "--method", "POST", endpoint, "--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("Post to GitHub: {error}"))?;
    let encoded = serde_json::to_vec(&payload).map_err(|error| error.to_string())?;
    let write = child
        .stdin
        .take()
        .ok_or("GitHub input unavailable")?
        .write_all(&encoded);
    if let Err(error) = write {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("Post to GitHub: {error}"));
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("Post to GitHub: {error}"))?;
    serde_json::from_slice(&checked_output(output, "Post to GitHub")?)
        .map_err(|error| format!("Read published GitHub comment: {error}"))
}

/// Post a general PR comment or a whole-file review comment. File comments must
/// match the published blob and PR diff; callers cannot publish a local-only
/// file as if it were reviewed on GitHub. No line/range comment is exposed.
/// The caller owns durable pending/ambiguous operation state: never blindly retry
/// a network failure that could have happened after GitHub accepted the comment.
pub fn post_comment(
    path: impl AsRef<Path>,
    number: u64,
    expected_head: &str,
    body: &str,
    file: Option<&FileDiff>,
) -> Result<PublishedComment> {
    if body.trim().is_empty() {
        return Err("A GitHub comment cannot be empty".to_owned());
    }
    let path = path.as_ref();
    let pr = validate_pr_head(path, number, expected_head)?;
    let slug = repo_slug(path)?;
    let (endpoint, payload) = if let Some(file) = file {
        // Confirm both PR membership and complete local vs published file identity.
        let list = gh_json(
            path,
            &["pr", "view", &number.to_string(), "--json", "files"],
        )?;
        let exists = list
            .get("files")
            .and_then(Value::as_array)
            .is_some_and(|files| {
                files.iter().any(|entry| {
                    entry.get("path").and_then(Value::as_str) == Some(file.path.as_str())
                })
            });
        if !exists {
            return Err(
                "This file is not in the published PR diff; push it or choose a general PR comment"
                    .to_owned(),
            );
        }
        let published = git_text(path, ["ls-tree", "-z", &pr.head, "--", &file.path])?;
        let published_blob = published.split_whitespace().nth(2).unwrap_or("");
        let published_mode = published.split_whitespace().next().unwrap_or("000000");
        let expected_blob = if file.new_mode == "000000" {
            ""
        } else {
            file.new_blob.as_str()
        };
        if published_blob != expected_blob || published_mode != file.new_mode {
            return Err(
                "This file has unpublished changes; push them or choose a general PR comment"
                    .to_owned(),
            );
        }
        (
            format!("repos/{slug}/pulls/{number}/comments"),
            json!({"body":body,"commit_id":expected_head,"path":file.path,"subject_type":"file"}),
        )
    } else {
        (
            format!("repos/{slug}/issues/{number}/comments"),
            json!({"body":body}),
        )
    };
    let value = gh_post(path, &endpoint, payload)?;
    Ok(PublishedComment {
        id: value.get("id").and_then(Value::as_u64).ok_or(
            "GitHub accepted the request but returned no comment ID; check the PR before retrying",
        )?,
        url: value_text(&value, "html_url", ""),
        body: value_text(&value, "body", body),
        path: file.map(|file| file.path.clone()),
    })
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
