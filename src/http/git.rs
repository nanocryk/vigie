//! Pending changes of a git repository (HEAD against the working tree, untracked
//! files included), read through the `git` command line.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use axum::{
    Json,
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::http::api::ApiError;
use crate::http::files::{PathQuery, blocking, canonical, display_path, is_binary};

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// A file with more changed lines than this keeps its counts but not its lines.
const FILE_LINES_MAX: usize = 5_000;
/// Untracked files past this many are listed without reading them.
const UNTRACKED_READ_MAX: usize = 200;
const UNTRACKED_BYTES_MAX: u64 = 1024 * 1024;
/// The tree of a repository with no commit yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Debug)]
pub enum GitError {
    NotFound,
    NotARepo,
    Failed(String),
}

impl From<io::Error> for GitError {
    fn from(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            _ => Self::Failed(err.to_string()),
        }
    }
}

impl From<GitError> for ApiError {
    fn from(err: GitError) -> Self {
        match err {
            GitError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found"),
            GitError::NotARepo => Self::new(StatusCode::BAD_REQUEST, "not_a_repo"),
            GitError::Failed(message) => Self::internal("git failed", message),
        }
    }
}

impl IntoResponse for GitError {
    fn into_response(self) -> Response {
        ApiError::from(self).into_response()
    }
}

/// The repository containing `path`: the closest ancestor holding a `.git`
/// folder, or a `.git` file as in a worktree or a submodule.
pub fn repo_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

fn canonical_root(raw: &str) -> Result<PathBuf, GitError> {
    repo_root(&canonical(raw)?).ok_or(GitError::NotARepo)
}

/// Runs git in `root` with `config` overrides and `args`, giving up after `GIT_TIMEOUT`.
async fn run(root: &Path, config: &[String], args: &[&str]) -> Result<Output, GitError> {
    let mut command = tokio::process::Command::new("git");
    command.arg("-C").arg(root);
    for entry in config {
        command.arg("-c").arg(entry);
    }
    command
        .args(args)
        // Status and diff would otherwise refresh the index, racing a session's own git.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    tokio::time::timeout(GIT_TIMEOUT, command.output())
        .await
        .map_err(|_| GitError::Failed("git timed out".to_string()))?
        .map_err(|err| GitError::Failed(format!("cannot run git: {err}")))
}

/// A repository's own config can name programs for git to run on `status` and
/// `diff`: an fsmonitor, filter drivers. Opening a folder in the explorer runs
/// those commands, so every call turns them off. Filters set in the user's
/// global or system config (Git LFS) are trusted and kept. Diffs also pass
/// `--no-textconv` and `--no-ext-diff`, and submodules, whose config is not
/// read here, are never entered.
async fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    let mut config = vec![
        "core.quotePath=false".to_string(),
        "core.fsmonitor=false".to_string(),
    ];
    config.extend(repo_filter_overrides(root).await?);
    let output = run(root, &config, args).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::Failed(stderr.trim().to_string()));
    }
    Ok(output.stdout)
}

/// Overrides emptying every filter driver key the repository's own config
/// sets, included files followed. Reading config runs nothing.
async fn repo_filter_overrides(root: &Path) -> Result<Vec<String>, GitError> {
    let listing = run(
        root,
        &[],
        &[
            "config",
            "--show-scope",
            "--name-only",
            "--get-regexp",
            r"^filter\.",
        ],
    )
    .await?;
    // Exit code 1: no key matched.
    if !listing.status.success() && listing.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&listing.stderr);
        return Err(GitError::Failed(stderr.trim().to_string()));
    }
    Ok(filter_overrides(&String::from_utf8_lossy(&listing.stdout)))
}

/// From `git config --show-scope --name-only` lines (`scope<TAB>key`), an empty
/// value for each key of local or worktree scope, `false` for `required`.
fn filter_overrides(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(scope, _)| matches!(*scope, "local" | "worktree"))
        .map(|(_, key)| {
            if key.ends_with(".required") {
                format!("{key}=false")
            } else {
                format!("{key}=")
            }
        })
        .collect()
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
    pub old: Option<u32>,
    pub new: Option<u32>,
}

