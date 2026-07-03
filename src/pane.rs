//! An editor pane: a view onto a [`Buffer`] (referenced by id) with its own
//! cursor, selection, and scroll offset.
//!
//! Panes do not own their buffer. The buffer lives in a central store on the
//! `App`, and each pane refers to it by `buffer_id`. Editing methods therefore
//! take the buffer as a parameter. This indirection lets two panes view one
//! buffer and avoids the `Rc<RefCell<>>` graph that traps Rust beginners.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use crate::buffer::Buffer;
use crate::complete::is_word_char;
use crate::syntax::Syntax;
use crate::theme::Theme;

/// Cursor position within the buffer. `target_col` remembers the column the
/// user "wants" so vertical movement across short lines doesn't lose it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,
    pub target_col: usize,
}

/// A single caret: a cursor plus its optional selection anchor. The primary
/// caret is stored as the pane's `cursor`/`anchor` fields (so the single-caret
/// path is unchanged); additional carets live in `EditorPane::secondary`.
#[derive(Debug, Clone, Copy)]
struct Caret {
    cursor: Cursor,
    anchor: Option<(usize, usize)>,
}

impl Caret {
    /// This caret's selection as an ordered (start, end) pair, if any.
    fn ordered_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        let cursor = (self.cursor.line, self.cursor.col);
        Some(if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        })
    }
}

/// Active incremental-search state for a pane: the match positions, which one
/// is current, the cursor to restore on cancel, and the query's char length.
#[derive(Debug)]
struct SearchState {
    /// `(line, col)` start of each match, in buffer order.
    matches: Vec<(usize, usize)>,
    current: usize,
    origin: Cursor,
    len: usize,
}

#[derive(Debug)]
pub struct EditorPane {
    pub buffer_id: usize,
    pub cursor: Cursor,
    /// Selection anchor (line, col) of the primary caret. The selection spans
    /// from here to the cursor; `None` means no active selection.
    anchor: Option<(usize, usize)>,
    /// Secondary carets for multi-cursor editing (empty in the common case).
    /// The primary caret is `cursor`/`anchor` above; these are the extras.
    secondary: Vec<Caret>,
    scroll_row: usize,
    scroll_col: usize,
    /// Height of the content region at the last render, used by page movement
    /// (which needs the viewport size, only known at render time).
    last_height: usize,
    /// Incremental-search state, present only while a search prompt is open.
    search: Option<SearchState>,
    /// Screen `(x, y)` of the hardware cursor at the last render, used to anchor
    /// the completion popup to the cursor. `None` before the first focused render.
    cursor_screen: Option<(u16, u16)>,
}

impl EditorPane {
    pub fn new(buffer_id: usize) -> Self {
        Self {
            buffer_id,
            cursor: Cursor::default(),
            anchor: None,
            secondary: Vec::new(),
            scroll_row: 0,
            scroll_col: 0,
            last_height: 0,
            search: None,
            cursor_screen: None,
        }
    }

    /// Point this pane at a different buffer, resetting the view state.
    pub fn set_buffer(&mut self, buffer_id: usize) {
        self.buffer_id = buffer_id;
        self.cursor = Cursor::default();
        self.anchor = None;
        self.secondary.clear();
        self.scroll_row = 0;
        self.scroll_col = 0;
        self.search = None;
        self.cursor_screen = None;
    }

    // --- queries -----------------------------------------------------------

    pub fn has_selection(&self) -> bool {
        self.anchor.is_some()
    }

    /// Screen `(x, y)` of the cursor at the last focused render, for anchoring
    /// the completion popup. `None` before the pane has rendered focused.
    pub fn cursor_screen(&self) -> Option<(u16, u16)> {
        self.cursor_screen
    }

    fn last_line(&self, buffer: &Buffer) -> usize {
        buffer.line_count() - 1
    }

    fn line_len(&self, buffer: &Buffer, line: usize) -> usize {
        buffer.line_len_chars(line)
    }

