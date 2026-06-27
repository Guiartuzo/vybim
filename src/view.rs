//! An editor view over a [`Buffer`]: the cursor, the scroll offset, and the
//! logic that renders the visible region and keeps the cursor on screen.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;

use crate::buffer::Buffer;

/// Cursor position within the buffer. `target_col` remembers the column the
/// user "wants" so vertical movement across short lines doesn't lose it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Cursor {
    pub line: usize,
    pub col: usize,
    pub target_col: usize,
}

#[derive(Debug)]
pub struct EditorView {
    pub buffer: Buffer,
    pub cursor: Cursor,
    scroll_row: usize,
    scroll_col: usize,
}

impl EditorView {
    pub fn new(buffer: Buffer) -> Self {
        Self {
            buffer,
            cursor: Cursor::default(),
            scroll_row: 0,
            scroll_col: 0,
        }
    }

    /// Render the visible slice of the buffer into `area`, scrolling first so
    /// the cursor stays visible, then placing the hardware cursor.
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let height = area.height as usize;
        let width = area.width as usize;

        self.scroll_row = scroll_to_show(self.scroll_row, self.cursor.line, height);
        self.scroll_col = scroll_to_show(self.scroll_col, self.cursor.col, width);

        let mut lines: Vec<Line> = Vec::with_capacity(height);
        for row in 0..height {
            let line_idx = self.scroll_row + row;
            if line_idx >= self.buffer.line_count() {
                break;
            }
            let text = self.buffer.line_text(line_idx);
            let visible: String = text.chars().skip(self.scroll_col).collect();
            lines.push(Line::raw(visible));
        }
        frame.render_widget(Paragraph::new(Text::from(lines)), area);

        let cx = area.x + (self.cursor.col.saturating_sub(self.scroll_col)) as u16;
        let cy = area.y + (self.cursor.line.saturating_sub(self.scroll_row)) as u16;
        frame.set_cursor_position((cx, cy));
    }
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
        // viewport shows rows [scroll, scroll+20); cursor at 35 needs scroll 16
        assert_eq!(scroll_to_show(10, 35, 20), 16);
    }

    #[test]
    fn zero_viewport_is_a_noop() {
        assert_eq!(scroll_to_show(7, 100, 0), 7);
    }
}
