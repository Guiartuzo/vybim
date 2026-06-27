//! The central application state and the main render/event loop.
//!
//! [`App`] is the single owner of all application state. Later milestones add
//! buffers, panes, and the file tree here; for now it tracks only whether the
//! editor should quit.

use std::io;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::terminal::Tui;
use crate::view::EditorView;

/// Central application state — the single owner of everything NyxVim tracks.
#[derive(Debug)]
pub struct App {
    should_quit: bool,
    view: EditorView,
}

impl App {
    pub fn new(view: EditorView) -> Self {
        Self {
            should_quit: false,
            view,
        }
    }

    /// Run the main loop until a quit is requested.
    ///
    /// Each iteration draws the current state, then blocks waiting for the next
    /// input event. Blocking on input means an idle editor consumes no CPU and
    /// never redraws on its own.
    pub fn run(&mut self, terminal: &mut Tui) -> io::Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.view.render(frame, frame.area()))?;
            self.handle_events()?;
        }
        Ok(())
    }

    /// Block for the next event and update state accordingly.
    fn handle_events(&mut self) -> io::Result<()> {
        if let Event::Key(key) = event::read()? {
            // Only react to presses, not key-release/repeat events some
            // terminals emit, so a single keystroke does one thing.
            if key.kind == KeyEventKind::Press {
                self.on_key(key);
            }
        }
        Ok(())
    }

    /// Handle a key press. Global keys are handled here; later milestones route
    /// non-global keys to the focused component.
    fn on_key(&mut self, key: KeyEvent) {
        if is_quit(key) {
            self.should_quit = true;
        }
    }
}

/// The global quit chord: Ctrl+Q.
fn is_quit(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    fn test_app() -> App {
        App::new(EditorView::new(Buffer::empty()))
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_q_requests_quit() {
        let mut app = test_app();
        assert!(!app.should_quit);
        app.on_key(press(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn plain_q_does_not_quit() {
        let mut app = test_app();
        app.on_key(press(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!app.should_quit);
    }
}