#[derive(Debug, Serialize, Default)]
pub struct Hunk {
    pub lines: Vec<Line>,
}

#[derive(Debug, Serialize)]
pub struct FileDiff {
    /// Relative to the repository root, `/`-separated; the old path for a deletion.
    pub path: String,
    /// The path before a rename.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub added: u32,
    pub removed: u32,
    /// Why lines are not shown (binary, too large), when they are not.
    pub note: Option<String>,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    fn new(path: String, status: FileStatus) -> Self {
        Self {
            path,
            old_path: None,
            status,
            added: 0,
            removed: 0,
            note: None,
            hunks: Vec::new(),
        }
    }

    /// Drops the lines of a file too large to show, keeping its counts.
    fn cap(mut self) -> Self {
        let lines: usize = self.hunks.iter().map(|hunk| hunk.lines.len()).sum();
        if lines > FILE_LINES_MAX {
            self.hunks.clear();
            self.note = Some(format!(
                "{} changed lines, too many to show",
                self.added + self.removed
            ));
        }
        self
    }
}

#[derive(Debug, Serialize)]
pub struct RepoDiff {
    pub root: String,
    pub files: Vec<FileDiff>,
}

/// Undoes git's C-style quoting of a path with unusual characters.
fn unquote(raw: &str) -> String {
    let Some(inner) = raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return raw.to_string();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.bytes().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b'a') => bytes.push(0x07),
            Some(b'b') => bytes.push(0x08),
            Some(b'f') => bytes.push(0x0c),
            Some(b'v') => bytes.push(0x0b),
            Some(digit @ b'0'..=b'7') => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    match chars.peek() {
                        Some(next @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(next - b'0');
                            chars.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(u8::try_from(value).unwrap_or(b'?'));
            }
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The path of a `---` or `+++` line, without its `a/` or `b/` prefix; None for `/dev/null`.
fn side_path(raw: &str, prefix: &str) -> Option<String> {
    // Git ends the name with a tab when it holds a space.
    let raw = raw.trim_end_matches(['\r', '\t']);
    if raw == "/dev/null" {
        return None;
    }
    let path = unquote(raw);
    Some(path.strip_prefix(prefix).unwrap_or(&path).to_string())
}

/// The path of a `diff --git a/P b/P` line when both sides are the same path.
fn header_path(rest: &str) -> Option<String> {
    let rest = rest.trim_end_matches('\r');
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find("\" ")? + 1;
        return side_path(&rest[..=end], "a/");
    }
    let name_len = rest.len().checked_sub(5)? / 2;
    let old = rest.get(..name_len + 2)?.strip_prefix("a/")?;
    let new = rest.get(name_len + 2..)?.strip_prefix(" b/")?;
    (old == new).then(|| old.to_string())
}

/// `-12,5 +12,7 @@ ...` into the first old and new line numbers and counts.
fn hunk_header(rest: &str) -> Option<(u32, u32, u32, u32)> {
    let mut parts = rest.split_whitespace();
    let range = |part: &str| -> Option<(u32, u32)> {
        let (start, count) = part.split_once(',').unwrap_or((part, "1"));
        Some((start.parse().ok()?, count.parse().ok()?))
    };
    let (old_start, old_count) = range(parts.next()?.strip_prefix('-')?)?;
    let (new_start, new_count) = range(parts.next()?.strip_prefix('+')?)?;
    Some((old_start, old_count, new_start, new_count))
}

