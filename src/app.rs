//! The central application state and the main render/event loop.
//!
//! [`App`] is the single owner of all application state: the central buffer
//! store, the panes (editor or terminal), the file tree, and focus.
//!
//! The loop is driven by a single [`AppEvent`] channel. A dedicated input
//! thread forwards keyboard events, and each terminal pane's reader thread
//! forwards shell output, into the same channel — so the UI wakes for both
//! without the main loop busy-spinning.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread;

use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};

use crate::buffer::Buffer;
use crate::file_tree::FileTree;
use crate::pane::EditorPane;
use crate::syntax::Syntax;
use crate::terminal::Tui;
use crate::terminal_pane::TerminalPane;

/// Width of the file-tree sidebar, in columns.
const SIDEBAR_WIDTH: u16 = 28;

/// An event delivered to the main loop, from input or from a terminal pane.
#[derive(Debug)]
pub enum AppEvent {
    Input(Event),
    PtyOutput(usize, Vec<u8>),
    PtyExit(usize),
}

/// Which region currently receives keyboard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Editor,
}

/// A pane in the editor area: a text editor or an integrated terminal.
#[derive(Debug)]
enum Pane {
    Editor(EditorPane),
    Terminal(TerminalPane),
}

/// Central application state — the single owner of everything NyxVim tracks.
#[derive(Debug)]
pub struct App {
    should_quit: bool,
    /// Central buffer store; editor panes reference entries by index.
    buffers: Vec<Buffer>,
    /// Per-buffer highlighter (parallel to `buffers`); `None` when the buffer's
    /// language has no bundled grammar.
    syntaxes: Vec<Option<Syntax>>,
    /// Side-by-side panes (vertical splits).
    panes: Vec<Pane>,
    /// Index into `panes` of the pane receiving input.
    focused: usize,
    tree: FileTree,
    focus: Focus,
    /// Sender for spawning terminal panes; set once the loop starts.
    event_tx: Option<Sender<AppEvent>>,
    /// Monotonic id source for terminal panes.
    next_terminal_id: usize,
}

impl App {
    /// Start with a single pane viewing `buffer` and a sidebar rooted at `root`.
    pub fn new(buffer: Buffer, root: impl AsRef<std::path::Path>) -> Self {
        let syntax = buffer.path().and_then(Syntax::for_path);
        Self {
            should_quit: false,
            buffers: vec![buffer],
            syntaxes: vec![syntax],
            panes: vec![Pane::Editor(EditorPane::new(0))],
            focused: 0,
            tree: FileTree::new(root),
            focus: Focus::Editor,
            event_tx: None,
            next_terminal_id: 0,
        }
    }

