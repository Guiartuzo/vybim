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

/// Background of the line the cursor is on (a subtle dark gray from the 256
/// palette, so it reads on most terminals without truecolor).
const CURRENT_LINE_BG: Color = Color::Indexed(236);

/// Background of selected text — a blue from the 256 palette, distinct from the
/// current-line tint so a selection clearly stands out.
const SELECTION_BG: Color = Color::Indexed(24);

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
    /// Height of the content region at the last render, used by page movement
    /// (which needs the viewport size, only known at render time).
    last_height: usize,
}

impl EditorPane {
    pub fn new(buffer_id: usize) -> Self {
        Self {
            buffer_id,
            cursor: Cursor::default(),
            anchor: None,
            scroll_row: 0,
            scroll_col: 0,
            last_height: 0,
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

    /// Move to column zero of the current line (Home).
    pub fn move_line_start(&mut self, _buffer: &Buffer, extend: bool) {
        self.pre_move(extend);
        self.cursor.col = 0;
        self.cursor.target_col = 0;
    }

    /// Move to the end of the current line (End).
    pub fn move_line_end(&mut self, buffer: &Buffer, extend: bool) {
        self.pre_move(extend);
        self.cursor.col = self.line_len(buffer, self.cursor.line);
        self.cursor.target_col = self.cursor.col;
    }

    /// Move up by roughly one viewport height (PageUp), keeping the target
    /// column. Uses the height cached at the last render.
    pub fn page_up(&mut self, buffer: &Buffer, extend: bool) {
        self.pre_move(extend);
        let page = self.page_rows();
        self.cursor.line = self.cursor.line.saturating_sub(page);
        self.cursor.col = self
            .cursor
            .target_col
            .min(self.line_len(buffer, self.cursor.line));
    }

    /// Move down by roughly one viewport height (PageDown), keeping the target
    /// column. Uses the height cached at the last render.
    pub fn page_down(&mut self, buffer: &Buffer, extend: bool) {
        self.pre_move(extend);
        let page = self.page_rows();
        self.cursor.line = (self.cursor.line + page).min(self.last_line(buffer));
        self.cursor.col = self
            .cursor
            .target_col
            .min(self.line_len(buffer, self.cursor.line));
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
                Style::new().fg(Color::White)
            } else {
                Style::new().fg(Color::DarkGray)
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
                    .set_style(row_rect, Style::new().bg(CURRENT_LINE_BG));
            }
        }

        // Paint the selection over the text (after the current-line tint so the
        // selected span wins where they overlap).
        if let Some((sel_start, sel_end)) = self.ordered_selection() {
            for row in 0..height {
                let line_idx = self.scroll_row + row;
                if line_idx < sel_start.0 || line_idx > sel_end.0 {
                    continue;
                }
                let start_col = if line_idx == sel_start.0 { sel_start.1 } else { 0 };
                // For lines fully inside the selection, extend one cell past the
                // end of line so the selected newline reads as highlighted.
                let end_col = if line_idx == sel_end.0 {
                    sel_end.1
                } else {
                    self.line_len(buffer, line_idx) + 1
                };
                let vis_start = start_col.max(self.scroll_col);
                if end_col <= vis_start {
                    continue;
                }
                let sx = content.x + (vis_start - self.scroll_col) as u16;
                let avail = content.width.saturating_sub(sx - content.x);
                let w = ((end_col - vis_start) as u16).min(avail);
                if w == 0 {
                    continue;
                }
                let rect = Rect::new(sx, content.y + row as u16, w, 1);
                frame
                    .buffer_mut()
                    .set_style(rect, Style::new().bg(SELECTION_BG));
            }
        }

        if focused {
            let cx = content.x + (self.cursor.col.saturating_sub(self.scroll_col)) as u16;
            let cy = content.y + (self.cursor.line.saturating_sub(self.scroll_row)) as u16;
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
        let text = (0..20).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
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
        let text = (0..10).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
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
}