    /// The primary caret's selection as an ordered (start, end) pair, if any.
    fn ordered_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        self.primary_caret().ordered_selection()
    }

    // --- carets ------------------------------------------------------------

    /// The primary caret (its cursor + anchor) as a [`Caret`] value.
    fn primary_caret(&self) -> Caret {
        Caret {
            cursor: self.cursor,
            anchor: self.anchor,
        }
    }

    /// Write a [`Caret`] back as the primary caret.
    fn set_primary_caret(&mut self, c: Caret) {
        self.cursor = c.cursor;
        self.anchor = c.anchor;
    }

    /// Whether any secondary carets are present (multi-cursor is active).
    pub fn has_secondary_carets(&self) -> bool {
        !self.secondary.is_empty()
    }

    /// Apply `f` to every caret (primary first, then each secondary), then merge
    /// any that became coincident. Used for movement: each caret moves on its
    /// own and no buffer indices shift, so order does not matter.
    fn apply_to_carets(&mut self, mut f: impl FnMut(&mut Caret)) {
        let mut primary = self.primary_caret();
        f(&mut primary);
        self.set_primary_caret(primary);
        for c in &mut self.secondary {
            f(c);
        }
        self.dedup_merge_carets();
    }

    /// Sort the secondary carets by position and merge any coincident with the
    /// primary or each other, always keeping the primary. A no-op while there
    /// are no secondary carets, so the single-caret path is untouched.
    fn dedup_merge_carets(&mut self) {
        if self.secondary.is_empty() {
            return;
        }
        let primary = (self.cursor.line, self.cursor.col);
        self.secondary
            .retain(|c| (c.cursor.line, c.cursor.col) != primary);
        self.secondary
            .sort_by_key(|c| (c.cursor.line, c.cursor.col));
        self.secondary
            .dedup_by_key(|c| (c.cursor.line, c.cursor.col));
    }

    /// The highest line index over all carets (primary + secondary).
    fn max_caret_line(&self) -> usize {
        self.secondary
            .iter()
            .map(|c| c.cursor.line)
            .max()
            .unwrap_or(self.cursor.line)
            .max(self.cursor.line)
    }

    /// The lowest line index over all carets (primary + secondary).
    fn min_caret_line(&self) -> usize {
        self.secondary
            .iter()
            .map(|c| c.cursor.line)
            .min()
            .unwrap_or(self.cursor.line)
            .min(self.cursor.line)
    }

    /// Add a caret one line below the bottom-most caret, at the primary caret's
    /// target column (clamped to that line). A no-op at the last line.
    pub fn add_caret_below(&mut self, buffer: &Buffer) {
        let bottom = self.max_caret_line();
        if bottom >= self.last_line(buffer) {
            return;
        }
        self.push_caret(buffer, bottom + 1);
    }

    /// Add a caret one line above the top-most caret, at the primary caret's
    /// target column (clamped to that line). A no-op at the first line.
    pub fn add_caret_above(&mut self, buffer: &Buffer) {
        let top = self.min_caret_line();
        if top == 0 {
            return;
        }
        self.push_caret(buffer, top - 1);
    }

    /// Push a new secondary caret onto `line` at the primary's target column.
    fn push_caret(&mut self, buffer: &Buffer, line: usize) {
        let col = self.cursor.target_col;
        self.secondary.push(Caret {
            cursor: Cursor {
                line,
                col: col.min(self.line_len(buffer, line)),
                target_col: col,
            },
            anchor: None,
        });
        self.dedup_merge_carets();
    }

    /// Drop all secondary carets, leaving just the primary.
    pub fn collapse_carets(&mut self) {
        self.secondary.clear();
    }

    // --- movement ----------------------------------------------------------

    pub fn move_left(&mut self, buffer: &Buffer, extend: bool) {
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            if c.cursor.col > 0 {
                c.cursor.col -= 1;
            } else if c.cursor.line > 0 {
                c.cursor.line -= 1;
                c.cursor.col = buffer.line_len_chars(c.cursor.line);
            }
            c.cursor.target_col = c.cursor.col;
        });
    }

    pub fn move_right(&mut self, buffer: &Buffer, extend: bool) {
        let last = buffer.line_count() - 1;
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            if c.cursor.col < buffer.line_len_chars(c.cursor.line) {
                c.cursor.col += 1;
            } else if c.cursor.line < last {
                c.cursor.line += 1;
                c.cursor.col = 0;
            }
            c.cursor.target_col = c.cursor.col;
        });
    }

    /// Move each caret forward to the next word boundary: skip any separators
    /// (crossing line ends), then advance over word chars, landing just past the
    /// word. `extend` grows the selection instead of collapsing it.
    pub fn move_word_right(&mut self, buffer: &Buffer, extend: bool) {
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            let (line, col) = scan_word_right(buffer, c.cursor.line, c.cursor.col);
            c.cursor.line = line;
            c.cursor.col = col;
            c.cursor.target_col = col;
        });
    }

    /// Move each caret backward to the previous word boundary: step back over
    /// separators (crossing line starts), then over word chars, landing at the
    /// start of the word. `extend` grows the selection instead of collapsing it.
    pub fn move_word_left(&mut self, buffer: &Buffer, extend: bool) {
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            let (line, col) = scan_word_left(buffer, c.cursor.line, c.cursor.col);
            c.cursor.line = line;
            c.cursor.col = col;
            c.cursor.target_col = col;
        });
    }

    pub fn move_up(&mut self, buffer: &Buffer, extend: bool) {
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            if c.cursor.line > 0 {
                c.cursor.line -= 1;
                c.cursor.col = c
                    .cursor
                    .target_col
                    .min(buffer.line_len_chars(c.cursor.line));
            }
        });
    }

    pub fn move_down(&mut self, buffer: &Buffer, extend: bool) {
        let last = buffer.line_count() - 1;
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            if c.cursor.line < last {
                c.cursor.line += 1;
                c.cursor.col = c
                    .cursor
                    .target_col
                    .min(buffer.line_len_chars(c.cursor.line));
            }
        });
    }

    /// Move to column zero of the current line (Home).
    pub fn move_line_start(&mut self, _buffer: &Buffer, extend: bool) {
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            c.cursor.col = 0;
            c.cursor.target_col = 0;
        });
    }

    /// Move to the end of the current line (End).
    pub fn move_line_end(&mut self, buffer: &Buffer, extend: bool) {
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            c.cursor.col = buffer.line_len_chars(c.cursor.line);
            c.cursor.target_col = c.cursor.col;
        });
    }

    /// Move up by roughly one viewport height (PageUp), keeping the target
    /// column. Uses the height cached at the last render.
    pub fn page_up(&mut self, buffer: &Buffer, extend: bool) {
        let page = self.page_rows();
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            c.cursor.line = c.cursor.line.saturating_sub(page);
            c.cursor.col = c
                .cursor
                .target_col
                .min(buffer.line_len_chars(c.cursor.line));
        });
    }

    /// Move down by roughly one viewport height (PageDown), keeping the target
    /// column. Uses the height cached at the last render.
    pub fn page_down(&mut self, buffer: &Buffer, extend: bool) {
        let last = buffer.line_count() - 1;
        let page = self.page_rows();
        self.apply_to_carets(|c| {
            pre_move(c, extend);
            c.cursor.line = (c.cursor.line + page).min(last);
            c.cursor.col = c
                .cursor
                .target_col
                .min(buffer.line_len_chars(c.cursor.line));
        });
    }

    /// Rows to jump for a page movement: the last-rendered content height, or a
    /// sane default before the first render.
    fn page_rows(&self) -> usize {
        self.last_height.max(1)
    }

    // --- editing -----------------------------------------------------------

    /// Delete the active selection, if any, moving the cursor to its start.
    /// Returns whether a selection was deleted.
    fn delete_selection(&mut self, buffer: &mut Buffer) -> bool {
        let Some((start, end)) = self.ordered_selection() else {
            return false;
        };
        let s = buffer.char_idx(start.0, start.1);
        let e = buffer.char_idx(end.0, end.1);
        buffer.remove(s, e);
        self.cursor.line = start.0;
        self.cursor.col = start.1;
        self.cursor.target_col = start.1;
        self.anchor = None;
        true
    }

    pub fn insert_char(&mut self, buffer: &mut Buffer, ch: char) {
        if !self.secondary.is_empty() {
            let text = ch.to_string();
            return self.fan_out_edit(buffer, |c, b| {
                let (s, e) = caret_remove_range(c, b);
                (s, e, text.clone())
            });
        }
        buffer.begin_edit(buffer.char_idx(self.cursor.line, self.cursor.col));
        self.delete_selection(buffer);
        let idx = buffer.char_idx(self.cursor.line, self.cursor.col);
        let mut encoded = [0u8; 4];
        buffer.insert(idx, ch.encode_utf8(&mut encoded));
        self.cursor.col += 1;
        self.cursor.target_col = self.cursor.col;
    }

    pub fn insert_newline(&mut self, buffer: &mut Buffer) {
        if !self.secondary.is_empty() {
            return self.fan_out_edit(buffer, |c, b| {
                let (s, e) = caret_remove_range(c, b);
                (s, e, "\n".to_string())
            });
        }
        // A line break is its own undo step: close any run before it, and close
        // the newline group after so the next typing starts fresh.
        buffer.finalize();
        buffer.begin_edit(buffer.char_idx(self.cursor.line, self.cursor.col));
        self.delete_selection(buffer);
        let idx = buffer.char_idx(self.cursor.line, self.cursor.col);
        buffer.insert(idx, "\n");
        buffer.finalize();
        self.cursor.line += 1;
        self.cursor.col = 0;
        self.cursor.target_col = 0;
    }

    /// Delete the character before the cursor, joining lines at a line start.
    pub fn backspace(&mut self, buffer: &mut Buffer) {
        if !self.secondary.is_empty() {
            return self.fan_out_edit(buffer, |c, b| match c.ordered_selection() {
                Some((start, end)) => (
                    b.char_idx(start.0, start.1),
                    b.char_idx(end.0, end.1),
                    String::new(),
                ),
                None => {
                    let idx = b.char_idx(c.cursor.line, c.cursor.col);
                    let s = idx.saturating_sub(1);
                    (s, idx, String::new())
                }
            });
        }
        buffer.begin_edit(buffer.char_idx(self.cursor.line, self.cursor.col));
        if self.delete_selection(buffer) {
            return;
        }
        let idx = buffer.char_idx(self.cursor.line, self.cursor.col);
        if idx == 0 {
            return;
        }
        buffer.remove(idx - 1, idx);
        let (line, col) = buffer.line_col(idx - 1);
        self.cursor.line = line;
        self.cursor.col = col;
        self.cursor.target_col = col;
    }

    /// Delete the character at the cursor, joining lines at a line end.
    pub fn delete_forward(&mut self, buffer: &mut Buffer) {
        if !self.secondary.is_empty() {
            let len = buffer.len_chars();
            return self.fan_out_edit(buffer, |c, b| match c.ordered_selection() {
                Some((start, end)) => (
                    b.char_idx(start.0, start.1),
                    b.char_idx(end.0, end.1),
                    String::new(),
                ),
                None => {
                    let idx = b.char_idx(c.cursor.line, c.cursor.col);
                    let e = if idx < len { idx + 1 } else { idx };
                    (idx, e, String::new())
                }
            });
        }
        buffer.begin_edit(buffer.char_idx(self.cursor.line, self.cursor.col));
        if self.delete_selection(buffer) {
            return;
        }
        let idx = buffer.char_idx(self.cursor.line, self.cursor.col);
        if idx < buffer.len_chars() {
            buffer.remove(idx, idx + 1);
        }
    }

    /// Apply one planned edit per caret as a single batch, then re-place every
    /// caret. `plan(caret, buffer)` returns `(remove_start, remove_end,
    /// insert_text)` as char indices against the *current* buffer. Edits are
    /// applied low-to-high with a running offset so positions stay correct
    /// across length changes and newlines; each caret lands just past its
    /// inserted text with its selection cleared.
    fn fan_out_edit(
        &mut self,
        buffer: &mut Buffer,
        mut plan: impl FnMut(&Caret, &Buffer) -> (usize, usize, String),
    ) {
        // Slot 0 is the primary caret; the rest are the secondaries in order.
        let mut carets: Vec<Caret> = Vec::with_capacity(1 + self.secondary.len());
        carets.push(self.primary_caret());
        carets.extend(self.secondary.iter().copied());

        let mut planned: Vec<(usize, (usize, usize, String))> = carets
            .iter()
            .enumerate()
            .map(|(i, c)| (i, plan(c, buffer)))
            .collect();
        planned.sort_by_key(|(_, (s, _, _))| *s);

        buffer.finalize();
        let mut delta: isize = 0;
        let mut new_pos = vec![0usize; carets.len()];
        for (slot, (s, e, text)) in &planned {
            let s2 = (*s as isize + delta) as usize;
            let e2 = (*e as isize + delta) as usize;
            buffer.begin_edit(s2);
            if s2 < e2 {
                buffer.remove(s2, e2);
            }
            if !text.is_empty() {
                buffer.insert(s2, text);
            }
            new_pos[*slot] = s2 + text.chars().count();
            delta += text.chars().count() as isize - (*e as isize - *s as isize);
        }
        buffer.finalize();

        for (i, c) in carets.iter_mut().enumerate() {
            let (line, col) = buffer.line_col(new_pos[i]);
            c.cursor.line = line;
            c.cursor.col = col;
            c.cursor.target_col = col;
            c.anchor = None;
        }
        self.set_primary_caret(carets[0]);
        self.secondary = carets[1..].to_vec();
        self.dedup_merge_carets();
    }

    /// Undo the last edit group on `buffer`, moving this pane's cursor to the
    /// edit site and clearing any selection. A no-op when there is nothing to
    /// undo.
    pub fn undo(&mut self, buffer: &mut Buffer) {
        if let Some(idx) = buffer.undo() {
            self.move_to_edit(buffer, idx);
        }
    }

    /// Redo the last undone edit group on `buffer`, moving this pane's cursor to
    /// the edit site and clearing any selection. A no-op when there is nothing
    /// to redo.
    pub fn redo(&mut self, buffer: &mut Buffer) {
        if let Some(idx) = buffer.redo() {
            self.move_to_edit(buffer, idx);
        }
    }

    /// Place the cursor at buffer char index `idx`, clear the selection, and
    /// collapse to a single caret, shared by undo and redo.
    fn move_to_edit(&mut self, buffer: &Buffer, idx: usize) {
        let (line, col) = buffer.line_col(idx);
        self.cursor.line = line;
        self.cursor.col = col;
        self.cursor.target_col = col;
        self.anchor = None;
        self.secondary.clear();
    }

    // --- search & go-to-line -----------------------------------------------

    /// Begin a search: remember the current cursor so a cancel can restore it.
    pub fn search_begin(&mut self) {
        self.search = Some(SearchState {
            matches: Vec::new(),
            current: 0,
            origin: self.cursor,
            len: 0,
        });
    }

    /// Recompute matches for `query` and jump to the nearest one at or after the
    /// search origin (wrapping to the first). An empty query or no matches puts
    /// the cursor back at the origin with no selection.
    pub fn search_update(&mut self, buffer: &Buffer, query: &str) {
        let Some(origin) = self.search.as_ref().map(|s| s.origin) else {
            return;
        };
        let matches = if query.is_empty() {
            Vec::new()
        } else {
            find_matches(buffer, query)
        };
        let current = nearest_at_or_after(&matches, (origin.line, origin.col)).unwrap_or(0);
        if let Some(s) = self.search.as_mut() {
            s.matches = matches;
            s.len = query.chars().count();
            s.current = current;
        }
        if self.search.as_ref().is_some_and(|s| s.matches.is_empty()) {
            self.cursor = origin;
            self.cursor.target_col = origin.col;
            self.anchor = None;
        } else {
            self.select_current_match();
        }
    }

    /// Move to the next match, wrapping past the last to the first.
    pub fn search_next(&mut self) {
        if let Some(s) = self.search.as_mut() {
            if s.matches.is_empty() {
                return;
            }
            s.current = (s.current + 1) % s.matches.len();
        }
        self.select_current_match();
    }

    /// Move to the previous match, wrapping past the first to the last.
    pub fn search_prev(&mut self) {
        if let Some(s) = self.search.as_mut() {
            if s.matches.is_empty() {
                return;
            }
            s.current = (s.current + s.matches.len() - 1) % s.matches.len();
        }
        self.select_current_match();
    }

    /// Confirm the search: drop the search state but leave the match selected.
    pub fn search_commit(&mut self) {
        self.search = None;
    }

    /// Abandon the search: restore the origin cursor and clear the selection.
    pub fn search_cancel(&mut self) {
        if let Some(s) = self.search.take() {
            self.cursor = s.origin;
            self.cursor.target_col = s.origin.col;
            self.anchor = None;
        }
    }

    /// Select the current match by spanning it: anchor at its start, cursor at
    /// its end, so the existing selection highlight draws it.
    fn select_current_match(&mut self) {
        let Some(s) = self.search.as_ref() else {
            return;
        };
        if s.matches.is_empty() {
            return;
        }
        let (line, col) = s.matches[s.current];
        let len = s.len;
        self.anchor = Some((line, col));
        self.cursor.line = line;
        self.cursor.col = col + len;
        self.cursor.target_col = self.cursor.col;
    }

    /// Move the cursor to the start of 1-based line `n`, clamped to the last
    /// line, clearing any selection.
    pub fn goto_line(&mut self, buffer: &Buffer, n: usize) {
        let line = n.saturating_sub(1).min(self.last_line(buffer));
        self.cursor.line = line;
        self.cursor.col = 0;
        self.cursor.target_col = 0;
        self.anchor = None;
    }

    /// The primary caret's current `(line, col)`, for snapshotting a jump origin.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        (self.cursor.line, self.cursor.col)
    }

    /// Move the primary caret to `(line, col)`, clamped into `buffer` (line to
    /// the last line, col to that line's length), clearing any selection and
    /// collapsing secondary carets. The next render reveals the caret via the
    /// same scroll-into-view path that go-to-line and search already use.
    pub fn set_cursor(&mut self, buffer: &Buffer, line: usize, col: usize) {
        self.collapse_carets();
        let line = line.min(self.last_line(buffer));
        let col = col.min(self.line_len(buffer, line));
        self.cursor.line = line;
        self.cursor.col = col;
        self.cursor.target_col = col;
        self.anchor = None;
    }

    // --- autocomplete ------------------------------------------------------

    /// Accept a completion: replace the word-prefix spanning from `prefix_start`
    /// to the cursor with `word`, leaving the cursor at the end of the inserted
    /// word. The swap is a single undo step.
    pub fn complete_accept(
        &mut self,
        buffer: &mut Buffer,
        prefix_start: (usize, usize),
        word: &str,
    ) {
        let start = buffer.char_idx(prefix_start.0, prefix_start.1);
        let end = buffer.char_idx(self.cursor.line, self.cursor.col);
        buffer.finalize();
        buffer.begin_edit(start);
        if start < end {
            buffer.remove(start, end);
        }
        buffer.insert(start, word);
        buffer.finalize();
        let (line, col) = buffer.line_col(start + word.chars().count());
        self.cursor.line = line;
        self.cursor.col = col;
        self.cursor.target_col = col;
        self.anchor = None;
    }

    // --- rendering ---------------------------------------------------------

    /// Render this pane into `area`: the buffer's visible region plus a
    /// one-row status bar. Only the focused pane places the hardware cursor.
    /// `syntax` is `Some` when the buffer's language has a grammar. `note` is
    /// an app-level status (LSP progress etc.) appended to the status bar.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        buffer: &Buffer,
        syntax: Option<&Syntax>,
        focused: bool,
        theme: &Theme,
        note: Option<&str>,
    ) {
        let [content, status] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

        self.render_content(frame, content, buffer, syntax, focused, theme);
        self.render_status(frame, status, buffer, focused, theme, note);
    }

    fn render_content(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        buffer: &Buffer,
        syntax: Option<&Syntax>,
        focused: bool,
        theme: &Theme,
    ) {
        // Reserve a left gutter for line numbers; text fills the rest.
        let num_w = gutter_num_width(buffer.line_count());
        let gutter_w = (num_w + 1) as u16;
        let [gutter, content] =
            Layout::horizontal([Constraint::Length(gutter_w), Constraint::Min(0)]).areas(area);

        let height = content.height as usize;
        let width = content.width as usize;
        self.last_height = height;

        self.scroll_row = scroll_to_show(self.scroll_row, self.cursor.line, height);
        self.scroll_col = scroll_to_show(self.scroll_col, self.cursor.col, width);

        let mut numbers: Vec<Line> = Vec::with_capacity(height);
        let mut lines: Vec<Line> = Vec::with_capacity(height);
        for row in 0..height {
            let line_idx = self.scroll_row + row;
            if line_idx >= buffer.line_count() {
                break;
            }
            let is_current = line_idx == self.cursor.line;
            let num_style = if is_current && focused {
                Style::new().fg(theme.text)
            } else {
                Style::new().fg(theme.text_muted)
            };
            numbers.push(Line::styled(
                format!("{:>width$} ", line_idx + 1, width = num_w),
                num_style,
            ));

            let text = buffer.line_text(line_idx);
            let visible: String = text.chars().skip(self.scroll_col).collect();
            lines.push(highlight_line(&visible, syntax));
        }
        frame.render_widget(Paragraph::new(Text::from(numbers)), gutter);
        frame.render_widget(Paragraph::new(Text::from(lines)), content);

        // Subtle current-line highlight: patch the background across the whole
        // row (gutter + text) so empty cells are covered too.
        if focused && self.cursor.line >= self.scroll_row {
            let row = (self.cursor.line - self.scroll_row) as u16;
            if row < content.height {
                let row_rect = Rect::new(area.x, content.y + row, area.width, 1);
                frame
                    .buffer_mut()
                    .set_style(row_rect, Style::new().bg(theme.cursor_line));
            }
        }

        // Paint selections over the text (after the current-line tint so the
        // selected span wins where they overlap). Every caret — primary and
        // secondary — contributes its own selection.
        let scroll_row = self.scroll_row;
        let scroll_col = self.scroll_col;
        let selection_bg = theme.selection;
        let paint_selection =
            |frame: &mut Frame, sel_start: (usize, usize), sel_end: (usize, usize)| {
                for row in 0..height {
                    let line_idx = scroll_row + row;
                    if line_idx < sel_start.0 || line_idx > sel_end.0 {
                        continue;
                    }
                    let start_col = if line_idx == sel_start.0 {
                        sel_start.1
                    } else {
                        0
                    };
                    // For lines fully inside the selection, extend one cell past the
                    // end of line so the selected newline reads as highlighted.
                    let end_col = if line_idx == sel_end.0 {
                        sel_end.1
                    } else {
                        buffer.line_len_chars(line_idx) + 1
                    };
                    let vis_start = start_col.max(scroll_col);
                    if end_col <= vis_start {
                        continue;
                    }
                    let sx = content.x + (vis_start - scroll_col) as u16;
                    let avail = content.width.saturating_sub(sx - content.x);
                    let w = ((end_col - vis_start) as u16).min(avail);
                    if w == 0 {
                        continue;
                    }
                    let rect = Rect::new(sx, content.y + row as u16, w, 1);
                    frame
                        .buffer_mut()
                        .set_style(rect, Style::new().bg(selection_bg));
                }
            };
        if let Some((s, e)) = self.ordered_selection() {
            paint_selection(frame, s, e);
        }
        for caret in &self.secondary {
            if let Some((s, e)) = caret.ordered_selection() {
                paint_selection(frame, s, e);
            }
        }

        // Paint each secondary caret as a reversed cell. A terminal has only one
        // hardware cursor (kept on the primary caret below), so the extras are
        // drawn in.
        for caret in &self.secondary {
            if caret.cursor.line < scroll_row || caret.cursor.col < scroll_col {
                continue;
            }
            let row = caret.cursor.line - scroll_row;
            let col = caret.cursor.col - scroll_col;
            if row >= height || col >= width {
                continue;
            }
            let rect = Rect::new(content.x + col as u16, content.y + row as u16, 1, 1);
            frame
                .buffer_mut()
                .set_style(rect, Style::new().add_modifier(Modifier::REVERSED));
        }

        if focused {
            let cx = content.x + (self.cursor.col.saturating_sub(self.scroll_col)) as u16;
            let cy = content.y + (self.cursor.line.saturating_sub(self.scroll_row)) as u16;
            frame.set_cursor_position((cx, cy));
            self.cursor_screen = Some((cx, cy));
        } else {
            self.cursor_screen = None;
        }
    }

    fn render_status(
        &self,
        frame: &mut Frame,
        area: Rect,
        buffer: &Buffer,
        focused: bool,
        theme: &Theme,
        note: Option<&str>,
    ) {
        let name = buffer
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[No Name]".to_string());
        let dirty = if buffer.is_dirty() { " [+]" } else { "" };
        let selection = if self.has_selection() { "  SEL" } else { "" };
        let note = note.map(|n| format!("    {n}")).unwrap_or_default();
        let text = format!(
            " {name}{dirty}{selection}    Ln {}, Col {}{note} ",
            self.cursor.line + 1,
            self.cursor.col + 1
        );
        // The focused pane's status bar stands out so it's clear where input goes.
        let style = theme.list_row(focused);
        frame.render_widget(Paragraph::new(text).style(style), area);
    }
}