/// Parses `git diff` output into one entry per file.
pub fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut current: Option<FileDiff> = None;
    let (mut old_left, mut new_left, mut old_line, mut new_line) = (0u32, 0u32, 0u32, 0u32);

    for line in text.split('\n') {
        if (old_left > 0 || new_left > 0)
            && let Some(file) = current.as_mut()
            && let Some(hunk) = file.hunks.last_mut()
        {
            let content = || line[1..].trim_end_matches('\r').to_string();
            match line.as_bytes().first() {
                Some(b' ') => {
                    hunk.lines.push(Line {
                        kind: LineKind::Context,
                        text: content(),
                        old: Some(old_line),
                        new: Some(new_line),
                    });
                    old_line += 1;
                    new_line += 1;
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                    continue;
                }
                Some(b'-') => {
                    hunk.lines.push(Line {
                        kind: LineKind::Removed,
                        text: content(),
                        old: Some(old_line),
                        new: None,
                    });
                    file.removed += 1;
                    old_line += 1;
                    old_left = old_left.saturating_sub(1);
                    continue;
                }
                Some(b'+') => {
                    hunk.lines.push(Line {
                        kind: LineKind::Added,
                        text: content(),
                        old: None,
                        new: Some(new_line),
                    });
                    file.added += 1;
                    new_line += 1;
                    new_left = new_left.saturating_sub(1);
                    continue;
                }
                Some(b'\\') => continue,
                _ => {
                    old_left = 0;
                    new_left = 0;
                }
            }
        }

        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(current.take().map(FileDiff::cap));
            current = Some(FileDiff::new(
                header_path(rest).unwrap_or_default(),
                FileStatus::Modified,
            ));
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };
        if line.starts_with("new file mode") {
            file.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
        } else if let Some(path) = line.strip_prefix("rename from ") {
            file.old_path = Some(unquote(path.trim_end_matches('\r')));
            file.status = FileStatus::Renamed;
        } else if let Some(path) = line.strip_prefix("rename to ") {
            file.path = unquote(path.trim_end_matches('\r'));
        } else if line.starts_with("Binary files ") {
            file.note = Some("Binary file".to_string());
        } else if let Some(path) = line.strip_prefix("--- ") {
            if let Some(path) = side_path(path, "a/")
                && file.path.is_empty()
            {
                file.path = path;
            }
        } else if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(path) = side_path(path, "b/") {
                file.path = path;
            }
        } else if let Some(rest) = line.strip_prefix("@@ ")
            && let Some((old_start, old_count, new_start, new_count)) = hunk_header(rest)
        {
            file.hunks.push(Hunk::default());
            (old_line, old_left, new_line, new_left) = (old_start, old_count, new_start, new_count);
        }
    }
    files.extend(current.map(FileDiff::cap));
    files
}

/// An untracked file as an addition of all its lines; `read` false lists it only.
fn untracked_file(root: &Path, path: String, read: bool) -> FileDiff {
    let mut file = FileDiff::new(path, FileStatus::Added);
    if !read {
        file.note = Some("Too many new files to show them all".to_string());
        return file;
    }
    let full = root.join(&file.path);
    match std::fs::metadata(&full) {
        Ok(meta) if meta.len() > UNTRACKED_BYTES_MAX => {
            file.note = Some("Too large to show".to_string());
            return file;
        }
        Ok(_) => {}
        Err(err) => {
            file.note = Some(format!("Cannot read: {err}"));
            return file;
        }
    }
    let bytes = match std::fs::read(&full) {
        Ok(bytes) => bytes,
        Err(err) => {
            file.note = Some(format!("Cannot read: {err}"));
            return file;
        }
    };
    if is_binary(&bytes) {
        file.note = Some("Binary file".to_string());
        return file;
    }
    let text = String::from_utf8_lossy(&bytes);
    let body = text.strip_suffix('\n').unwrap_or(&text);
    if body.is_empty() {
        return file;
    }
    let lines: Vec<Line> = body
        .split('\n')
        .zip(1u32..)
        .map(|(line, number)| Line {
            kind: LineKind::Added,
            text: line.trim_end_matches('\r').to_string(),
            old: None,
            new: Some(number),
        })
        .collect();
    file.added = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    file.hunks.push(Hunk { lines });
    file.cap()
}

/// What the working tree is compared to: HEAD, or the empty tree before the first commit.
async fn base(root: &Path) -> &'static str {
    let has_head = git(root, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .await
        .is_ok();
    if has_head { "HEAD" } else { EMPTY_TREE }
}

/// `git diff` against `base`, limited to `paths` when given.
async fn patch(root: &Path, paths: &[&str]) -> Result<Vec<u8>, GitError> {
    let mut args = vec![
        "diff",
        base(root).await,
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=dirty",
        "--find-renames",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "-U3",
    ];
    if !paths.is_empty() {
        args.push("--");
        args.extend_from_slice(paths);
    }
    git(root, &args).await
}

