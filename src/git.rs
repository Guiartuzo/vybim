//! Git backend for the diff view.
//!
//! All git access goes through this module. It shells out to the `git` CLI for
//! the two things the diff view needs — the list of changed files and the
//! committed (`HEAD`) contents of a file — so there is no C dependency to link.
//! Swapping to a library backend later is an implementation change behind this
//! seam, invisible to the view.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How a file differs from `HEAD` in the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
}

impl ChangeKind {
    /// A one-character indicator for the changed-files list.
    pub fn indicator(self) -> char {
        match self {
            ChangeKind::Modified => 'M',
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
        }
    }
}

/// One entry in the changed-files list: a repo-relative path and its change
/// kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub kind: ChangeKind,
}

/// The committed (`HEAD`) contents of a file: either UTF-8 text, or a flag that
/// the blob is binary / non-UTF-8 and must not be line-diffed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContents {
    Text(String),
    Binary,
}

/// The workspace root if the working directory is inside a git repository,
/// `None` otherwise (git missing from `PATH` also yields `None`).
pub fn repo_root() -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Every file in the working tree that differs from `HEAD` (modified, added, or
/// deleted), excluding unchanged files. Returns an empty list when the tree is
/// clean. `git status --porcelain=v1 -z` gives NUL-separated, script-stable
/// output so filenames with spaces or newlines parse unambiguously.
pub fn changed_files() -> Vec<ChangedFile> {
    let Some(out) = Command::new("git")
        .args(["status", "--porcelain=v1", "-z"])
        .output()
        .ok()
        .filter(|o| o.status.success())
    else {
        return Vec::new();
    };
    parse_porcelain(&out.stdout)
}

/// The committed contents of `path` at `HEAD`. Added files (absent from `HEAD`)
/// yield empty text; non-UTF-8 blobs yield [`FileContents::Binary`].
pub fn file_at_head(path: &str) -> FileContents {
    let spec = format!("HEAD:{path}");
    let Some(out) = Command::new("git").args(["show", &spec]).output().ok() else {
        return FileContents::Text(String::new());
    };
    if !out.status.success() {
        // Absent in HEAD (e.g. an added file) — treat as empty.
        return FileContents::Text(String::new());
    }
    decode(out.stdout)
}

/// The working-tree contents of `path`, read straight from disk relative to
/// `root`. A missing file (e.g. deleted) yields empty text; non-UTF-8 yields
/// [`FileContents::Binary`].
pub fn file_on_disk(root: &Path, path: &str) -> FileContents {
    match std::fs::read(root.join(path)) {
        Ok(bytes) => decode(bytes),
        Err(_) => FileContents::Text(String::new()),
    }
}

/// Decode raw bytes as UTF-8 text, flagging anything else as binary. A NUL byte
/// is a strong binary signal even within otherwise-decodable bytes.
fn decode(bytes: Vec<u8>) -> FileContents {
    if bytes.contains(&0) {
        return FileContents::Binary;
    }
    match String::from_utf8(bytes) {
        Ok(text) => FileContents::Text(text),
        Err(_) => FileContents::Binary,
    }
}

/// Parse `git status --porcelain=v1 -z` output into changed-file entries.
///
/// Each record is `XY<space>PATH`, NUL-terminated. For renames/copies the
/// record is followed by a second NUL-separated field (the original path),
/// which we consume but ignore — the new path is what differs from `HEAD`.
fn parse_porcelain(bytes: &[u8]) -> Vec<ChangedFile> {
    let text = String::from_utf8_lossy(bytes);
    let mut fields = text.split('\0');
    let mut files = Vec::new();

    while let Some(entry) = fields.next() {
        if entry.len() < 3 {
            continue;
        }
        let status = &entry[..2];
        let path = entry[3..].to_string();
        let bytes = status.as_bytes();
        let (x, y) = (bytes[0] as char, bytes[1] as char);

        // Renames/copies carry an extra NUL-separated original-path field.
        if x == 'R' || x == 'C' {
            fields.next();
            files.push(ChangedFile {
                path,
                kind: ChangeKind::Added,
            });
            continue;
        }

        // Prefer the more specific status across the index (X) and worktree (Y)
        // columns; '?' is an untracked (new) file.
        let kind = if x == 'D' || y == 'D' {
            ChangeKind::Deleted
        } else if x == 'A' || x == '?' {
            ChangeKind::Added
        } else {
            ChangeKind::Modified
        };
        files.push(ChangedFile { path, kind });
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a NUL-separated porcelain payload from `XY PATH` lines.
    fn porcelain(records: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        for r in records {
            v.extend_from_slice(r.as_bytes());
            v.push(0);
        }
        v
    }

    #[test]
    fn parses_each_change_kind() {
        let raw = porcelain(&[" M src/a.rs", "A  src/new.rs", " D src/gone.rs"]);
        let files = parse_porcelain(&raw);
        assert_eq!(
            files,
            vec![
                ChangedFile {
                    path: "src/a.rs".into(),
                    kind: ChangeKind::Modified
                },
                ChangedFile {
                    path: "src/new.rs".into(),
                    kind: ChangeKind::Added
                },
                ChangedFile {
                    path: "src/gone.rs".into(),
                    kind: ChangeKind::Deleted
                },
            ]
        );
    }

    #[test]
    fn untracked_files_count_as_added() {
        let files = parse_porcelain(&porcelain(&["?? notes.txt"]));
        assert_eq!(
            files,
            vec![ChangedFile {
                path: "notes.txt".into(),
                kind: ChangeKind::Added
            }]
        );
    }

    #[test]
    fn preserves_spaces_in_names() {
        let files = parse_porcelain(&porcelain(&[" M my file.rs"]));
        assert_eq!(files[0].path, "my file.rs");
    }

    #[test]
    fn rename_consumes_origin_field_and_keeps_new_path() {
        // A rename record is followed by its original path as a separate field.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"R  new name.rs");
        raw.push(0);
        raw.extend_from_slice(b"old name.rs");
        raw.push(0);
        raw.extend_from_slice(b" M after.rs");
        raw.push(0);
        let files = parse_porcelain(&raw);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "new name.rs");
        assert_eq!(files[1].path, "after.rs");
    }

    #[test]
    fn clean_tree_yields_no_files() {
        assert!(parse_porcelain(b"").is_empty());
    }

    #[test]
    fn decode_flags_non_utf8_and_nul_as_binary() {
        assert_eq!(decode(vec![0xff, 0xfe]), FileContents::Binary);
        assert_eq!(decode(vec![b'a', 0, b'b']), FileContents::Binary);
        assert_eq!(decode(b"hi".to_vec()), FileContents::Text("hi".into()));
    }

    /// End-to-end against a throwaway repo: the CLI calls go through `git` with
    /// the working directory inside the repo, so this runs serially and restores
    /// the previous directory. Skipped silently if `git` is unavailable.
    #[test]
    fn end_to_end_against_a_temp_repo() {
        let dir = std::env::temp_dir().join(format!("vybim_git_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !run(&["init", "-q"]) {
            return; // git not present — nothing to verify
        }
        run(&["config", "user.email", "t@t.t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("keep.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "init"]);
        // Modify, add, and delete relative to HEAD.
        std::fs::write(dir.join("keep.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.join("new file.txt"), "brand new\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();

        let guard = CwdGuard::enter(&dir);

        let root = repo_root().expect("inside a repo");
        assert!(root.exists());

        let mut files = changed_files();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            files,
            vec![
                ChangedFile {
                    path: "gone.txt".into(),
                    kind: ChangeKind::Deleted
                },
                ChangedFile {
                    path: "keep.txt".into(),
                    kind: ChangeKind::Modified
                },
                ChangedFile {
                    path: "new file.txt".into(),
                    kind: ChangeKind::Added
                },
            ]
        );

        // Committed contents for a modified file; empty for an added one.
        assert_eq!(
            file_at_head("keep.txt"),
            FileContents::Text("one\ntwo\nthree\n".into())
        );
        assert_eq!(
            file_at_head("new file.txt"),
            FileContents::Text(String::new())
        );

        drop(guard);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Restores the process working directory when dropped, so the cwd change is
    /// scoped to the test even if an assertion panics.
    struct CwdGuard(std::path::PathBuf);
    impl CwdGuard {
        fn enter(to: &Path) -> Self {
            let prev = std::env::current_dir().unwrap();
            std::env::set_current_dir(to).unwrap();
            CwdGuard(prev)
        }
    }
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
}
