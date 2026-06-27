//! An editor pane: a view onto a [`Buffer`] (referenced by id) with its own
//! cursor, selection, and scroll offset.
//!
//! Panes do not own their buffer. The buffer lives in a central store on the
//! `App`, and each pane refers to it by `buffer_id`. Editing methods therefore
//! take the buffer as a parameter. This indirection lets two panes view one
//! buffer and avoids the `Rc<RefCell<>>` graph that traps Rust beginners.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use crate::buffer::Buffer;
use crate::syntax::Syntax;

/// Cursor position within the buffer. `target_col` remembers the column the
/// user "wants" so vertical movement across short lines doesn't lose it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,
    pub target_col: usize,
}

#[derive(Debug)]
pub struct EditorPane {
    pub buffer_id: usize,
    pub cursor: Cursor,
    /// Selection anchor (line, col). The selection spans from here to the
    /// cursor; `None` means no active selection.
    anchor: Option<(usize, usize)>,
    scroll_row: usize,
    scroll_col: usize,
}

impl EditorPane {
    pub fn new(buffer_id: usize) -> Self {
        Self {
            buffer_id,
            cursor: Cursor::default(),
            anchor: None,
            scroll_row: 0,
            scroll_col: 0,
        }
    }

    /// Point this pane at a different buffer, resetting the view state.
    pub fn set_buffer(&mut self, buffer_id: usize) {
        self.buffer_id = buffer_id;
        self.cursor = Cursor::default();
        self.anchor = None;
        self.scroll_row = 0;
        self.scroll_col = 0;
    }

    // --- queries -----------------------------------------------------------

    pub fn has_selection(&self) -> bool {
        self.anchor.is_some()
    }

    fn last_line(&self, buffer: &Buffer) -> usize {
        buffer.line_count() - 1
    }

    fn line_len(&self, buffer: &Buffer, line: usize) -> usize {
        buffer.line_len_chars(line)
    }

    /// The selection as an ordered (start, end) pair, if any.
    fn ordered_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        let cursor = (self.cursor.line, self.cursor.col);
        Some(if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        })
    }

    // --- movement ----------------------------------------------------------

    /// Manage the selection anchor for a movement: extend keeps/sets the
    /// anchor, a plain move collapses any selection.
    fn pre_move(&mut self, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some((self.cursor.line, self.cursor.col));
            }
        } else {
            self.anchor = None;
        }
    }

    pub fn move_left(&mut self, buffer: &Buffer, extend: bool) {
        self.pre_move(extend);
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
        } else if self.cursor.line > 0 {
            self.cursor.line -= 1;
            self.cursor.col = self.line_len(buffer, self.cursor.line);
        }
        self.cursor.target_col = self.cursor.col;
    }

    pub fn move_right(&mut self, buffer: &Buffer, extend: bool) {
        self.pre_move(extend);
        if self.cursor.col < self.line_len(buffer, self.cursor.line) {
            self.cursor.col += 1;
        } else if self.cursor.line < self.last_line(buffer) {
            self.cursor.line += 1;
            self.cursor.col = 0;
        }
        self.cursor.target_col = self.cursor.col;
    }

    pub fn move_up(&mut self, buffer: &Buffer, extend: bool) {
        self.pre_move(extend);
        if self.cursor.line > 0 {
            self.cursor.line -= 1;
            self.cursor.col = self
                .cursor
                .target_col
                .min(self.line_len(buffer, self.cursor.line));
        }
    }

    pub fn move_down(&mut self, buffer: &Buffer, extend: bool) {
        self.pre_move(extend);
        if self.cursor.line < self.last_line(buffer) {
            self.cursor.line += 1;
            self.cursor.col = self
                .cursor
                .target_col
                .min(self.line_len(buffer, self.cursor.line));
        }
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
        self.delete_selection(buffer);
        let idx = buffer.char_idx(self.cursor.line, self.cursor.col);
        let mut encoded = [0u8; 4];
        buffer.insert(idx, ch.encode_utf8(&mut encoded));
        self.cursor.col += 1;
        self.cursor.target_col = self.cursor.col;
    }

    pub fn insert_newline(&mut self, buffer: &mut Buffer) {
        self.delete_selection(buffer);
        let idx = buffer.char_idx(self.cursor.line, self.cursor.col);
        buffer.insert(idx, "\n");
        self.cursor.line += 1;
        self.cursor.col = 0;
        self.cursor.target_col = 0;
    }

    /// Delete the character before the cursor, joining lines at a line start.
    pub fn backspace(&mut self, buffer: &mut Buffer) {
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
        if self.delete_selection(buffer) {
            return;
        }
        let idx = buffer.char_idx(self.cursor.line, self.cursor.col);
        if idx < buffer.len_chars() {
            buffer.remove(idx, idx + 1);
        }
    }

    // --- rendering ---------------------------------------------------------

    /// Render this pane into `area`: the buffer's visible region plus a
    /// one-row status bar. Only the focused pane places the hardware cursor.
    /// `syntax` is `Some` when the buffer's language has a grammar.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        buffer: &Buffer,
        syntax: Option<&Syntax>,
        focused: bool,
    ) {
        let [content, status] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

        self.render_content(frame, content, buffer, syntax, focused);
        self.render_status(frame, status, buffer, focused);
    }

    fn render_content(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        buffer: &Buffer,
        syntax: Option<&Syntax>,
        focused: bool,
    ) {
        let height = area.height as usize;
        let width = area.width as usize;

        self.scroll_row = scroll_to_show(self.scroll_row, self.cursor.line, height);
        self.scroll_col = scroll_to_show(self.scroll_col, self.cursor.col, width);

        let mut lines: Vec<Line> = Vec::with_capacity(height);
        for row in 0..height {
            let line_idx = self.scroll_row + row;
            if line_idx >= buffer.line_count() {
                break;
            }
            let text = buffer.line_text(line_idx);
            let visible: String = text.chars().skip(self.scroll_col).collect();
            lines.push(highlight_line(&visible, syntax));
        }
        frame.render_widget(Paragraph::new(Text::from(lines)), area);

        if focused {
            let cx = area.x + (self.cursor.col.saturating_sub(self.scroll_col)) as u16;
            let cy = area.y + (self.cursor.line.saturating_sub(self.scroll_row)) as u16;
            frame.set_cursor_position((cx, cy));
        }
    }

    fn render_status(&self, frame: &mut Frame, area: Rect, buffer: &Buffer, focused: bool) {
        let name = buffer
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[No Name]".to_string());
        let dirty = if buffer.is_dirty() { " [+]" } else { "" };
        let selection = if self.has_selection() { "  SEL" } else { "" };
        let text = format!(
            " {name}{dirty}{selection}    Ln {}, Col {} ",
            self.cursor.line + 1,
            self.cursor.col + 1
        );
        // The focused pane's status bar stands out so it's clear where input goes.
        let style = if focused {
            Style::new().bg(Color::Blue).fg(Color::White)
        } else {
            Style::new().bg(Color::DarkGray).fg(Color::Gray)
        };
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
}
