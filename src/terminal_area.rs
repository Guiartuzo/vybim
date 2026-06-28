//! The docked terminal area: a single right-docked region that hosts multiple
//! integrated terminals as tabs, of which exactly one is active and displayed
//! at a time.
//!
//! Hiding the area does NOT tear down any shell — each terminal's reader thread
//! keeps feeding `process`, so its grid stays current and showing the area
//! again reveals up-to-date contents. This is the whole point of lifting the
//! terminal out of the editor pane row: the area is the unit that hides, and it
//! hides without killing.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::terminal_pane::TerminalPane;

#[derive(Debug, Default)]
pub struct TerminalArea {
    terminals: Vec<TerminalPane>,
    /// Index into `terminals` of the displayed terminal.
    active: usize,
    visible: bool,
}

impl TerminalArea {
    pub fn new() -> Self {
        Self::default()
    }

    // --- queries -----------------------------------------------------------

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_empty(&self) -> bool {
        self.terminals.is_empty()
    }

    // --- visibility --------------------------------------------------------

    pub fn show(&mut self) {
        self.visible = true;
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    // --- terminals ---------------------------------------------------------

    /// Add an already-spawned terminal, make it the active one, and show the
    /// area (creating a terminal always brings the area forward).
    pub fn add(&mut self, term: TerminalPane) {
        self.terminals.push(term);
        self.active = self.terminals.len() - 1;
        self.visible = true;
    }

    pub fn cycle_next(&mut self) {
        if !self.terminals.is_empty() {
            self.active = (self.active + 1) % self.terminals.len();
        }
    }

    pub fn cycle_prev(&mut self) {
        if !self.terminals.is_empty() {
            self.active = (self.active + self.terminals.len() - 1) % self.terminals.len();
        }
    }

    /// Close the active terminal (its shell is terminated by `TerminalPane`'s
    /// `Drop`). Returns `true` when the area is now empty, in which case it has
    /// hidden itself and the caller should return focus to the editor.
    pub fn close_active(&mut self) -> bool {
        if self.terminals.is_empty() {
            return true;
        }
        self.terminals.remove(self.active);
        self.after_removal()
    }

    /// Remove the terminal whose shell exited on its own. Returns `true` when
    /// the area is now empty (and has hidden itself).
    pub fn remove_by_id(&mut self, id: usize) -> bool {
        if let Some(idx) = self.terminals.iter().position(|t| t.id == id) {
            self.terminals.remove(idx);
            return self.after_removal();
        }
        false
    }

    /// Keep `active` in range after a removal; hide the area if it emptied.
    fn after_removal(&mut self) -> bool {
        if self.terminals.is_empty() {
            self.active = 0;
            self.visible = false;
            return true;
        }
        if self.active >= self.terminals.len() {
            self.active = self.terminals.len() - 1;
        }
        false
    }

    /// The terminal matching `id`, for routing PTY output.
    pub fn find_mut(&mut self, id: usize) -> Option<&mut TerminalPane> {
        self.terminals.iter_mut().find(|t| t.id == id)
    }

    /// The displayed terminal, for forwarding keystrokes.
    pub fn active_mut(&mut self) -> Option<&mut TerminalPane> {
        self.terminals.get_mut(self.active)
    }

    // --- rendering ---------------------------------------------------------

    /// Render the tab strip and the active terminal into `area`. The caller
    /// only invokes this when the area is visible; an empty area draws nothing.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        if self.terminals.is_empty() {
            return;
        }
        let [tabs, content] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
        self.render_tabs(frame, tabs, focused);
        self.terminals[self.active].render(frame, content, focused);
    }

    /// A thin tab strip numbering the terminals, with the active one marked.
    fn render_tabs(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let mut spans: Vec<Span> = Vec::with_capacity(self.terminals.len());
        for i in 0..self.terminals.len() {
            let label = format!(" {} ", i + 1);
            let style = if i == self.active {
                let bg = if focused { Color::Blue } else { Color::DarkGray };
                Style::new().bg(bg).fg(Color::White)
            } else {
                Style::new().fg(Color::Gray)
            };
            spans.push(Span::styled(label, style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppEvent;
    use std::sync::mpsc;

    /// A terminal area holding `n` freshly spawned terminals. Each spawns a real
    /// shell (as the terminal-pane tests already do); the receiver is kept alive
    /// so the reader threads do not error out mid-test.
    fn area_with(n: usize) -> (TerminalArea, mpsc::Receiver<AppEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut area = TerminalArea::new();
        for i in 0..n {
            let term = TerminalPane::spawn(i, tx.clone()).expect("spawn shell");
            area.add(term);
        }
        (area, rx)
    }

    #[test]
    fn add_makes_active_and_visible() {
        let (area, _rx) = area_with(1);
        assert!(area.is_visible());
        assert_eq!(area.active, 0);
        assert_eq!(area.terminals.len(), 1);
    }

    #[test]
    fn add_second_terminal_makes_it_active() {
        let (area, _rx) = area_with(2);
        assert_eq!(area.active, 1);
    }

    #[test]
    fn cycle_wraps_through_terminals() {
        let (mut area, _rx) = area_with(3);
        assert_eq!(area.active, 2);
        area.cycle_next(); // wraps to 0
        assert_eq!(area.active, 0);
        area.cycle_prev(); // wraps back to 2
        assert_eq!(area.active, 2);
    }

    #[test]
    fn close_active_with_others_picks_an_adjacent() {
        let (mut area, _rx) = area_with(2);
        assert_eq!(area.active, 1);
        let empty = area.close_active();
        assert!(!empty);
        assert!(area.is_visible());
        assert_eq!(area.terminals.len(), 1);
        assert_eq!(area.active, 0);
    }

    #[test]
    fn close_last_hides_the_area() {
        let (mut area, _rx) = area_with(1);
        let empty = area.close_active();
        assert!(empty);
        assert!(!area.is_visible());
        assert!(area.is_empty());
    }

    #[test]
    fn remove_by_id_of_last_hides_the_area() {
        let (mut area, _rx) = area_with(1);
        let id = area.terminals[0].id;
        let empty = area.remove_by_id(id);
        assert!(empty);
        assert!(!area.is_visible());
    }
}
