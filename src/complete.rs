//! Buffer-word autocomplete (vim `Ctrl+N` style): suggest identifier-like words
//! already present in the focused buffer that share the prefix at the cursor.
//!
//! No language server and no index — the candidate list is gathered by scanning
//! the buffer's lines on demand, the same instinct `find_matches` uses for
//! incremental search. Candidates come only from the focused buffer (sidestepping
//! the `buffer_id`-is-a-raw-index landmine of completing across all buffers) and
//! are ranked by proximity to the cursor, then frequency.

use std::collections::HashMap;

use crate::buffer::Buffer;

/// Upper bound on candidates gathered and rows the popup shows. Keeps the scan
/// result and the overlay both bounded on large buffers / long lists.
pub const MAX_CANDIDATES: usize = 10;

/// An open completion popup: where the word being completed began, the prefix
/// typed so far, the ranked candidate words, and which one is selected.
#[derive(Debug)]
pub struct Completion {
    /// `(line, col)` where the word being completed starts. Accept replaces the
    /// span `[prefix_start, cursor]` with the chosen word.
    pub prefix_start: (usize, usize),
    /// The identifier prefix the candidates were filtered against.
    pub query: String,
    /// Ranked candidate words (proximity, then frequency), bounded.
    pub candidates: Vec<String>,
    /// Index into `candidates` of the highlighted entry.
    pub selected: usize,
}

impl Completion {
    /// Build a popup for the word immediately before `cursor` in `buffer`, or
    /// `None` when there is no identifier prefix there or nothing else in the
    /// buffer matches it (in which case no popup should open).
    pub fn open(buffer: &Buffer, cursor: (usize, usize)) -> Option<Self> {
        let (query, prefix_start) = prefix_at(buffer, cursor)?;
        let candidates = candidates(buffer, &query, prefix_start, cursor.0);
        if candidates.is_empty() {
            return None;
        }
        Some(Self {
            prefix_start,
            query,
            candidates,
            selected: 0,
        })
    }

    /// Recompute the candidate list after a buffer edit, returning whether the
    /// popup should stay open. Closes (returns `false`) when the cursor has left
    /// the word — moved before `prefix_start`, onto another line, or onto a
    /// non-word boundary — the prefix empties, or nothing matches.
    pub fn requery(&mut self, buffer: &Buffer, cursor: (usize, usize)) -> bool {
        if cursor.0 != self.prefix_start.0 || cursor.1 < self.prefix_start.1 {
            return false;
        }
        let Some((query, prefix_start)) = prefix_at(buffer, cursor) else {
            return false;
        };
        if prefix_start != self.prefix_start {
            return false;
        }
        let candidates = candidates(buffer, &query, prefix_start, cursor.0);
        if candidates.is_empty() {
            return false;
        }
        self.query = query;
        self.candidates = candidates;
        self.selected = 0;
        true
    }

    /// Move the selection to the previous entry, wrapping to the last.
    pub fn move_up(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        self.selected = (self.selected + self.candidates.len() - 1) % self.candidates.len();
    }

    /// Move the selection to the next entry, wrapping to the first.
    pub fn move_down(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.candidates.len();
    }

    /// The currently selected candidate word.
    pub fn selected_word(&self) -> Option<&str> {
        self.candidates.get(self.selected).map(String::as_str)
    }
}

/// Is `ch` part of an identifier token (`[A-Za-z0-9_]`)? Shared with cursor
/// word-movement so "what counts as a word" stays consistent.
pub(crate) fn is_word_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

/// May `ch` start an identifier token (`[A-Za-z_]`)? Digits cannot, so a run
/// like `123abc` is not treated as an identifier.
fn is_word_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

/// The identifier prefix immediately before `cursor` and the `(line, col)` where
/// it starts, or `None` when there is no word char before the cursor or the run
/// is not a valid identifier (it begins with a digit). `col` counts characters.
fn prefix_at(buffer: &Buffer, cursor: (usize, usize)) -> Option<(String, (usize, usize))> {
    let (line, col) = cursor;
    let chars: Vec<char> = buffer.line_text(line).chars().collect();
    let col = col.min(chars.len());
    let mut start = col;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    if start == col || !is_word_start(chars[start]) {
        return None;
    }
    let prefix: String = chars[start..col].iter().collect();
    Some((prefix, (line, start)))
}