/// Build a styled line from `text`, applying syntax highlight spans (byte
/// ranges) where a grammar is available, and leaving gaps in the default style.
/// The returned line owns its text, so it does not borrow `text`.
fn highlight_line(text: &str, syntax: Option<&Syntax>) -> Line<'static> {
    let Some(syntax) = syntax else {
        return Line::raw(text.to_string());
    };
    let spans = syntax.highlight_line(text);
    if spans.is_empty() {
        return Line::raw(text.to_string());
    }

    let mut out: Vec<Span> = Vec::new();
    let mut cursor = 0;
    for (start, end, style) in spans {
        // Spans are ordered and non-overlapping; fill any gap before this run.
        if start > cursor {
            out.push(Span::raw(text[cursor..start].to_string()));
        }
        out.push(Span::styled(text[start..end].to_string(), style));
        cursor = end;
    }
    if cursor < text.len() {
        out.push(Span::raw(text[cursor..].to_string()));
    }
    Line::from(out)
}

/// Forward word-boundary scan from `(line, col)`: skip separators (crossing line
/// ends), then advance over word chars, returning the position just past the
/// word (or the buffer end). Re-collects a line's chars only when crossing a
/// line boundary.
fn scan_word_right(buffer: &Buffer, mut line: usize, mut col: usize) -> (usize, usize) {
    let last = buffer.line_count() - 1;
    let mut chars: Vec<char> = buffer.line_text(line).chars().collect();
    // Skip separators, crossing line ends, until a word char or the buffer end.
    loop {
        if col < chars.len() {
            if is_word_char(chars[col]) {
                break;
            }
            col += 1;
        } else if line < last {
            line += 1;
            col = 0;
            chars = buffer.line_text(line).chars().collect();
        } else {
            return (line, col); // end of buffer
        }
    }
    // Advance over the word to the following separator / line end.
    while col < chars.len() && is_word_char(chars[col]) {
        col += 1;
    }
    (line, col)
}

