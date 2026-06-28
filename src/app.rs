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
use crate::terminal_area::TerminalArea;
use crate::terminal_pane::TerminalPane;

/// Width of the file-tree sidebar, in columns.
const SIDEBAR_WIDTH: u16 = 28;

/// Width of the docked terminal area, in columns. Fixed in v1 (resize is a
/// deliberate non-goal); the PTY resizes to whatever width it is given.
const TERMINAL_AREA_WIDTH: u16 = 60;

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
    KeyBinding { group: "Global", keys: "Ctrl+J", action: "Show / hide terminal", footer: Some("term") },
    KeyBinding { group: "Global", keys: "F1", action: "Toggle this help", footer: Some("help") },
    KeyBinding { group: "Editor", keys: "Arrows", action: "Move cursor", footer: None },
    KeyBinding { group: "Editor", keys: "Shift+move", action: "Extend selection", footer: None },
    KeyBinding { group: "Editor", keys: "Home / End", action: "Line start / end", footer: None },
    KeyBinding { group: "Editor", keys: "PageUp / PageDown", action: "Move by a screenful", footer: None },
    KeyBinding { group: "Editor", keys: "Ctrl+S", action: "Save", footer: Some("save") },
    KeyBinding { group: "Editor", keys: "Ctrl+E", action: "Split pane vertically", footer: Some("split") },
    KeyBinding { group: "Editor", keys: "Ctrl+\\", action: "Split pane (alias)", footer: None },
    KeyBinding { group: "Editor", keys: "Ctrl+W", action: "Close pane", footer: Some("close") },
    KeyBinding { group: "Editor", keys: "Alt+Left / Alt+Right", action: "Move focus between panes", footer: None },
    KeyBinding { group: "Terminal", keys: "Ctrl+T", action: "New terminal", footer: None },
    KeyBinding { group: "Terminal", keys: "Ctrl+PageUp / PageDown", action: "Switch terminal", footer: None },
    KeyBinding { group: "Terminal", keys: "Ctrl+W", action: "Close terminal", footer: None },
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
    Terminal,
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
    /// Side-by-side editor panes (vertical splits). The terminal is no longer a
    /// peer here — it lives in `terminal_area`.
    panes: Vec<EditorPane>,
    /// Index into `panes` of the editor pane receiving input.
    focused: usize,
    /// The docked terminal area: multiple terminals, toggled as one unit.
    terminal_area: TerminalArea,
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
            panes: vec![EditorPane::new(0)],
            focused: 0,
            terminal_area: TerminalArea::new(),
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

        // Sidebar on the left when visible; otherwise the main area fills the
        // whole body.
        let main = if self.sidebar_visible {
            let [sidebar, main] =
                Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)])
                    .areas(body);
            self.tree
                .render(frame, sidebar, self.focus == Focus::Sidebar);
            main
        } else {
            body
        };

        // Terminal area docked on the right when visible, separated from the
        // editors by a thin divider (the same style as inter-pane dividers).
        let pane_area = if self.terminal_area.is_visible() {
            let [editors, divider, term] = Layout::horizontal([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(TERMINAL_AREA_WIDTH),
            ])
            .areas(main);
            let div = Block::new()
                .borders(Borders::LEFT)
                .border_style(Style::new().fg(Color::DarkGray));
            frame.render_widget(div, divider);
            self.terminal_area
                .render(frame, term, self.focus == Focus::Terminal);
            editors
        } else {
            main
        };

        // Divide the pane area across editor panes, with a 1-column divider
        // between adjacent panes. Pane i lives at regions[i*2]; its divider (if
        // any) at regions[i*2 + 1].
        let n = self.panes.len();
        let mut constraints = Vec::with_capacity(n * 2);
        for i in 0..n {
            constraints.push(Constraint::Fill(1));
            if i + 1 < n {
                constraints.push(Constraint::Length(1));
            }
        }
        let regions = Layout::horizontal(constraints).split(pane_area);
        for (i, ed) in self.panes.iter_mut().enumerate() {
            let region = regions[i * 2];
            let focused = self.focus == Focus::Editor && i == self.focused;
            let buffer = &self.buffers[ed.buffer_id];
            let syntax = self.syntaxes[ed.buffer_id].as_ref();
            ed.render(frame, region, buffer, syntax, focused);
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
            KeyCode::Char('j') if ctrl => {
                self.toggle_terminal_area();
                return;
            }
            _ => {}
        }

        match self.focus {
            Focus::Sidebar => self.on_sidebar_key(key),
            Focus::Editor => self.on_pane_key(key),
            Focus::Terminal => self.on_terminal_key(key),
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
            KeyCode::Char('t') if ctrl => return self.spawn_terminal(),
            KeyCode::Char('w') if ctrl => return self.close_focused_pane(),
            KeyCode::Left if alt => return self.focus_prev(),
            KeyCode::Right if alt => return self.focus_next(),
            _ => {}
        }

        let ed = &mut self.panes[self.focused];
        let buffer = &mut self.buffers[ed.buffer_id];
        dispatch_editor(ed, buffer, key);
    }

    /// Keys while the terminal area is focused: area-management chords (new,
    /// cycle, close), otherwise forwarded to the active terminal's shell.
    fn on_terminal_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('t') if ctrl => return self.spawn_terminal(),
            KeyCode::Char('w') if ctrl => return self.close_active_terminal(),
            KeyCode::PageUp if ctrl => return self.terminal_area.cycle_prev(),
            KeyCode::PageDown if ctrl => return self.terminal_area.cycle_next(),
            _ => {}
        }

        if let Some(term) = self.terminal_area.active_mut() {
            term.send_key(key);
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
            self.panes[self.focused].set_buffer(id);
            self.focus = Focus::Editor;
        }
    }

    // --- pane management ---------------------------------------------------

    /// Split the focused editor pane, placing a new pane viewing the same
    /// buffer beside it.
    fn split_vertical(&mut self) {
        let buffer_id = self.panes[self.focused].buffer_id;
        self.panes.insert(self.focused + 1, EditorPane::new(buffer_id));
        self.focused += 1;
    }

    /// Close the focused editor pane, unless it is the last one.
    fn close_focused_pane(&mut self) {
        if self.panes.len() <= 1 {
            return;
        }
        self.panes.remove(self.focused);
        self.clamp_focus();
    }

    // --- terminal area -----------------------------------------------------

    /// Show or hide the whole terminal area without killing any shell. Showing
    /// it focuses the terminal (spawning a first one if the area is empty);
    /// hiding it returns focus to the editor.
    fn toggle_terminal_area(&mut self) {
        if self.terminal_area.is_visible() {
            self.terminal_area.hide();
            self.focus = Focus::Editor;
        } else if self.terminal_area.is_empty() {
            // Nothing to show yet — opening the area creates its first terminal.
            self.spawn_terminal();
        } else {
            self.terminal_area.show();
            self.focus = Focus::Terminal;
        }
    }

    /// Spawn a new terminal into the area, make it active, show the area, and
    /// move focus into it.
    fn spawn_terminal(&mut self) {
        let Some(tx) = self.event_tx.clone() else {
            return;
        };
        let id = self.next_terminal_id;
        if let Ok(term) = TerminalPane::spawn(id, tx) {
            self.next_terminal_id += 1;
            self.terminal_area.add(term);
            self.focus = Focus::Terminal;
        }
    }

    /// Close the active terminal; if it was the last one the area hides itself,
    /// so return focus to the editor.
    fn close_active_terminal(&mut self) {
        if self.terminal_area.close_active() {
            self.focus = Focus::Editor;
        }
    }

    /// Deliver shell output to the matching terminal, even when the area is
    /// hidden — its grid stays current so showing it reveals up-to-date output.
    fn feed_terminal(&mut self, id: usize, bytes: &[u8]) {
        if let Some(term) = self.terminal_area.find_mut(id) {
            term.process(bytes);
        }
    }

    /// A terminal's shell exited: remove it from the area. If it was the last
    /// one the area hides itself, so return focus to the editor.
    fn on_terminal_exit(&mut self, id: usize) {
        if self.terminal_area.remove_by_id(id) && self.focus == Focus::Terminal {
            self.focus = Focus::Editor;
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

    /// The buffer id of editor pane `i`.
    fn buffer_id_at(app: &App, i: usize) -> usize {
        app.panes[i].buffer_id
    }

    /// Wire up an event sender so `spawn_terminal` can run (it spawns a real
    /// shell, as the terminal-pane tests already do). The returned receiver must
    /// be kept alive so the reader threads do not error out mid-test.
    fn with_terminals(app: &mut App) -> std::sync::mpsc::Receiver<AppEvent> {
        let (tx, rx) = mpsc::channel();
        app.event_tx = Some(tx);
        rx
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

    #[test]
    fn ctrl_j_toggles_terminal_area_and_focus() {
        let mut app = test_app();
        let _rx = with_terminals(&mut app);
        assert!(!app.terminal_area.is_visible());
        // first toggle: empty area spawns a terminal, shows it, focuses terminal
        app.on_key(press(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert!(app.terminal_area.is_visible());
        assert!(!app.terminal_area.is_empty());
        assert_eq!(app.focus, Focus::Terminal);
        // second toggle: hides the area (shell stays alive) and refocuses editor
        app.on_key(press(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert!(!app.terminal_area.is_visible());
        assert!(!app.terminal_area.is_empty());
        assert_eq!(app.focus, Focus::Editor);
        // third toggle: non-empty area just shows again, no new terminal
        app.on_key(press(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert!(app.terminal_area.is_visible());
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn ctrl_t_spawns_terminal_without_touching_editor_panes() {
        let mut app = test_app();
        let _rx = with_terminals(&mut app);
        app.split_vertical(); // two editor panes, focused = 1
        assert_eq!(app.panes.len(), 2);
        app.on_key(press(KeyCode::Char('t'), KeyModifiers::CONTROL));
        // editor panes are untouched; the terminal is the new focus
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.focused, 1);
        assert!(app.terminal_area.is_visible());
        assert_eq!(app.focus, Focus::Terminal);
    }

    #[test]
    fn closing_last_terminal_hides_area_and_refocuses_editor() {
        let mut app = test_app();
        let _rx = with_terminals(&mut app);
        app.spawn_terminal();
        assert!(app.terminal_area.is_visible());
        app.close_active_terminal();
        assert!(!app.terminal_area.is_visible());
        assert!(app.terminal_area.is_empty());
        assert_eq!(app.focus, Focus::Editor);
    }

    #[test]
    fn last_shell_exit_hides_area_and_refocuses_editor() {
        let mut app = test_app();
        let rx = with_terminals(&mut app);
        app.spawn_terminal();
        let id = app.next_terminal_id - 1;
        app.on_terminal_exit(id);
        assert!(!app.terminal_area.is_visible());
        assert!(app.terminal_area.is_empty());
        assert_eq!(app.focus, Focus::Editor);
        drop(rx);
    }

    #[test]
    fn toggling_terminal_leaves_editor_panes_unchanged() {
        let mut app = test_app();
        let _rx = with_terminals(&mut app);
        app.split_vertical(); // two panes, focused = 1
        let (n, focused) = (app.panes.len(), app.focused);
        app.toggle_terminal_area(); // show (spawns first terminal)
        app.toggle_terminal_area(); // hide
        assert_eq!(app.panes.len(), n);
        assert_eq!(app.focused, focused);
    }
}
