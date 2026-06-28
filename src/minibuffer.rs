//! The minibuffer: a single reusable prompt rendered on the bottom row that
//! collects input for a feature (search, go-to-line, and — later — fuzzy
//! file-find). One prompt is active at a time; the mode decides its label and
//! what its input drives. The input is global here; per-feature result state
//! (e.g. search matches) lives with the focused pane.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Paragraph;

use crate::theme::Theme;

/// Which feature the minibuffer is currently driving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniMode {
    Search,
    GotoLine,
    Files,
}

#[derive(Debug)]
pub struct Minibuffer {
    /// The fixed label shown before the input (e.g. `/` or `Go to line: `).
    prompt: String,
    /// The text the user has typed so far.
    pub input: String,
    pub mode: MiniMode,
}

impl Minibuffer {
    pub fn search() -> Self {
        Self {
            prompt: "/".to_string(),
            input: String::new(),
            mode: MiniMode::Search,
        }
    }

    pub fn goto_line() -> Self {
        Self {
            prompt: "Go to line: ".to_string(),
            input: String::new(),
            mode: MiniMode::GotoLine,
        }
    }

    pub fn files() -> Self {
        Self {
            prompt: "> ".to_string(),
            input: String::new(),
            mode: MiniMode::Files,
        }
    }

    pub fn push(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// Draw `<prompt><input>` across `area` and return the screen column where
    /// the text cursor belongs (just past the input, clamped into the row).
    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) -> u16 {
        let text = format!("{}{}", self.prompt, self.input);
        let style = Style::new().bg(theme.prompt_bg).fg(theme.text);
        frame.render_widget(Paragraph::new(text).style(style), area);
        let col = (self.prompt.chars().count() + self.input.chars().count()) as u16;
        area.x + col.min(area.width.saturating_sub(1))
    }
}
