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
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::buffer::Buffer;
use crate::file_tree::FileTree;
use crate::pane::EditorPane;
use crate::syntax::Syntax;
use crate::terminal::Tui;
use crate::terminal_pane::TerminalPane;

/// Width of the file-tree sidebar, in columns.
const SIDEBAR_WIDTH: u16 = 28;

/// One keybinding, the single source of truth for both the footer hint and the
/// help overlay (so they can't drift). `footer` holds a terse label when the
/// binding should also appear in the bottom hint.
struct KeyBinding {
    group: &'static str,
    keys: &'static str,
    action: &'static str,
    footer: Option<&'static str>,
}

/// All user-facing keybindings, grouped. The overlay renders every entry; the
/// footer renders only those with a `footer` label.
const BINDINGS: &[KeyBinding] = &[
    KeyBinding { group: "Global", keys: "Ctrl+Q", action: "Quit", footer: Some("quit") },
    KeyBinding { group: "Global", keys: "Ctrl+B", action: "Show / hide sidebar", footer: Some("sidebar") },
    KeyBinding { group: "Global", keys: "F1", action: "Toggle this help", footer: Some("help") },
    KeyBinding { group: "Editor", keys: "Arrows", action: "Move cursor", footer: None },
    KeyBinding { group: "Editor", keys: "Shift+move", action: "Extend selection", footer: None },
    KeyBinding { group: "Editor", keys: "Home / End", action: "Line start / end", footer: None },
    KeyBinding { group: "Editor", keys: "PageUp / PageDown", action: "Move by a screenful", footer: None },
    KeyBinding { group: "Editor", keys: "Ctrl+S", action: "Save", footer: Some("save") },
    KeyBinding { group: "Editor", keys: "Ctrl+E", action: "Split pane vertically", footer: Some("split") },
    KeyBinding { group: "Editor", keys: "Ctrl+\\", action: "Split pane (alias)", footer: None },
    KeyBinding { group: "Editor", keys: "Ctrl+T", action: "Open terminal", footer: Some("term") },
    KeyBinding { group: "Editor", keys: "Ctrl+W", action: "Close pane", footer: Some("close") },
    KeyBinding { group: "Editor", keys: "Alt+Left / Alt+Right", action: "Move focus between panes", footer: None },
    KeyBinding { group: "Sidebar", keys: "Up / Down", action: "Move selection", footer: None },
    KeyBinding { group: "Sidebar", keys: "Left / Right", action: "Collapse / expand directory", footer: None },
    KeyBinding { group: "Sidebar", keys: "Enter", action: "Open file / toggle directory", footer: None },
];

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
/// The terminal is boxed because it is much larger than an editor pane.
#[derive(Debug)]
enum Pane {
    Editor(EditorPane),
    Terminal(Box<TerminalPane>),
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
    /// Whether the keybinding help overlay is currently shown.
    help_visible: bool,
    /// Whether the file-tree sidebar is shown.
    sidebar_visible: bool,
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
            help_visible: false,
            sidebar_visible: true,
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
        // A one-row keybinding hint sits at the very bottom; everything else
        // fills the body above it.
        let [body, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());

