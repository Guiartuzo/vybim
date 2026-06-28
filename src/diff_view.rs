//! The diff view: a read-only, full-body surface showing the working tree's
//! changes against `HEAD`. A changed-files list on the left; a side-by-side
//! before/after diff of the selected file on the right.
//!
//! The diff model (`DiffRow`, `build_rows`, hunks) is computed locally with the
//! `similar` crate from the committed and working texts that [`crate::git`]
//! provides, rather than parsed out of `git diff`'s unified output — `similar`
//! yields exactly the per-line op stream that maps onto two aligned columns.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use similar::{ChangeTag, TextDiff};

use crate::git::{self, ChangeKind, ChangedFile, FileContents};
use crate::theme::Theme;

/// Width of the changed-files list column, in columns.
const LIST_WIDTH: u16 = 30;

/// One aligned row of a side-by-side diff. `Equal` lines sit across from each
/// other; `Delete` is left-only (removed), `Insert` is right-only (added).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffRow {
    Equal(String),
    Delete(String),
    Insert(String),
}

impl DiffRow {
    fn is_change(&self) -> bool {
        !matches!(self, DiffRow::Equal(_))
    }
}

/// A contiguous run of changed rows, as `[start, end)` row indices. Used for
/// next/previous-change navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hunk {
    pub start: usize,
    pub end: usize,
}

/// Which side of the view has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffFocus {
    List,
    Diff,
}

/// The diff of one file, plus the navigation state over it.
#[derive(Debug, Default)]
struct FileDiff {
    rows: Vec<DiffRow>,
    hunks: Vec<Hunk>,
    current_hunk: usize,
    scroll: usize,
    /// True when the file is binary / non-UTF-8 on either side; no rows are
    /// built and a placeholder is shown instead.
    binary: bool,
}

/// The full diff-view state: the changed-files snapshot, the selected file's
/// diff, and inner focus. `root` is `None` when not inside a git repository.
#[derive(Debug)]
pub struct DiffView {
    root: Option<PathBuf>,
    files: Vec<ChangedFile>,
    selected: usize,
    file: FileDiff,
    focus: DiffFocus,
}

impl DiffView {
    /// Snapshot git state and open the view. Outside a repo, opens in an empty
    /// "no repository" state; with a clean tree, an empty "no changes" state.
    pub fn open() -> Self {
        let root = git::repo_root();
        let files = if root.is_some() {
            git::changed_files()
        } else {
            Vec::new()
        };
        let mut view = Self {
            root,
            files,
            selected: 0,
            file: FileDiff::default(),
            focus: DiffFocus::List,
        };
        view.reload_selected();
        view
    }

    /// Re-run the git snapshot in place (the refresh key), keeping the selection
    /// clamped to the new list.
    pub fn refresh(&mut self) {
        self.root = git::repo_root();
        self.files = if self.root.is_some() {
            git::changed_files()
        } else {
            Vec::new()
        };
        if self.selected >= self.files.len() {
            self.selected = self.files.len().saturating_sub(1);
        }
        self.reload_selected();
    }

    /// Recompute the selected file's diff from its committed and working texts.
    fn reload_selected(&mut self) {
        let Some(root) = self.root.clone() else {
            self.file = FileDiff::default();
            return;
        };
        let Some(file) = self.files.get(self.selected).cloned() else {
            self.file = FileDiff::default();
            return;
        };
        // Added files have no HEAD blob; deleted files have nothing on disk.
        let old = match file.kind {
            ChangeKind::Added => FileContents::Text(String::new()),
            _ => git::file_at_head(&file.path),
        };
        let new = match file.kind {
            ChangeKind::Deleted => FileContents::Text(String::new()),
            _ => git::file_on_disk(&root, &file.path),
        };
        self.file = match (old, new) {
            (FileContents::Text(o), FileContents::Text(n)) => {
                let rows = build_rows(&o, &n);
                let hunks = build_hunks(&rows);
                FileDiff {
                    rows,
                    hunks,
                    current_hunk: 0,
                    scroll: 0,
                    binary: false,
                }
            }
            _ => FileDiff {
                binary: true,
                ..FileDiff::default()
            },
        };
    }