    /// Run the main loop until a quit is requested.
    ///
    /// Input and terminal output are multiplexed onto one channel; the loop
    /// blocks on the channel, so it idles without busy-spinning yet still wakes
    /// when a terminal pane produces output.
    pub fn run(&mut self, terminal: &mut Tui) -> io::Result<()> {
        let (tx, rx) = mpsc::channel();
        self.event_tx = Some(tx.clone());

        // Forward terminal input events onto the shared channel.
        thread::spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(AppEvent::Input(ev)).is_err() {
                    break;
                }
            }
        });

        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;

            match rx.recv() {
                Ok(ev) => self.handle_app_event(ev),
                Err(_) => break,
            }
            // Coalesce any already-queued events (e.g. bursty shell output)
            // before the next redraw.
            while let Ok(ev) = rx.try_recv() {
                self.handle_app_event(ev);
            }
        }
        Ok(())
    }

    fn handle_app_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => self.on_key(key),
            AppEvent::Input(_) => {}
            AppEvent::PtyOutput(id, bytes) => self.feed_terminal(id, &bytes),
            AppEvent::PtyExit(id) => self.on_terminal_exit(id),
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        // Sidebar on the left, the pane area filling the rest.
        let [sidebar, pane_area] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)])
                .areas(frame.area());

        self.tree
            .render(frame, sidebar, self.focus == Focus::Sidebar);

        // Divide the pane area evenly across panes (vertical splits).
        let constraints = vec![Constraint::Fill(1); self.panes.len()];
        let regions = Layout::horizontal(constraints).split(pane_area);
        for (i, pane) in self.panes.iter_mut().enumerate() {
            let focused = self.focus == Focus::Editor && i == self.focused;
            match pane {
                Pane::Editor(ed) => {
                    let buffer = &self.buffers[ed.buffer_id];
                    let syntax = self.syntaxes[ed.buffer_id].as_ref();
                    ed.render(frame, regions[i], buffer, syntax, focused);
                }
                Pane::Terminal(term) => term.render(frame, regions[i], focused),
            }
        }
    }

    /// Handle a key press. Truly global chords (quit, toggle sidebar) are
    /// handled first; the rest are routed by which region has focus.
    fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('q') if ctrl => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('b') if ctrl => {
                self.toggle_sidebar_focus();
                return;
            }
            _ => {}
        }

        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(key),
            Focus::Editor => self.on_pane_key(key),
        }
    }

    /// Keys while the sidebar is focused: navigate and open.
    fn on_sidebar_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.tree.select_prev(),
            KeyCode::Down => self.tree.select_next(),
            KeyCode::Left => self.tree.collapse_selected(),
            KeyCode::Right => self.tree.expand_selected(),
            KeyCode::Enter => {
                if let Some(path) = self.tree.activate() {
                    self.open_in_focused_pane(path);
                }
            }
            _ => {}
        }
    }

    /// Keys while the pane area is focused: pane-management chords, otherwise
    /// routed to the focused pane (editor or terminal).
    fn on_pane_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            KeyCode::Char('\\') if ctrl => return self.split_vertical(),
            KeyCode::Char('t') if ctrl => return self.open_terminal(),
            KeyCode::Char('w') if ctrl => return self.close_focused_pane(),
            KeyCode::Left if alt => return self.focus_prev(),
            KeyCode::Right if alt => return self.focus_next(),
            _ => {}
        }

        match &mut self.panes[self.focused] {
            Pane::Editor(ed) => {
                let buffer = &mut self.buffers[ed.buffer_id];
                dispatch_editor(ed, buffer, key);
            }
            Pane::Terminal(term) => term.send_key(key),
        }
    }

    // --- sidebar -----------------------------------------------------------

    fn toggle_sidebar_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sidebar => Focus::Editor,
            Focus::Editor => Focus::Sidebar,
        };
    }

    /// Open `path` into the focused pane (as a new buffer) and focus the editor.
    fn open_in_focused_pane(&mut self, path: PathBuf) {
        if let Ok(buffer) = Buffer::from_path(&path) {
            let syntax = buffer.path().and_then(Syntax::for_path);
            let id = self.buffers.len();
            self.buffers.push(buffer);
            self.syntaxes.push(syntax);
            match &mut self.panes[self.focused] {
                Pane::Editor(ed) => ed.set_buffer(id),
                // Opening a file replaces a focused terminal with an editor.
                pane => *pane = Pane::Editor(EditorPane::new(id)),
            }
            self.focus = Focus::Editor;
        }
    }

    // --- pane management ---------------------------------------------------

    /// Split the focused editor pane, placing a new pane viewing the same
    /// buffer beside it. No-op when a terminal is focused.
    fn split_vertical(&mut self) {
        if let Pane::Editor(ed) = &self.panes[self.focused] {
            let new_pane = Pane::Editor(EditorPane::new(ed.buffer_id));
            self.panes.insert(self.focused + 1, new_pane);
            self.focused += 1;
        }
    }

    /// Open a new integrated terminal pane beside the focused one.
    fn open_terminal(&mut self) {
        let Some(tx) = self.event_tx.clone() else {
            return;
        };
        let id = self.next_terminal_id;
        if let Ok(term) = TerminalPane::spawn(id, tx) {
            self.next_terminal_id += 1;
            self.panes.insert(self.focused + 1, Pane::Terminal(term));
            self.focused += 1;
        }
    }

    /// Close the focused pane, unless it is the last one.
    fn close_focused_pane(&mut self) {
        if self.panes.len() <= 1 {
            return;
        }
        self.panes.remove(self.focused);
        self.clamp_focus();
    }

    /// Deliver shell output to the matching terminal pane.
    fn feed_terminal(&mut self, id: usize, bytes: &[u8]) {
        for pane in &mut self.panes {
            if let Pane::Terminal(term) = pane {
                if term.id == id {
                    term.process(bytes);
                    return;
                }
            }
        }
    }

    /// A terminal's shell exited: remove its pane, keeping at least one pane.
    fn on_terminal_exit(&mut self, id: usize) {
        if let Some(idx) = self.panes.iter().position(
            |p| matches!(p, Pane::Terminal(t) if t.id == id),
        ) {
            self.panes.remove(idx);
            if self.panes.is_empty() {
                self.panes.push(Pane::Editor(EditorPane::new(0)));
            }
            self.clamp_focus();
        }
    }

    fn clamp_focus(&mut self) {
        if self.focused >= self.panes.len() {
            self.focused = self.panes.len() - 1;
        }
    }

    fn focus_next(&mut self) {
        self.focused = (self.focused + 1) % self.panes.len();
    }

    fn focus_prev(&mut self) {
        self.focused = (self.focused + self.panes.len() - 1) % self.panes.len();
    }
}