        // Sidebar on the left when visible; otherwise the pane area fills the
        // whole body.
        let pane_area = if self.sidebar_visible {
            let [sidebar, pane_area] =
                Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)])
                    .areas(body);
            self.tree
                .render(frame, sidebar, self.focus == Focus::Sidebar);
            pane_area
        } else {
            body
        };

        // Divide the pane area across panes, with a 1-column divider between
        // adjacent panes. Pane i lives at regions[i*2]; its divider (if any) at
        // regions[i*2 + 1].
        let n = self.panes.len();
        let mut constraints = Vec::with_capacity(n * 2);
        for i in 0..n {
            constraints.push(Constraint::Fill(1));
            if i + 1 < n {
                constraints.push(Constraint::Length(1));
            }
        }
        let regions = Layout::horizontal(constraints).split(pane_area);
        for (i, pane) in self.panes.iter_mut().enumerate() {
            let region = regions[i * 2];
            let focused = self.focus == Focus::Editor && i == self.focused;
            match pane {
                Pane::Editor(ed) => {
                    let buffer = &self.buffers[ed.buffer_id];
                    let syntax = self.syntaxes[ed.buffer_id].as_ref();
                    ed.render(frame, region, buffer, syntax, focused);
                }
                Pane::Terminal(term) => term.render(frame, region, focused),
            }
        }
        // Thin vertical lines between panes.
        for i in 0..n.saturating_sub(1) {
            let divider = Block::new()
                .borders(Borders::LEFT)
                .border_style(Style::new().fg(Color::DarkGray));
            frame.render_widget(divider, regions[i * 2 + 1]);
        }

        render_footer(frame, footer);

        if self.help_visible {
            render_help_overlay(frame);
        }
    }

    /// Handle a key press. Truly global chords (quit, toggle sidebar) are
    /// handled first; the rest are routed by which region has focus.
    fn on_key(&mut self, key: KeyEvent) {
        // The help overlay is modal: F1 toggles it, and while it is open every
        // other key is swallowed (Esc also closes) so nothing edits underneath.
        if key.code == KeyCode::F(1) {
            self.help_visible = !self.help_visible;
            return;
        }
        if self.help_visible {
            if key.code == KeyCode::Esc {
                self.help_visible = false;
            }
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('q') if ctrl => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('b') if ctrl => {
                self.toggle_sidebar();
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
            // Primary split chord. `ctrl+e` is layout-independent and reachable
            // (e.g. on ABNT, where `ctrl+\` is impractical); `ctrl+\` stays as
            // a secondary alias.
            KeyCode::Char('e') if ctrl => return self.split_vertical(),
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

    /// Show or hide the sidebar, moving focus automatically: showing it focuses
    /// the tree (ready to navigate); hiding it returns focus to the editor.
    fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        self.focus = if self.sidebar_visible {
            Focus::Sidebar
        } else {
            Focus::Editor
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
            self.panes
                .insert(self.focused + 1, Pane::Terminal(Box::new(term)));
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
            if let Pane::Terminal(term) = pane
                && term.id == id
            {
                term.process(bytes);
                return;
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

/// Render the bottom keybinding hint so the core chords are discoverable. Built
/// from [`BINDINGS`] so it can never drift from the help overlay.
fn render_footer(frame: &mut Frame, area: Rect) {
    let mut hint = String::new();
    for b in BINDINGS {
        if let Some(label) = b.footer {
            hint.push_str(&format!("  {} {} ", compact_keys(b.keys), label));
        }
    }
    let style = Style::new().bg(Color::DarkGray).fg(Color::White);
    frame.render_widget(Paragraph::new(hint).style(style), area);
}

/// Compact a key string for the narrow footer (`Ctrl+Q` -> `^Q`).
fn compact_keys(keys: &str) -> String {
    keys.replace("Ctrl+", "^").replace("Alt+", "M-")
}

/// Draw the keybinding help overlay centered over the screen. Modal: the caller
/// has already ensured input is swallowed while it is visible.
fn render_help_overlay(frame: &mut Frame) {
    let mut lines: Vec<Line> = Vec::new();
    let mut group = "";
    for b in BINDINGS {
        if b.group != group {
            if !group.is_empty() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(b.group, Style::new().fg(Color::Yellow)));
            group = b.group;
        }
        lines.push(Line::raw(format!("  {:<22} {}", b.keys, b.action)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Esc or F1 to close",
        Style::new().fg(Color::DarkGray),
    ));

    let height = lines.len() as u16 + 2; // + borders
    let area = centered_rect(frame.area(), 52, height);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Blue))
        .title(" NyxVim — Keybindings ");
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
}

/// A `width`x`height` rectangle centered within `area`, clamped to fit.
fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    Rect::new(x, y, w, h)
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
        KeyCode::Home => ed.move_line_start(buffer, extend),
        KeyCode::End => ed.move_line_end(buffer, extend),
        KeyCode::PageUp => ed.page_up(buffer, extend),
        KeyCode::PageDown => ed.page_down(buffer, extend),
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
    fn ctrl_b_toggles_sidebar_visibility_and_focus() {
        let mut app = test_app();
        assert!(app.sidebar_visible);
        assert_eq!(app.focus, Focus::Editor);
        // hide: focus returns to the editor
        app.on_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert!(!app.sidebar_visible);
        assert_eq!(app.focus, Focus::Editor);
        // show: focus moves to the tree, ready to navigate
        app.on_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert!(app.sidebar_visible);
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
    fn f1_toggles_help_and_swallows_input_while_open() {
        let mut app = test_app();
        assert!(!app.help_visible);
        app.on_key(press(KeyCode::F(1), KeyModifiers::NONE));
        assert!(app.help_visible);
        // while help is open, typing does not edit the buffer
        app.on_key(press(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.buffers[0].line_text(0), "hello");
        // Esc closes it
        app.on_key(press(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.help_visible);
        // F1 again toggles back on
        app.on_key(press(KeyCode::F(1), KeyModifiers::NONE));
        assert!(app.help_visible);
    }

    #[test]
    fn ctrl_e_splits_the_focused_pane() {
        let mut app = test_app();
        app.on_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.focused, 1);
    }

    #[test]
    fn ctrl_backslash_still_splits() {
        let mut app = test_app();
        app.on_key(press(KeyCode::Char('\\'), KeyModifiers::CONTROL));
        assert_eq!(app.panes.len(), 2);
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