/// Backward word-boundary scan from `(line, col)`: step back over separators
/// (crossing line starts), then over word chars, returning the start of the word
/// (or the buffer start). Re-collects a line's chars only when crossing a line
/// boundary.
fn scan_word_left(buffer: &Buffer, mut line: usize, mut col: usize) -> (usize, usize) {
    let mut chars: Vec<char> = buffer.line_text(line).chars().collect();
    // Step back over separators (looking at the char before the position).
    loop {
        if col > 0 {
            if is_word_char(chars[col - 1]) {
                break;
            }
            col -= 1;
        } else if line > 0 {
            line -= 1;
            chars = buffer.line_text(line).chars().collect();
            col = chars.len();
        } else {
            return (line, col); // start of buffer
        }
    }
    // Step back over the word to its first char.
    while col > 0 && is_word_char(chars[col - 1]) {
        col -= 1;
    }
    (line, col)
}

/// Manage a caret's selection anchor for a movement: extend keeps/sets the
/// anchor, a plain move collapses any selection.
fn pre_move(caret: &mut Caret, extend: bool) {
    if extend {
        if caret.anchor.is_none() {
            caret.anchor = Some((caret.cursor.line, caret.cursor.col));
        }
    } else {
        caret.anchor = None;
    }
}

/// The half-open char range a caret's edit removes first: its selection if any,
/// otherwise an empty range at the cursor (a pure insertion point).
fn caret_remove_range(caret: &Caret, buffer: &Buffer) -> (usize, usize) {
    match caret.ordered_selection() {
        Some((start, end)) => (
            buffer.char_idx(start.0, start.1),
            buffer.char_idx(end.0, end.1),
        ),
        None => {
            let idx = buffer.char_idx(caret.cursor.line, caret.cursor.col);
            (idx, idx)
        }
    }
}

