//! The fuzzy file-finder's data layer: snapshot the workspace's files once, then
//! rank that snapshot against a typed query in memory.
//!
//! The match is a *subsequence* test (the query's characters must appear in
//! order anywhere in the path), not a substring — so `mn` finds `main.rs`.
//! Matches are scored so that consecutive runs, filename hits, and word/path
//! boundary starts rank higher, the same instinct VSCode's `Ctrl+P` follows.
//! Case folding is ASCII-only, matching the rest of Vybim's matching code.

use std::path::{Path, PathBuf};

use walkdir::{DirEntry, WalkDir};

/// Upper bound on files gathered into a snapshot, so the finder stays bounded on
/// huge trees. Beyond this the walk simply stops.
pub const MAX_FILES: usize = 10_000;

/// A score bonus for a query char that lands on a word/path boundary (start of
/// the string, or just after a separator like `/`, `_`, `-`, `.`, or space).
const BOUNDARY_BONUS: i64 = 10;
/// A bonus for a query char that immediately follows the previous match (a
/// consecutive run, e.g. typing the literal start of a name).
const CONSEC_BONUS: i64 = 5;
/// A bonus for a query char that falls within the file name segment (after the
/// last separator), so filename hits beat directory hits.
const FILENAME_BONUS: i64 = 3;
/// Penalty per skipped character between two matches. Set high enough that a
/// tight consecutive run beats a gappy match even when the gappy one collects
/// several boundary bonuses.
const GAP_PENALTY: i64 = 3;

/// One file in the snapshot: its absolute path (used to open it) and the
/// root-relative display string (what the user reads and we score against).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileItem {
    pub path: PathBuf,
    pub display: String,
}

/// Recursively walk `root`, collecting regular files (skipping `.git` and any
/// other dot-prefixed / hidden directory or file), with the root-relative path
/// as the display string. Bounded to [`MAX_FILES`].
pub fn gather_files(root: impl AsRef<Path>) -> Vec<FileItem> {
    let root = root.as_ref();
    let mut out = Vec::new();
    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_hidden(e));
    for entry in walker.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        if entry.file_type().is_file() {
            let path = entry.path().to_path_buf();
            let display = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            out.push(FileItem { path, display });
        }
    }
    out
}

/// Whether `entry` is a hidden (dot-prefixed) file or directory below the root.
/// Filtering a directory here prunes its whole subtree, so `.git` and friends
/// never get walked. The root itself (depth 0) is never treated as hidden.
fn is_hidden(entry: &DirEntry) -> bool {
    entry.depth() > 0
        && entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.starts_with('.'))
}

/// Score `candidate` against `query` as a case-insensitive subsequence, or
/// `None` when `query` is not a subsequence of `candidate`. Higher is a tighter
/// match. An empty query scores zero (so everything "matches", in snapshot
/// order — see [`rank`]). Matching is greedy-leftmost, which always finds a
/// subsequence when one exists.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() {
        return Some(0);
    }
    let c: Vec<char> = candidate.chars().collect();
    // Start of the file name segment (just after the last separator).
    let name_start = c
        .iter()
        .rposition(|&ch| ch == '/' || ch == '\\')
        .map(|i| i + 1)
        .unwrap_or(0);

    let mut score = 0i64;
    let mut qi = 0usize;
    let mut prev: Option<usize> = None;
    for (ci, &ch) in c.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if ch.eq_ignore_ascii_case(&q[qi]) {
            score += 1;
            let at_boundary = ci == 0 || matches!(c[ci - 1], '/' | '\\' | '_' | '-' | '.' | ' ');
            if at_boundary {
                score += BOUNDARY_BONUS;
            }
            if ci >= name_start {
                score += FILENAME_BONUS;
            }
            match prev {
                Some(p) if p + 1 == ci => score += CONSEC_BONUS,
                Some(p) => score -= GAP_PENALTY * (ci - p - 1) as i64,
                None => {}
            }
            prev = Some(ci);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score)
}

/// Indices into `items` of the entries matching `query`, ordered by score
/// (descending), ties broken by shorter `display` then lexically. An empty
/// query yields every index in snapshot order.
pub fn rank(items: &[FileItem], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    let mut scored: Vec<(usize, i64)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| fuzzy_score(query, &it.display).map(|s| (i, s)))
        .collect();
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| items[a.0].display.len().cmp(&items[b.0].display.len()))
            .then_with(|| items[a.0].display.cmp(&items[b.0].display))
    });
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(display: &str) -> FileItem {
        FileItem {
            path: PathBuf::from(display),
            display: display.to_string(),
        }
    }

    #[test]
    fn subsequence_matches_in_order() {
        assert!(fuzzy_score("mn", "main.rs").is_some());
        assert!(fuzzy_score("main", "src/main.rs").is_some());
        // out of order is not a subsequence
        assert!(fuzzy_score("nm", "main.rs").is_none());
        // a char missing entirely
        assert!(fuzzy_score("xyz", "main.rs").is_none());
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(fuzzy_score("MAIN", "src/main.rs").is_some());
        assert!(fuzzy_score("main", "src/MAIN.rs").is_some());
    }

    #[test]
    fn tighter_match_scores_higher_than_looser() {
        // consecutive run at a boundary beats a scattered, gappy match
        let tight = fuzzy_score("main", "main.rs").unwrap();
        let loose = fuzzy_score("main", "m_a_i_long_n.rs").unwrap();
        assert!(tight > loose, "tight {tight} should beat loose {loose}");
    }

    #[test]
    fn filename_hit_beats_directory_hit() {
        let items = vec![item("main/util.rs"), item("src/main.rs")];
        // querying "main": the filename hit (src/main.rs) should rank first
        let order = rank(&items, "main");
        assert_eq!(order[0], 1);
    }

    #[test]
    fn empty_query_returns_all_in_snapshot_order() {
        let items = vec![item("b.rs"), item("a.rs"), item("c.rs")];
        assert_eq!(rank(&items, ""), vec![0, 1, 2]);
    }

    #[test]
    fn rank_drops_non_matches_and_orders_by_score() {
        let items = vec![item("README.md"), item("src/main.rs"), item("Cargo.toml")];
        let order = rank(&items, "main");
        // only main.rs matches "main"
        assert_eq!(order, vec![1]);
    }

    #[test]
    fn rank_breaks_ties_by_shorter_then_lexical() {
        // identical match shape; shorter display wins, then lexical order
        let items = vec![item("ab/x.rs"), item("x.rs"), item("ax.rs")];
        let order = rank(&items, "x");
        // "x.rs" (len 4) and "ax.rs" (len 5) and "ab/x.rs" (len 7)
        assert_eq!(items[order[0]].display, "x.rs");
    }

    #[test]
    fn gather_files_skips_hidden_dirs() {
        let root = std::env::temp_dir().join(format!("vybim_ff_{}", std::process::id()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join(".git/config"), "x").unwrap();
        std::fs::write(root.join("Cargo.toml"), "y").unwrap();

        let files = gather_files(&root);
        let displays: Vec<&str> = files.iter().map(|f| f.display.as_str()).collect();
        assert!(displays.contains(&"src/main.rs"));
        assert!(displays.contains(&"Cargo.toml"));
        // nothing from the hidden .git directory
        assert!(!displays.iter().any(|d| d.contains(".git")));
        std::fs::remove_dir_all(&root).ok();
    }
}