/// Route an editing/movement key to an editor pane and its buffer.
fn dispatch_editor(ed: &mut EditorPane, buffer: &mut Buffer, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let extend = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Char('s') if ctrl => {
            let _ = buffer.save();
        }
        KeyCode::Left => ed.move_left(buffer, extend),
        KeyCode::Right => ed.move_right(buffer, extend),
        KeyCode::Up => ed.move_up(buffer, extend),
        KeyCode::Down => ed.move_down(buffer, extend),
        KeyCode::Enter => ed.insert_newline(buffer),
        KeyCode::Backspace => ed.backspace(buffer),
        KeyCode::Delete => ed.delete_forward(buffer),
        KeyCode::Tab => {
            // Insert spaces so one character always equals one column.
            for _ in 0..4 {
                ed.insert_char(buffer, ' ');
            }
        }
        // Printable input: any char that isn't part of a Ctrl/Alt chord.
        KeyCode::Char(c) if !ctrl && !alt => ed.insert_char(buffer, c),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        App::new(Buffer::from_str("hello"), std::env::temp_dir())
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// The buffer id of an editor pane (panics if it is a terminal).
    fn buffer_id_at(app: &App, i: usize) -> usize {
        match &app.panes[i] {
            Pane::Editor(ed) => ed.buffer_id,
            _ => panic!("pane {i} is not an editor"),
        }
    }

    #[test]
    fn ctrl_q_requests_quit() {
        let mut app = test_app();
        assert!(!app.should_quit);
        app.on_key(press(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn plain_q_inserts_into_focused_pane() {
        let mut app = test_app();
        app.on_key(press(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!app.should_quit);
        assert_eq!(app.buffers[0].line_text(0), "qhello");
    }

    #[test]
    fn ctrl_b_toggles_sidebar_focus() {
        let mut app = test_app();
        assert_eq!(app.focus, Focus::Editor);
        app.on_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(app.focus, Focus::Sidebar);
        // while in the sidebar, plain chars do not edit the buffer
        app.on_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.buffers[0].line_text(0), "hello");
    }

    #[test]
    fn opening_a_file_adds_buffer_and_focuses_editor() {
        let dir = std::env::temp_dir().join(format!("nyxvim_open_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        std::fs::write(&file, "from disk").unwrap();

        let mut app = App::new(Buffer::from_str(""), &dir);
        app.focus = Focus::Sidebar;
        app.open_in_focused_pane(file);

        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.buffers.len(), 2);
        let id = buffer_id_at(&app, app.focused);
        assert_eq!(app.buffers[id].line_text(0), "from disk");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn split_adds_pane_and_focuses_it() {
        let mut app = test_app();
        app.split_vertical();
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.focused, 1);
        // both panes view the same buffer
        assert_eq!(buffer_id_at(&app, 0), buffer_id_at(&app, 1));
    }

    #[test]
    fn focus_cycles_through_panes() {
        let mut app = test_app();
        app.split_vertical(); // focused = 1
        app.focus_next(); // wraps to 0
        assert_eq!(app.focused, 0);
        app.focus_prev(); // wraps to 1
        assert_eq!(app.focused, 1);
    }

    #[test]
    fn close_pane_keeps_at_least_one() {
        let mut app = test_app();
        app.split_vertical();
        app.close_focused_pane();
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.focused, 0);
        // closing the last pane is a no-op
        app.close_focused_pane();
        assert_eq!(app.panes.len(), 1);
    }
}