/// HEAD against the working tree, untracked files included, sorted by path.
pub async fn repo_diff(root: &Path) -> Result<RepoDiff, GitError> {
    let patch = patch(root, &[]).await?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).await?;

    let root_owned = root.to_path_buf();
    let mut files = blocking(move || {
        let mut files = parse_diff(&String::from_utf8_lossy(&patch));
        let paths = untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned());
        for (index, path) in paths.enumerate() {
            files.push(untracked_file(
                &root_owned,
                path,
                index < UNTRACKED_READ_MAX,
            ));
        }
        Ok::<_, GitError>(files)
    })
    .await?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(RepoDiff {
        root: display_path(root),
        files,
    })
}

/// `path` relative to `root`, `/`-separated as git writes it.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let parts: Vec<String> = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// One file's pending changes, or None when it has none.
pub async fn file_diff(path: &Path) -> Result<Option<FileDiff>, GitError> {
    let parent = path.parent().map(Path::to_path_buf);
    let Some(root) =
        blocking(move || Ok::<_, GitError>(parent.as_deref().and_then(repo_root))).await?
    else {
        return Ok(None);
    };
    let Some(rel) = relative(&root, path) else {
        return Ok(None);
    };
    let untracked = git(
        &root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            &rel,
        ],
    )
    .await?;
    if !untracked.is_empty() {
        return blocking(move || Ok(Some(untracked_file(&root, rel, true)))).await;
    }
    let patch = patch(&root, &[&rel]).await?;
    Ok(parse_diff(&String::from_utf8_lossy(&patch))
        .into_iter()
        .next())
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Change {
    /// Relative to the repository root, `/`-separated.
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Serialize)]
pub struct RepoStatus {
    /// Null outside a repository, with no changes.
    pub root: Option<String>,
    pub changes: Vec<Change>,
}

/// Parses `git status --porcelain=v1 -z`: `XY path`, a rename followed by its old path.
pub fn parse_status(raw: &[u8]) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut records = raw
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        let [index, tree, b' ', path @ ..] = record else {
            continue;
        };
        let path = String::from_utf8_lossy(path).into_owned();
        let kind = match (index, tree) {
            (b'?', _) | (b'A', _) if *tree != b'D' => ChangeKind::Added,
            (b'D', _) | (_, b'D') => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        changes.push(Change { path, kind });
        if matches!(index, b'R' | b'C') {
            let old = records.next();
            if *index == b'R'
                && let Some(old) = old
            {
                changes.push(Change {
                    path: String::from_utf8_lossy(old).into_owned(),
                    kind: ChangeKind::Deleted,
                });
            }
        }
    }
    changes
}

pub async fn repo_status(path: &Path) -> Result<RepoStatus, GitError> {
    let lookup = path.to_path_buf();
    let Some(root) = blocking(move || Ok::<_, GitError>(repo_root(&lookup))).await? else {
        return Ok(RepoStatus {
            root: None,
            changes: Vec::new(),
        });
    };
    let raw = git(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=dirty",
        ],
    )
    .await?;
    Ok(RepoStatus {
        root: Some(display_path(&root)),
        changes: parse_status(&raw),
    })
}

/// Changed paths of the repository containing a folder, for its listing.
pub async fn status_route(Query(query): Query<PathQuery>) -> Result<Json<RepoStatus>, GitError> {
    let path = blocking(move || Ok::<_, GitError>(canonical(&query.path)?)).await?;
    repo_status(&path).await.map(Json)
}

/// A file's pending changes, null when it has none or is outside a repository.
pub async fn file_route(Query(query): Query<PathQuery>) -> Result<Response, GitError> {
    let (path, root) = blocking(move || {
        let path = canonical(&query.path)?;
        let root = path.parent().and_then(repo_root);
        Ok::<_, GitError>((path, root))
    })
    .await?;
    let diff = file_diff(&path).await?;
    Ok(Json(json!({
        "root": root.as_deref().map(display_path),
        "diff": diff,
    }))
    .into_response())
}