/// All `(line, col)` starts where `query` matches case-insensitively (ASCII
/// case folding). Matching works in character columns so highlight positions
/// line up with the char-based cursor. A query never spans a newline, so every
/// match is contained within a single line.
fn find_matches(buffer: &Buffer, query: &str) -> Vec<(usize, usize)> {
    let needle: Vec<char> = query.chars().collect();
    let nlen = needle.len();
    if nlen == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in 0..buffer.line_count() {
        let hay: Vec<char> = buffer.line_text(line).chars().collect();
        if hay.len() < nlen {
            continue;
        }
        for start in 0..=(hay.len() - nlen) {
            if hay[start..start + nlen]
                .iter()
                .zip(&needle)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
            {
                out.push((line, start));
            }
        }
    }
    out
}

/// Index of the first match at or after `pos` in a buffer-ordered match list,
/// or `None` if every match precedes `pos`.
fn nearest_at_or_after(matches: &[(usize, usize)], pos: (usize, usize)) -> Option<usize> {
    matches.iter().position(|&m| m >= pos)
}

/// Width (in digits) reserved for line numbers, given the line count. A floor
/// of 3 keeps the gutter from jittering on small files.
fn gutter_num_width(line_count: usize) -> usize {
    line_count.to_string().len().max(3)
}