/// Distinct identifier words in `buffer` that start with `prefix`, excluding the
/// in-progress token at `exclude` and the prefix itself, ranked by nearest line
/// to `cursor_line` then frequency, and bounded to [`MAX_CANDIDATES`].
fn candidates(
    buffer: &Buffer,
    prefix: &str,
    exclude: (usize, usize),
    cursor_line: usize,
) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    // word -> (nearest line distance to the cursor, occurrence count).
    let mut stats: HashMap<String, (usize, usize)> = HashMap::new();
    for line in 0..buffer.line_count() {
        let chars: Vec<char> = buffer.line_text(line).chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if !is_word_char(chars[i]) {
                i += 1;
                continue;
            }
            let start = i;
            let mut j = i + 1;
            while j < chars.len() && is_word_char(chars[j]) {
                j += 1;
            }
            // Only identifier-shaped runs (not the in-progress token) count.
            if is_word_start(chars[start]) && (line, start) != exclude {
                let word: String = chars[start..j].iter().collect();
                if word != prefix && word.starts_with(prefix) {
                    let dist = line.abs_diff(cursor_line);
                    let entry = stats.entry(word).or_insert((usize::MAX, 0));
                    entry.0 = entry.0.min(dist);
                    entry.1 += 1;
                }
            }
            i = j;
        }
    }

    let mut ranked: Vec<(String, usize, usize)> = stats
        .into_iter()
        .map(|(word, (dist, count))| (word, dist, count))
        .collect();
    // Nearest first; ties broken by higher frequency, then alphabetically so the
    // order is deterministic.
    ranked.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then(b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(word, _, _)| word)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_at_reads_the_word_before_the_cursor() {
        let b = Buffer::from_str("foo bar");
        // cursor just after "ba"
        assert_eq!(prefix_at(&b, (0, 6)), Some(("ba".to_string(), (0, 4))));
    }

    #[test]
    fn prefix_at_none_without_a_word_char_before_cursor() {
        let b = Buffer::from_str("foo bar");
        assert_eq!(prefix_at(&b, (0, 4)), None); // cursor just after the space
        assert_eq!(prefix_at(&b, (0, 0)), None); // start of line
    }

    #[test]
    fn prefix_at_rejects_a_run_starting_with_a_digit() {
        let b = Buffer::from_str("123abc");
        assert_eq!(prefix_at(&b, (0, 6)), None);
    }

    #[test]
    fn prefix_at_spans_into_the_middle_of_a_word() {
        let b = Buffer::from_str("foobar");
        // cursor inside the word, after "foo"
        assert_eq!(prefix_at(&b, (0, 3)), Some(("foo".to_string(), (0, 0))));
    }

    #[test]
    fn candidates_filter_dedup_and_exclude_the_in_progress_word() {
        // The "ap" at (0,0) is the in-progress token; it must not appear, and
        // duplicates collapse to one entry.
        let b = Buffer::from_str("ap apple apple apricot\nax");
        let c = candidates(&b, "ap", (0, 0), 0);
        assert_eq!(c, vec!["apple".to_string(), "apricot".to_string()]);
    }

    #[test]
    fn candidates_exclude_the_prefix_itself() {
        let b = Buffer::from_str("foo foo bar");
        // typing the full word "foo" at (0,0); other "foo" equals the prefix and
        // is useless, so nothing is offered.
        assert!(candidates(&b, "foo", (0, 0), 0).is_empty());
    }

    #[test]
    fn candidates_rank_nearer_lines_first() {
        let b = Buffer::from_str("albatross\n\n\nalpha alpine");
        // cursor on the last line (no token at the exclude position): alpha/alpine
        // (dist 0) beat albatross (dist 3).
        let c = candidates(&b, "al", (99, 99), 3);
        assert_eq!(
            c,
            vec![
                "alpha".to_string(),
                "alpine".to_string(),
                "albatross".to_string()
            ]
        );
    }

    #[test]
    fn candidates_break_distance_ties_by_frequency() {
        let b = Buffer::from_str("foo bar foo fond");
        // all on line 0 (dist 0): foo occurs twice, fond once -> foo first.
        let c = candidates(&b, "fo", (99, 99), 0);
        assert_eq!(c, vec!["foo".to_string(), "fond".to_string()]);
    }

    #[test]
    fn open_returns_none_when_nothing_matches() {
        let b = Buffer::from_str("foo");
        // prefix "fo" but no other word matches -> no popup.
        assert!(Completion::open(&b, (0, 2)).is_none());
    }

    #[test]
    fn move_selection_wraps() {
        let mut comp = Completion {
            prefix_start: (0, 0),
            query: "a".to_string(),
            candidates: vec!["aa".into(), "ab".into(), "ac".into()],
            selected: 0,
        };
        comp.move_up(); // wraps to last
        assert_eq!(comp.selected, 2);
        comp.move_down(); // wraps to first
        assert_eq!(comp.selected, 0);
        comp.move_down();
        assert_eq!(comp.selected, 1);
    }
}