/// The repository containing a folder, or null.
pub async fn root_route(Query(query): Query<PathQuery>) -> Response {
    let root = blocking(move || {
        Ok::<_, GitError>(
            canonical(&query.path)
                .ok()
                .and_then(|path| repo_root(&path)),
        )
    })
    .await
    .ok()
    .flatten();
    Json(json!({ "root": root.as_deref().map(display_path) })).into_response()
}

#[derive(Deserialize)]
pub struct RootQuery {
    pub root: String,
}

pub async fn diff_route(Query(query): Query<RootQuery>) -> Result<Json<RepoDiff>, GitError> {
    let root = blocking(move || canonical_root(&query.root)).await?;
    repo_diff(&root).await.map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,4 @@
 fn main() {
-    old();
+    new();
+    more();
 }
--- stays
@@ -10 +11 @@ fn tail
-a
+b
\\ No newline at end of file
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3333333..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-x
-y
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
diff --git a/img.png b/img.png
new file mode 100644
index 0000000..4444444
Binary files /dev/null and b/img.png differ
";

    #[test]
    fn parses_modified_deleted_renamed_and_binary_files() {
        let files = parse_diff(PATCH);
        let summary: Vec<(&str, FileStatus, u32, u32)> = files
            .iter()
            .map(|file| (file.path.as_str(), file.status, file.added, file.removed))
            .collect();
        assert_eq!(
            summary,
            [
                ("src/main.rs", FileStatus::Modified, 3, 3),
                ("gone.txt", FileStatus::Deleted, 0, 2),
                ("new name.txt", FileStatus::Renamed, 0, 0),
                ("img.png", FileStatus::Added, 0, 0),
            ]
        );
        assert_eq!(files[2].old_path.as_deref(), Some("old name.txt"));
        assert_eq!(files[3].note.as_deref(), Some("Binary file"));
    }

    #[test]
    fn numbers_lines_and_keeps_content_that_looks_like_a_header() {
        let files = parse_diff(PATCH);
        let main = &files[0];
        assert_eq!(main.hunks.len(), 2);
        let first: Vec<(LineKind, Option<u32>, Option<u32>)> = main.hunks[0]
            .lines
            .iter()
            .map(|line| (line.kind, line.old, line.new))
            .collect();
        assert_eq!(
            first,
            [
                (LineKind::Context, Some(1), Some(1)),
                (LineKind::Removed, Some(2), None),
                (LineKind::Added, None, Some(2)),
                (LineKind::Added, None, Some(3)),
                (LineKind::Context, Some(3), Some(4)),
                (LineKind::Removed, Some(4), None),
            ]
        );
        assert_eq!(main.hunks[0].lines[5].text, "-- stays");
        assert_eq!(main.hunks[1].lines[0].old, Some(10));
        assert_eq!(main.hunks[1].lines[1].new, Some(11));
    }

    #[test]
    fn unquotes_c_style_paths() {
        assert_eq!(unquote(r#""a\tb\\c\"d""#), "a\tb\\c\"d");
        assert_eq!(unquote(r#""caf\303\251""#), "café");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(side_path("\"b/sp ace\"\t", "b/").as_deref(), Some("sp ace"));
        assert_eq!(header_path("a/x y b/x y").as_deref(), Some("x y"));
    }

    #[test]
    fn parses_status_records_and_renames() {
        let raw =
            b" M src/a.rs\0?? new.md\0A  staged.rs\0 D gone.rs\0AD brief.rs\0R  to.rs\0from.rs\0";
        let parsed = parse_status(raw);
        let changes: Vec<(&str, ChangeKind)> = parsed
            .iter()
            .map(|change| (change.path.as_str(), change.kind))
            .collect();
        assert_eq!(
            changes,
            [
                ("src/a.rs", ChangeKind::Modified),
                ("new.md", ChangeKind::Added),
                ("staged.rs", ChangeKind::Added),
                ("gone.rs", ChangeKind::Deleted),
                ("brief.rs", ChangeKind::Deleted),
                ("to.rs", ChangeKind::Modified),
                ("from.rs", ChangeKind::Deleted),
            ]
        );
    }

    fn run(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn only_the_repository_s_own_filter_keys_are_emptied() {
        let listing = "system\tfilter.lfs.clean\nglobal\tfilter.lfs.process\n\
            local\tfilter.x.clean\nlocal\tfilter.x.required\nworktree\tfilter.my.drv.smudge\n";
        assert_eq!(
            filter_overrides(listing),
            [
                "filter.x.clean=",
                "filter.x.required=false",
                "filter.my.drv.smudge="
            ]
        );
    }

    /// A folder carrying its own `.git/config` must not run anything when the
    /// explorer reads its status or diff.
    #[tokio::test]
    async fn programs_named_by_a_repository_s_own_config_never_run() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = std::fs::canonicalize(tmp.path()).expect("canonical");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).expect("mkdir");
        run(&repo, &["init", "-q"]);
        std::fs::write(repo.join(".gitattributes"), "*.txt filter=x diff=x\n").expect("write");
        std::fs::write(repo.join("a.txt"), "one\n").expect("write");
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-q", "-m", "init"]);

        let marker = |name: &str| dir.join(name).display().to_string().replace('\\', "/");
        let touch = |name: &str| format!("sh -c 'touch {}; cat'", marker(name));
        for (key, value) in [
            ("core.fsmonitor", touch("fsmonitor")),
            ("filter.x.clean", touch("clean")),
            ("filter.x.process", touch("process")),
            ("filter.x.required", "true".to_string()),
            ("diff.x.textconv", touch("textconv")),
            ("diff.x.command", touch("command")),
        ] {
            run(&repo, &["config", key, &value]);
        }
        std::fs::write(repo.join("a.txt"), "two\n").expect("write");

        let status = repo_status(&repo).await.expect("status");
        assert_eq!(status.changes.len(), 1);
        let diff = repo_diff(&repo).await.expect("diff");
        assert_eq!(diff.files.len(), 1);
        assert!(
            file_diff(&repo.join("a.txt"))
                .await
                .expect("file diff")
                .is_some()
        );

        let ran: Vec<&str> = ["fsmonitor", "clean", "process", "textconv", "command"]
            .into_iter()
            .filter(|name| dir.join(name).exists())
            .collect();
        assert!(ran.is_empty(), "repository programs ran: {ran:?}");
    }

    #[tokio::test]
    async fn repo_diff_merges_tracked_changes_and_untracked_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = std::fs::canonicalize(tmp.path()).expect("canonical");
        run(&dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").expect("write");
        run(&dir, &["add", "a.txt"]);
        run(&dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("a.txt"), "one\nTWO\n").expect("write");
        std::fs::create_dir(dir.join("sub")).expect("mkdir");
        std::fs::write(dir.join("sub").join("new.md"), "# hi\n").expect("write");

        assert_eq!(repo_root(&dir.join("sub")).as_deref(), Some(dir.as_path()));
        let diff = repo_diff(&dir).await.expect("diff");
        let summary: Vec<(&str, FileStatus, u32, u32)> = diff
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.status, file.added, file.removed))
            .collect();
        assert_eq!(
            summary,
            [
                ("a.txt", FileStatus::Modified, 1, 1),
                ("sub/new.md", FileStatus::Added, 1, 0),
            ]
        );

        let one = file_diff(&dir.join("a.txt"))
            .await
            .expect("file diff")
            .expect("a.txt changed");
        assert_eq!((one.added, one.removed), (1, 1));
        let new = file_diff(&dir.join("sub").join("new.md"))
            .await
            .expect("file diff")
            .expect("new.md is new");
        assert_eq!(new.status, FileStatus::Added);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").expect("write");
        assert!(
            file_diff(&dir.join("a.txt"))
                .await
                .expect("file diff")
                .is_none()
        );

        let status = repo_status(&dir.join("sub")).await.expect("status");
        assert_eq!(
            status.changes,
            [Change {
                path: "sub/new.md".to_string(),
                kind: ChangeKind::Added
            }]
        );
    }
}