/// Given the current scroll offset, the cursor index, and the viewport size on
/// one axis, return the scroll offset that keeps the cursor visible while
/// moving as little as possible.
fn scroll_to_show(scroll: usize, cursor: usize, viewport: usize) -> usize {
    if viewport == 0 {
        return scroll;
    }
    if cursor < scroll {
        cursor
    } else if cursor >= scroll + viewport {
        cursor + 1 - viewport
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane over a fresh in-memory buffer.
    fn setup(text: &str) -> (EditorPane, Buffer) {
        (EditorPane::new(0), Buffer::from_str(text))
    }

    #[test]
    fn cursor_inside_viewport_does_not_scroll() {
        assert_eq!(scroll_to_show(10, 12, 20), 10);
    }

    #[test]
    fn cursor_above_viewport_scrolls_up_to_cursor() {
        assert_eq!(scroll_to_show(10, 4, 20), 4);
    }

    #[test]
    fn cursor_below_viewport_scrolls_just_enough() {
        assert_eq!(scroll_to_show(10, 35, 20), 16);
    }

    #[test]
    fn zero_viewport_is_a_noop() {
        assert_eq!(scroll_to_show(7, 100, 0), 7);
    }

    #[test]
    fn vertical_move_preserves_target_column() {
        let (mut p, b) = setup("abcde\nxy\nlongerline");
        for _ in 0..4 {
            p.move_right(&b, false);
        }
        assert_eq!((p.cursor.line, p.cursor.col), (0, 4));
        p.move_down(&b, false); // "xy" len 2 -> clamp to 2, target stays 4
        assert_eq!((p.cursor.line, p.cursor.col), (1, 2));
        assert_eq!(p.cursor.target_col, 4);
        p.move_down(&b, false); // "longerline" -> col back to 4
        assert_eq!((p.cursor.line, p.cursor.col), (2, 4));
    }

    #[test]
    fn move_left_wraps_to_previous_line_end() {
        let (mut p, b) = setup("ab\ncd");
        p.move_down(&b, false);
        assert_eq!((p.cursor.line, p.cursor.col), (1, 0));
        p.move_left(&b, false);
        assert_eq!((p.cursor.line, p.cursor.col), (0, 2));
    }

    #[test]
    fn word_right_stops_at_end_of_next_word() {
        let (mut p, b) = setup("foo bar baz");
        p.move_word_right(&b, false); // end of "foo"
        assert_eq!((p.cursor.line, p.cursor.col), (0, 3));
        p.move_word_right(&b, false); // skip space, end of "bar"
        assert_eq!((p.cursor.line, p.cursor.col), (0, 7));
    }

    #[test]
    fn word_left_stops_at_start_of_previous_word() {
        let (mut p, b) = setup("foo bar baz");
        p.cursor.col = 11; // end of line
        p.move_word_left(&b, false); // start of "baz"
        assert_eq!((p.cursor.line, p.cursor.col), (0, 8));
        p.move_word_left(&b, false); // start of "bar"
        assert_eq!((p.cursor.line, p.cursor.col), (0, 4));
    }

    #[test]
    fn word_movement_wraps_across_line_boundaries() {
        let (mut p, b) = setup("foo\nbar");
        p.cursor.col = 3; // end of "foo"
        p.move_word_right(&b, false); // cross newline, end of "bar"
        assert_eq!((p.cursor.line, p.cursor.col), (1, 3));
        p.move_word_left(&b, false); // start of "bar"
        assert_eq!((p.cursor.line, p.cursor.col), (1, 0));
        p.move_word_left(&b, false); // cross newline back, start of "foo"
        assert_eq!((p.cursor.line, p.cursor.col), (0, 0));
    }

    #[test]
    fn word_movement_clamps_at_buffer_ends() {
        let (mut p, b) = setup("foo bar");
        p.move_word_left(&b, false); // already at start
        assert_eq!((p.cursor.line, p.cursor.col), (0, 0));
        p.cursor.col = 7; // end of buffer
        p.move_word_right(&b, false); // nowhere to go
        assert_eq!((p.cursor.line, p.cursor.col), (0, 7));
    }

    #[test]
    fn shift_word_move_extends_selection_plain_collapses() {
        let (mut p, b) = setup("foo bar");
        p.move_word_right(&b, true); // select "foo"
        assert_eq!(p.ordered_selection(), Some(((0, 0), (0, 3))));
        p.move_word_left(&b, false); // plain move collapses
        assert_eq!(p.ordered_selection(), None);
    }

    #[test]
    fn word_movement_applies_at_every_caret() {
        let (mut p, b) = setup("foo bar\nfoo bar");
        p.add_caret_below(&b);
        p.move_word_right(&b, false); // both carets to end of "foo"
        assert_eq!((p.cursor.line, p.cursor.col), (0, 3));
        assert_eq!(
            (p.secondary[0].cursor.line, p.secondary[0].cursor.col),
            (1, 3)
        );
    }

    #[test]
    fn insert_chars_and_newline() {
        let (mut p, mut b) = setup("");
        p.insert_char(&mut b, 'h');
        p.insert_char(&mut b, 'i');
        assert_eq!(b.line_text(0), "hi");
        assert_eq!(p.cursor.col, 2);
        p.insert_newline(&mut b);
        assert_eq!((p.cursor.line, p.cursor.col), (1, 0));
        p.insert_char(&mut b, 'x');
        assert_eq!(b.line_text(1), "x");
        assert!(b.is_dirty());
    }

    #[test]
    fn enter_splits_line() {
        let (mut p, mut b) = setup("abcd");
        p.move_right(&b, false);
        p.move_right(&b, false);
        p.insert_newline(&mut b);
        assert_eq!(b.line_text(0), "ab");
        assert_eq!(b.line_text(1), "cd");
        assert_eq!((p.cursor.line, p.cursor.col), (1, 0));
    }

    #[test]
    fn backspace_at_line_start_joins_lines() {
        let (mut p, mut b) = setup("ab\ncd");
        p.move_down(&b, false);
        p.backspace(&mut b);
        assert_eq!(b.line_text(0), "abcd");
        assert_eq!((p.cursor.line, p.cursor.col), (0, 2));
    }

    #[test]
    fn delete_forward_at_line_end_joins_lines() {
        let (mut p, mut b) = setup("ab\ncd");
        p.move_right(&b, false);
        p.move_right(&b, false);
        p.delete_forward(&mut b);
        assert_eq!(b.line_text(0), "abcd");
        assert_eq!((p.cursor.line, p.cursor.col), (0, 2));
    }

    #[test]
    fn shift_arrow_selects_and_typing_replaces() {
        let (mut p, mut b) = setup("hello");
        p.move_right(&b, true);
        p.move_right(&b, true);
        p.move_right(&b, true); // select "hel"
        assert!(p.has_selection());
        p.insert_char(&mut b, 'H');
        assert_eq!(b.line_text(0), "Hlo");
        assert_eq!(p.cursor.col, 1);
        assert!(!p.has_selection());
    }

    #[test]
    fn plain_move_collapses_selection() {
        let (mut p, b) = setup("hello");
        p.move_right(&b, true);
        assert!(p.has_selection());
        p.move_right(&b, false);
        assert!(!p.has_selection());
    }

    #[test]
    fn backspace_removes_selection() {
        let (mut p, mut b) = setup("hello");
        p.move_right(&b, true);
        p.move_right(&b, true); // select "he"
        p.backspace(&mut b);
        assert_eq!(b.line_text(0), "llo");
        assert!(!p.has_selection());
        assert_eq!(p.cursor.col, 0);
    }

    #[test]
    fn home_and_end_move_to_line_edges() {
        let (mut p, b) = setup("hello\nworld");
        p.move_down(&b, false); // line 1, col 0
        p.move_line_end(&b, false);
        assert_eq!((p.cursor.line, p.cursor.col), (1, 5));
        p.move_line_start(&b, false);
        assert_eq!((p.cursor.line, p.cursor.col), (1, 0));
    }

    #[test]
    fn shift_end_extends_selection() {
        let (mut p, b) = setup("hello");
        p.move_line_end(&b, true);
        assert!(p.has_selection());
        assert_eq!(p.cursor.col, 5);
    }

    #[test]
    fn plain_home_collapses_selection() {
        let (mut p, b) = setup("hello");
        p.move_right(&b, true);
        assert!(p.has_selection());
        p.move_line_start(&b, false);
        assert!(!p.has_selection());
        assert_eq!(p.cursor.col, 0);
    }

    #[test]
    fn page_down_and_up_jump_by_viewport_height() {
        let text = (0..20)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (mut p, b) = setup(&text);
        p.last_height = 5; // pretend a 5-row viewport
        p.page_down(&b, false);
        assert_eq!(p.cursor.line, 5);
        p.page_down(&b, false);
        assert_eq!(p.cursor.line, 10);
        p.page_up(&b, false);
        assert_eq!(p.cursor.line, 5);
    }

    #[test]
    fn page_down_clamps_to_last_line() {
        let (mut p, b) = setup("a\nb\nc");
        p.last_height = 100;
        p.page_down(&b, false);
        assert_eq!(p.cursor.line, 2); // last line, not past it
    }

    #[test]
    fn shift_page_down_extends_selection() {
        let text = (0..10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let (mut p, b) = setup(&text);
        p.last_height = 3;
        p.page_down(&b, true);
        assert!(p.has_selection());
        assert_eq!(p.cursor.line, 3);
    }

    #[test]
    fn gutter_width_has_floor_of_three() {
        assert_eq!(gutter_num_width(1), 3);
        assert_eq!(gutter_num_width(999), 3);
        assert_eq!(gutter_num_width(1000), 4);
        assert_eq!(gutter_num_width(12345), 5);
    }

    // --- search & go-to-line ----------------------------------------------

    /// The current match as an ordered (start, end) selection, for assertions.
    fn match_selection(p: &EditorPane) -> Option<((usize, usize), (usize, usize))> {
        p.ordered_selection()
    }

    #[test]
    fn find_matches_is_case_insensitive_and_finds_all() {
        let b = Buffer::from_str("Foo foo\nbar FOO");
        let m = find_matches(&b, "foo");
        assert_eq!(m, vec![(0, 0), (0, 4), (1, 4)]);
        assert!(find_matches(&b, "zzz").is_empty());
    }

    #[test]
    fn nearest_at_or_after_picks_first_following_match() {
        let m = vec![(0, 0), (1, 2), (3, 0)];
        assert_eq!(nearest_at_or_after(&m, (1, 0)), Some(1));
        assert_eq!(nearest_at_or_after(&m, (3, 0)), Some(2));
        assert_eq!(nearest_at_or_after(&m, (9, 9)), None);
    }

    #[test]
    fn search_update_selects_nearest_match_and_wraps() {
        let (mut p, b) = setup("foo\nbar foo\nfoo end");
        p.cursor.line = 1; // origin on line 1
        p.search_begin();
        p.search_update(&b, "foo");
        // nearest at/after (1,0) is the "foo" at (1,4)
        assert_eq!(match_selection(&p), Some(((1, 4), (1, 7))));
    }

    #[test]
    fn search_update_with_no_origin_match_wraps_to_first() {
        let (mut p, b) = setup("foo\nbar");
        p.cursor.line = 1; // nothing matches at/after line 1
        p.search_begin();
        p.search_update(&b, "foo");
        assert_eq!(match_selection(&p), Some(((0, 0), (0, 3))));
    }

    #[test]
    fn search_next_and_prev_wrap_around() {
        let (mut p, b) = setup("foo foo\nfoo");
        p.search_begin();
        p.search_update(&b, "foo"); // matches (0,0),(0,4),(1,0); current 0
        assert_eq!(match_selection(&p), Some(((0, 0), (0, 3))));
        p.search_next();
        assert_eq!(match_selection(&p), Some(((0, 4), (0, 7))));
        p.search_next();
        assert_eq!(match_selection(&p), Some(((1, 0), (1, 3))));
        p.search_next(); // wraps to first
        assert_eq!(match_selection(&p), Some(((0, 0), (0, 3))));
        p.search_prev(); // wraps back to last
        assert_eq!(match_selection(&p), Some(((1, 0), (1, 3))));
    }

    #[test]
    fn empty_query_and_no_match_restore_origin_without_selection() {
        let (mut p, b) = setup("foo\nbar");
        p.cursor.line = 1;
        p.search_begin();
        p.search_update(&b, "foo"); // jumps to (0,0)
        assert!(p.has_selection());
        p.search_update(&b, ""); // empty -> back to origin, no selection
        assert!(!p.has_selection());
        assert_eq!((p.cursor.line, p.cursor.col), (1, 0));
        p.search_update(&b, "zzz"); // no match -> origin, no selection
        assert!(!p.has_selection());
        assert_eq!((p.cursor.line, p.cursor.col), (1, 0));
    }

    #[test]
    fn search_cancel_restores_origin_commit_keeps_selection() {
        let (mut p, b) = setup("hello\nfoo here");
        p.cursor.line = 0;
        p.cursor.col = 2; // origin (0,2)
        p.search_begin();
        p.search_update(&b, "foo");
        assert_eq!(match_selection(&p), Some(((1, 0), (1, 3))));
        p.search_cancel();
        assert!(!p.has_selection());
        assert_eq!((p.cursor.line, p.cursor.col), (0, 2));

        // commit leaves the match selected
        p.search_begin();
        p.search_update(&b, "foo");
        p.search_commit();
        assert_eq!(match_selection(&p), Some(((1, 0), (1, 3))));
    }

    // --- undo / redo -------------------------------------------------------

    #[test]
    fn typed_run_undoes_and_redoes_as_one_group() {
        let (mut p, mut b) = setup("");
        for c in "hello".chars() {
            p.insert_char(&mut b, c);
        }
        assert_eq!(b.line_text(0), "hello");
        p.undo(&mut b);
        assert_eq!(b.line_text(0), ""); // whole run gone in one step
        p.redo(&mut b);
        assert_eq!(b.line_text(0), "hello");
    }

    #[test]
    fn deletion_is_its_own_undo_step() {
        let (mut p, mut b) = setup("");
        for c in "abc".chars() {
            p.insert_char(&mut b, c);
        }
        p.backspace(&mut b); // delete 'c'
        assert_eq!(b.line_text(0), "ab");
        p.undo(&mut b); // reverts only the deletion
        assert_eq!(b.line_text(0), "abc");
        p.undo(&mut b); // reverts the typed run
        assert_eq!(b.line_text(0), "");
    }

    #[test]
    fn line_breaks_split_groups() {
        let (mut p, mut b) = setup("");
        for c in "ab".chars() {
            p.insert_char(&mut b, c);
        }
        p.insert_newline(&mut b);
        for c in "cd".chars() {
            p.insert_char(&mut b, c);
        }
        assert_eq!(b.line_text(0), "ab");
        assert_eq!(b.line_text(1), "cd");
        p.undo(&mut b); // remove "cd"
        assert_eq!(b.line_text(1), "");
        p.undo(&mut b); // remove the newline
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.line_text(0), "ab");
        p.undo(&mut b); // remove "ab"
        assert_eq!(b.line_text(0), "");
    }

    #[test]
    fn selection_replace_is_one_group() {
        let (mut p, mut b) = setup("hello");
        p.move_right(&b, true);
        p.move_right(&b, true);
        p.move_right(&b, true); // select "hel"
        p.insert_char(&mut b, 'H'); // type over the selection -> "Hlo"
        assert_eq!(b.line_text(0), "Hlo");
        p.undo(&mut b); // single step restores the original
        assert_eq!(b.line_text(0), "hello");
    }

    #[test]
    fn new_edit_after_undo_discards_redo() {
        let (mut p, mut b) = setup("");
        for c in "abc".chars() {
            p.insert_char(&mut b, c);
        }
        p.undo(&mut b);
        assert_eq!(b.line_text(0), "");
        p.insert_char(&mut b, 'x'); // diverge
        p.redo(&mut b); // nothing to redo
        assert_eq!(b.line_text(0), "x");
    }

    #[test]
    fn undo_redo_restore_cursor_and_clear_selection() {
        let (mut p, mut b) = setup("");
        for c in "hi".chars() {
            p.insert_char(&mut b, c);
        }
        // move away and start a selection to prove undo restores both
        p.move_line_start(&b, false);
        p.move_right(&b, true);
        assert!(p.has_selection());
        p.undo(&mut b);
        assert_eq!(b.line_text(0), "");
        assert!(!p.has_selection());
        assert_eq!((p.cursor.line, p.cursor.col), (0, 0)); // cursor_before
        p.redo(&mut b);
        assert_eq!(b.line_text(0), "hi");
        assert_eq!((p.cursor.line, p.cursor.col), (0, 2)); // cursor_after
    }

    #[test]
    fn undo_redo_on_empty_stacks_are_noops() {
        let (mut p, mut b) = setup("seed");
        p.undo(&mut b); // nothing recorded yet
        assert_eq!(b.line_text(0), "seed");
        p.redo(&mut b);
        assert_eq!(b.line_text(0), "seed");
        assert_eq!((p.cursor.line, p.cursor.col), (0, 0));
    }

    #[test]
    fn goto_line_jumps_and_clamps() {
        let (mut p, b) = setup("a\nb\nc\nd");
        p.goto_line(&b, 3);
        assert_eq!((p.cursor.line, p.cursor.col), (2, 0));
        p.goto_line(&b, 999); // clamp to last line
        assert_eq!(p.cursor.line, 3);
        p.goto_line(&b, 1);
        assert_eq!(p.cursor.line, 0);
    }

    #[test]
    fn set_cursor_clamps_line_and_col() {
        let (mut p, b) = setup("ab\ncdef\ng");
        p.set_cursor(&b, 1, 3);
        assert_eq!((p.cursor.line, p.cursor.col), (1, 3));
        // Line past the end lands on the last line; col past the end lands on
        // that line's length.
        p.set_cursor(&b, 999, 999);
        assert_eq!((p.cursor.line, p.cursor.col), (2, 1));
        assert_eq!(p.cursor.target_col, 1);
    }

    #[test]
    fn set_cursor_clears_selection() {
        let (mut p, b) = setup("hello\nworld");
        p.move_right(&b, true); // start a selection
        assert!(p.has_selection());
        p.set_cursor(&b, 1, 0);
        assert!(!p.has_selection());
    }

    // --- multi-cursor ------------------------------------------------------

    /// A secondary caret at `(line, col)` with no selection.
    fn caret_at(line: usize, col: usize) -> Caret {
        Caret {
            cursor: Cursor {
                line,
                col,
                target_col: col,
            },
            anchor: None,
        }
    }

    #[test]
    fn add_caret_below_and_above_place_carets_and_stop_at_edges() {
        let (mut p, b) = setup("aaa\nbbb\nccc");
        p.cursor.col = 1;
        p.cursor.target_col = 1;
        p.add_caret_below(&b);
        p.add_caret_below(&b);
        assert_eq!(p.secondary.len(), 2);
        assert_eq!(
            (p.secondary[0].cursor.line, p.secondary[0].cursor.col),
            (1, 1)
        );
        assert_eq!(
            (p.secondary[1].cursor.line, p.secondary[1].cursor.col),
            (2, 1)
        );
        // below the last line is a no-op
        p.add_caret_below(&b);
        assert_eq!(p.secondary.len(), 2);
    }

    #[test]
    fn add_caret_above_is_a_noop_at_the_top() {
        let (mut p, b) = setup("aaa\nbbb");
        p.cursor.line = 0;
        p.add_caret_above(&b);
        assert!(p.secondary.is_empty());
    }

    #[test]
    fn caret_clamps_column_to_a_short_line() {
        let (mut p, b) = setup("longline\nab");
        p.cursor.col = 6;
        p.cursor.target_col = 6;
        p.add_caret_below(&b); // line "ab" is only 2 long
        assert_eq!(
            (p.secondary[0].cursor.line, p.secondary[0].cursor.col),
            (1, 2)
        );
        assert_eq!(p.secondary[0].cursor.target_col, 6); // remembered
    }

    #[test]
    fn typing_fans_out_to_carets_on_different_lines() {
        let (mut p, mut b) = setup("aaa\nbbb\nccc");
        p.cursor.col = 0;
        p.add_caret_below(&b);
        p.add_caret_below(&b);
        p.insert_char(&mut b, 'X');
        assert_eq!(b.line_text(0), "Xaaa");
        assert_eq!(b.line_text(1), "Xbbb");
        assert_eq!(b.line_text(2), "Xccc");
        assert_eq!(p.cursor.col, 1);
        assert!(p.secondary.iter().all(|c| c.cursor.col == 1));
    }

    #[test]
    fn typing_at_two_carets_on_the_same_line_is_ordered_correctly() {
        // The ordering guard: edits must apply low-to-high without corrupting
        // the not-yet-applied positions.
        let (mut p, mut b) = setup("abcd");
        p.cursor.col = 1;
        p.cursor.target_col = 1;
        p.secondary.push(caret_at(0, 3));
        p.insert_char(&mut b, '-');
        assert_eq!(b.line_text(0), "a-bc-d");
    }

    #[test]
    fn backspace_fans_out_to_all_carets() {
        let (mut p, mut b) = setup("aXa\nbXb");
        p.cursor.line = 0;
        p.cursor.col = 2;
        p.cursor.target_col = 2;
        p.secondary.push(caret_at(1, 2));
        p.backspace(&mut b);
        assert_eq!(b.line_text(0), "aa");
        assert_eq!(b.line_text(1), "bb");
    }

    #[test]
    fn newline_at_multiple_carets_splits_each_line() {
        let (mut p, mut b) = setup("ab\ncd");
        p.cursor.line = 0;
        p.cursor.col = 1;
        p.cursor.target_col = 1;
        p.secondary.push(caret_at(1, 1));
        p.insert_newline(&mut b);
        assert_eq!(b.line_count(), 4);
        assert_eq!(b.line_text(0), "a");
        assert_eq!(b.line_text(1), "b");
        assert_eq!(b.line_text(2), "c");
        assert_eq!(b.line_text(3), "d");
    }

    #[test]
    fn shift_move_extends_a_selection_at_every_caret() {
        let (mut p, b) = setup("aaaa\nbbbb");
        p.cursor.col = 0;
        p.add_caret_below(&b);
        p.move_right(&b, true);
        p.move_right(&b, true);
        assert!(p.has_selection());
        assert!(p.secondary[0].anchor.is_some());
        assert_eq!(p.cursor.col, 2);
        assert_eq!(p.secondary[0].cursor.col, 2);
    }

    #[test]
    fn collapse_drops_secondary_carets() {
        let (mut p, b) = setup("aa\nbb\ncc");
        p.add_caret_below(&b);
        p.add_caret_below(&b);
        assert!(p.has_secondary_carets());
        p.collapse_carets();
        assert!(!p.has_secondary_carets());
    }

    #[test]
    fn coincident_carets_merge_after_movement() {
        let (mut p, b) = setup("ab\ncd");
        p.cursor.line = 0;
        p.cursor.col = 1;
        p.cursor.target_col = 1;
        p.add_caret_below(&b); // secondary at (1,1)
        assert_eq!(p.secondary.len(), 1);
        // move all carets down: primary -> (1,1); the secondary is clamped at the
        // last line and stays (1,1), so they coincide and merge.
        p.move_down(&b, false);
        assert!(p.secondary.is_empty());
        assert_eq!((p.cursor.line, p.cursor.col), (1, 1));
    }
}