    // --- list navigation ---------------------------------------------------

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.reload_selected();
        }
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.files.len() {
            self.selected += 1;
            self.reload_selected();
        }
    }

    /// Move focus from the list into the diff (no-op if there is nothing to
    /// show).
    pub fn enter_diff(&mut self) {
        if !self.file.rows.is_empty() {
            self.focus = DiffFocus::Diff;
        }
    }

    pub fn back_to_list(&mut self) {
        self.focus = DiffFocus::List;
    }

    pub fn focus_in_list(&self) -> bool {
        self.focus == DiffFocus::List
    }

    // --- diff navigation ---------------------------------------------------

    pub fn scroll_up(&mut self) {
        self.file.scroll = self.file.scroll.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        if self.file.scroll + 1 < self.file.rows.len() {
            self.file.scroll += 1;
        }
    }

    pub fn page_up(&mut self) {
        self.file.scroll = self.file.scroll.saturating_sub(10);
    }

    pub fn page_down(&mut self) {
        let max = self.file.rows.len().saturating_sub(1);
        self.file.scroll = (self.file.scroll + 10).min(max);
    }

    /// Jump to the next hunk, wrapping to the first; scrolls it into view.
    pub fn next_hunk(&mut self) {
        if self.file.hunks.is_empty() {
            return;
        }
        self.file.current_hunk = (self.file.current_hunk + 1) % self.file.hunks.len();
        self.file.scroll = self.file.hunks[self.file.current_hunk].start;
    }

    /// Jump to the previous hunk, wrapping to the last; scrolls it into view.
    pub fn prev_hunk(&mut self) {
        if self.file.hunks.is_empty() {
            return;
        }
        let n = self.file.hunks.len();
        self.file.current_hunk = (self.file.current_hunk + n - 1) % n;
        self.file.scroll = self.file.hunks[self.file.current_hunk].start;
    }

    // --- rendering ---------------------------------------------------------

    /// Draw the view over `area`, claiming the whole body. Modal: the caller has
    /// ensured input is routed here while it is open.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        frame.render_widget(Clear, area);
        // Two adjacent panels; each is its own rounded box (no divider column —
        // the boxes' borders separate them).
        let [list_area, diff_area] =
            Layout::horizontal([Constraint::Length(LIST_WIDTH), Constraint::Fill(1)]).areas(area);

        self.render_list(frame, list_area, theme);
        self.render_diff(frame, diff_area, theme);
    }

    fn render_list(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // Rounded, untitled box; the changed-files list renders inside it.
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(theme.border_type())
            .border_style(Style::new().fg(theme.border));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let list_focused = self.focus == DiffFocus::List;
        let mut lines: Vec<Line> = Vec::with_capacity(self.files.len());
        for (i, f) in self.files.iter().enumerate() {
            let kind_style = match f.kind {
                ChangeKind::Modified => Style::new().fg(theme.accent),
                ChangeKind::Added => Style::new().fg(theme.diff_add_fg),
                ChangeKind::Deleted => Style::new().fg(theme.diff_del_fg),
            };
            let selected = i == self.selected;
            let row_style = if selected {
                // Selected row keeps a focus-colored foreground in both states;
                // only the background tracks focus, so this builds from tokens
                // rather than `list_row` (whose unfocused fg differs).
                let bg = if list_focused { theme.focus_bg } else { theme.inactive_bg };
                Style::new().bg(bg).fg(theme.focus_fg)
            } else {
                Style::new()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", f.kind.indicator()), kind_style.patch(row_style)),
                Span::styled(f.path.clone(), row_style),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_diff(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        // Rounded, untitled box; the diff content renders inside it.
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(theme.border_type())
            .border_style(Style::new().fg(theme.border));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Empty / placeholder states.
        if let Some(msg) = self.placeholder() {
            let p = Paragraph::new(msg).style(Style::new().fg(theme.text_muted));
            frame.render_widget(p, inner);
            return;
        }

        let path = self.files[self.selected].path.clone();
        let [header, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
        let diff_focused = self.focus == DiffFocus::Diff;
        let header_style = if diff_focused {
            Style::new().fg(theme.text).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.inactive_fg)
        };
        frame.render_widget(Paragraph::new(Span::styled(path, header_style)), header);

        // Two equal halves with a thin divider: HEAD on the left, working right.
        let [left, mid, right] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(body);

        let height = body.height as usize;
        let mut left_lines: Vec<Line> = Vec::with_capacity(height);
        let mut right_lines: Vec<Line> = Vec::with_capacity(height);
        for row in self.file.rows.iter().skip(self.file.scroll).take(height) {
            let (l, r) = render_row(row, theme);
            left_lines.push(l);
            right_lines.push(r);
        }
        frame.render_widget(Paragraph::new(left_lines), left);
        let div = Block::new()
            .borders(Borders::LEFT)
            .border_type(theme.border_type())
            .border_style(Style::new().fg(theme.border));
        frame.render_widget(div, mid);
        frame.render_widget(Paragraph::new(right_lines), right);
    }

    /// The placeholder message for the diff pane, if no diff should be drawn.
    fn placeholder(&self) -> Option<&'static str> {
        if self.root.is_none() {
            Some("No git repository found.")
        } else if self.files.is_empty() {
            Some("No changes against HEAD.")
        } else if self.file.binary {
            Some("Binary file — not shown.")
        } else if self.file.rows.is_empty() {
            Some("No changes in this file.")
        } else {
            None
        }
    }
}

/// Render one diff row into its (left, right) styled lines, gap-filling the
/// opposite side of an insert/delete.
fn render_row(row: &DiffRow, theme: &Theme) -> (Line<'static>, Line<'static>) {
    match row {
        DiffRow::Equal(t) => (Line::raw(t.clone()), Line::raw(t.clone())),
        DiffRow::Delete(t) => (
            Line::styled(t.clone(), Style::new().fg(theme.diff_del_fg).bg(theme.diff_del_bg)),
            Line::styled("", Style::new().bg(theme.diff_gap_bg)),
        ),
        DiffRow::Insert(t) => (
            Line::styled("", Style::new().bg(theme.diff_gap_bg)),
            Line::styled(t.clone(), Style::new().fg(theme.diff_add_fg).bg(theme.diff_add_bg)),
        ),
    }
}

/// Align `old` and `new` into a row stream via `similar`. Deleted lines become
/// left-only `Delete` rows, inserted lines right-only `Insert` rows, and equal
/// lines sit on both sides. Line endings are normalized away.
pub fn build_rows(old: &str, new: &str) -> Vec<DiffRow> {
    // Normalize line endings before diffing so CRLF/LF differences don't show
    // up as spurious whole-line changes.
    let old = old.replace("\r\n", "\n");
    let new = new.replace("\r\n", "\n");
    let diff = TextDiff::from_lines(&old, &new);
    let mut rows = Vec::new();
    for change in diff.iter_all_changes() {
        let text = change.value().trim_end_matches('\n').to_string();
        rows.push(match change.tag() {
            ChangeTag::Equal => DiffRow::Equal(text),
            ChangeTag::Delete => DiffRow::Delete(text),
            ChangeTag::Insert => DiffRow::Insert(text),
        });
    }
    rows
}

/// Group contiguous changed rows into hunks for next/previous navigation.
pub fn build_hunks(rows: &[DiffRow]) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut start = None;
    for (i, row) in rows.iter().enumerate() {
        match (row.is_change(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                hunks.push(Hunk { start: s, end: i });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        hunks.push(Hunk { start: s, end: rows.len() });
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_with(rows: Vec<DiffRow>) -> DiffView {
        let hunks = build_hunks(&rows);
        DiffView {
            root: Some(PathBuf::from("/repo")),
            files: vec![
                ChangedFile { path: "a.rs".into(), kind: ChangeKind::Modified },
                ChangedFile { path: "b.rs".into(), kind: ChangeKind::Added },
            ],
            selected: 0,
            file: FileDiff { rows, hunks, current_hunk: 0, scroll: 0, binary: false },
            focus: DiffFocus::List,
        }
    }

    #[test]
    fn pure_add_is_all_inserts_on_the_right() {
        let rows = build_rows("", "a\nb\n");
        assert_eq!(rows, vec![DiffRow::Insert("a".into()), DiffRow::Insert("b".into())]);
    }

    #[test]
    fn pure_delete_is_all_deletes_on_the_left() {
        let rows = build_rows("a\nb\n", "");
        assert_eq!(rows, vec![DiffRow::Delete("a".into()), DiffRow::Delete("b".into())]);
    }

    #[test]
    fn mixed_change_keeps_equal_lines_aligned() {
        let rows = build_rows("keep\nold\ntail\n", "keep\nnew\ntail\n");
        assert_eq!(
            rows,
            vec![
                DiffRow::Equal("keep".into()),
                DiffRow::Delete("old".into()),
                DiffRow::Insert("new".into()),
                DiffRow::Equal("tail".into()),
            ]
        );
    }

    #[test]
    fn crlf_does_not_create_spurious_diffs() {
        let rows = build_rows("a\r\nb\r\n", "a\nb\n");
        assert!(rows.iter().all(|r| matches!(r, DiffRow::Equal(_))));
    }

    #[test]
    fn hunks_group_contiguous_changes() {
        let rows = vec![
            DiffRow::Equal("0".into()),
            DiffRow::Delete("1".into()),
            DiffRow::Insert("2".into()),
            DiffRow::Equal("3".into()),
            DiffRow::Equal("4".into()),
            DiffRow::Insert("5".into()),
        ];
        let hunks = build_hunks(&rows);
        assert_eq!(hunks, vec![Hunk { start: 1, end: 3 }, Hunk { start: 5, end: 6 }]);
    }

    #[test]
    fn next_prev_hunk_wraps_and_scrolls() {
        let rows = vec![
            DiffRow::Delete("0".into()),
            DiffRow::Equal("1".into()),
            DiffRow::Insert("2".into()),
        ];
        let mut v = view_with(rows);
        v.focus = DiffFocus::Diff;
        assert_eq!(v.file.hunks.len(), 2);
        v.next_hunk();
        assert_eq!(v.file.current_hunk, 1);
        assert_eq!(v.file.scroll, 2); // hunk at row 2
        v.next_hunk(); // wraps to first
        assert_eq!(v.file.current_hunk, 0);
        assert_eq!(v.file.scroll, 0);
        v.prev_hunk(); // wraps back to last
        assert_eq!(v.file.current_hunk, 1);
    }

    #[test]
    fn enter_diff_requires_rows_then_back_returns() {
        let mut empty = view_with(vec![]);
        empty.enter_diff();
        assert!(empty.focus_in_list()); // nothing to enter
        let mut v = view_with(vec![DiffRow::Insert("x".into())]);
        v.enter_diff();
        assert!(!v.focus_in_list());
        v.back_to_list();
        assert!(v.focus_in_list());
    }

    #[test]
    fn select_next_prev_clamps_to_ends() {
        // reload_selected would call git; avoid it by checking index movement
        // only on a view whose git calls are harmless (selected file paths do
        // not exist, yielding empty diffs).
        let mut v = view_with(vec![DiffRow::Equal("x".into())]);
        v.select_prev(); // already at 0, clamped
        assert_eq!(v.selected, 0);
        v.select_next();
        assert_eq!(v.selected, 1);
        v.select_next(); // clamped at last
        assert_eq!(v.selected, 1);
    }
}
