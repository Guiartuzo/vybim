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
use crate::complete::{Completion, MAX_CANDIDATES};
use crate::diff_view::DiffView;
use crate::file_tree::FileTree;
use crate::minibuffer::{MiniMode, Minibuffer};
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
    KeyBinding { group: "Global", keys: "Ctrl+D", action: "Show / hide diff view", footer: Some("diff") },
    KeyBinding { group: "Global", keys: "Ctrl+Shift+G", action: "Show / hide diff view (alias)", footer: None },
    KeyBinding { group: "Global", keys: "F1", action: "Toggle this help", footer: Some("help") },
    KeyBinding { group: "Editor", keys: "Arrows", action: "Move cursor", footer: None },
    KeyBinding { group: "Editor", keys: "Shift+move", action: "Extend selection", footer: None },
    KeyBinding { group: "Editor", keys: "Home / End", action: "Line start / end", footer: None },
    KeyBinding { group: "Editor", keys: "PageUp / PageDown", action: "Move by a screenful", footer: None },
    KeyBinding { group: "Editor", keys: "Ctrl+S", action: "Save", footer: Some("save") },
    KeyBinding { group: "Editor", keys: "Ctrl+Z", action: "Undo", footer: Some("undo") },
    KeyBinding { group: "Editor", keys: "Ctrl+Y", action: "Redo", footer: None },
    KeyBinding { group: "Editor", keys: "Ctrl+F", action: "Find", footer: Some("find") },
    KeyBinding { group: "Editor", keys: "Ctrl+G", action: "Go to line", footer: None },
    KeyBinding { group: "Editor", keys: "Ctrl+N", action: "Autocomplete word (Ctrl+Space alias)", footer: Some("complete") },
    KeyBinding { group: "Editor", keys: "Up / Down", action: "Completion: previous / next", footer: None },
    KeyBinding { group: "Editor", keys: "Tab / Enter", action: "Completion: accept", footer: None },
    KeyBinding { group: "Editor", keys: "Esc", action: "Completion: dismiss", footer: None },
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
    KeyBinding { group: "Diff view", keys: "Up / Down", action: "Select file / scroll diff", footer: None },
    KeyBinding { group: "Diff view", keys: "Enter / Right", action: "Enter the diff", footer: None },
    KeyBinding { group: "Diff view", keys: "Left", action: "Back to the file list", footer: None },
    KeyBinding { group: "Diff view", keys: "n / p", action: "Next / previous change", footer: None },
    KeyBinding { group: "Diff view", keys: "r", action: "Refresh", footer: None },
    KeyBinding { group: "Diff view", keys: "Esc", action: "Close the diff view", footer: None },
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
    Minibuffer,
    Diff,
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
    /// The active minibuffer prompt (search / go-to-line), if any.
    minibuffer: Option<Minibuffer>,
    /// The diff view, if open. While `Some` it claims the body and captures
    /// input (read-only — it never edits a buffer).
    diff: Option<DiffView>,
    /// The active completion popup, if any. Semi-modal: while open it intercepts
    /// only navigate/accept/dismiss keys; other keys edit and re-query it.
    completion: Option<Completion>,
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
            minibuffer: None,
            diff: None,
            completion: None,
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

        // The diff view is modal: while open it claims the whole body (the
        // footer still renders), so the editor layout below is skipped.
        if let Some(diff) = self.diff.as_mut() {
            diff.render(frame, body);
            render_footer(frame, footer);
            return;
        }

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

        // The completion popup is anchored to the focused pane's cursor.
        if self.focus == Focus::Editor
            && let Some(comp) = &self.completion
            && let Some(anchor) = self.panes[self.focused].cursor_screen()
        {
            render_completion_popup(frame, anchor, comp);
        }

        // The minibuffer prompt takes over the bottom row while open, and owns
        // the hardware cursor (the focused pane suppresses its own cursor then).
        if let Some(mini) = &self.minibuffer {
            let cx = mini.render(frame, footer);
            frame.set_cursor_position((cx, footer.y));
        } else {
            render_footer(frame, footer);
        }

        if self.help_visible {
            render_help_overlay(frame);
        }
    }

    /// Handle a key press. Truly global chords (quit, toggle sidebar) are
    /// handled first; the rest are routed by which region has focus.
    fn on_key(&mut self, key: KeyEvent) {
        // The minibuffer is modal: while a prompt is open it captures every key
        // (Enter commits, Esc cancels) so nothing edits or toggles underneath.
        if self.minibuffer.is_some() {
            self.on_minibuffer_key(key);
            return;
        }
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

        // The diff view is modal: while open, quit still works and the toggle
        // closes it; every other key is routed to the (read-only) view.
        if self.diff.is_some() {
            if ctrl && key.code == KeyCode::Char('q') {
                self.should_quit = true;
            } else if is_diff_toggle(&key) || key.code == KeyCode::Esc {
                self.close_diff_view();
            } else {
                self.on_diff_key(key);
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') if ctrl => {
                self.should_quit = true;
                return;
            }
            _ if is_diff_toggle(&key) => {
                self.open_diff_view();
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
            // Both handled at the top of on_key while their surface is open.
            Focus::Minibuffer | Focus::Diff => {}
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

        // While the completion popup is open it is semi-modal: navigate, accept,
        // and dismiss are intercepted here — before `dispatch_editor` — so `Tab`
        // and `Enter` accept the selection instead of inserting spaces/newlines.
        if let Some(comp) = self.completion.as_mut() {
            match key.code {
                KeyCode::Up => return comp.move_up(),
                KeyCode::Down => return comp.move_down(),
                KeyCode::Tab | KeyCode::Enter => return self.completion_accept(),
                KeyCode::Esc => return self.completion_dismiss(),
                _ => {}
            }
        }

        match key.code {
            // Primary split chord. `ctrl+e` is layout-independent and reachable
            // (e.g. on ABNT, where `ctrl+\` is impractical); `ctrl+\` stays as
            // a secondary alias.
            KeyCode::Char('e') if ctrl => return self.split_vertical(),
            KeyCode::Char('\\') if ctrl => return self.split_vertical(),
            KeyCode::Char('t') if ctrl => return self.spawn_terminal(),
            KeyCode::Char('w') if ctrl => return self.close_focused_pane(),
            KeyCode::Char('f') if ctrl => return self.open_search(),
            KeyCode::Char('g') if ctrl => return self.open_goto_line(),
            KeyCode::Left if alt => return self.focus_prev(),
            KeyCode::Right if alt => return self.focus_next(),
            // Autocomplete trigger: `Ctrl+N` (reliable) plus a best-effort
            // `Ctrl+Space` alias (reported as `Char(' ')`+Ctrl or `Null`).
            KeyCode::Char('n') if ctrl => return self.open_completion(),
            KeyCode::Char(' ') if ctrl => return self.open_completion(),
            KeyCode::Null => return self.open_completion(),
            _ => {}
        }

        let ed = &mut self.panes[self.focused];
        let buffer = &mut self.buffers[ed.buffer_id];
        dispatch_editor(ed, buffer, key);

        // After a pass-through editing key, re-query the popup so the list tracks
        // the buffer; it closes itself when the prefix no longer matches.
        if self.completion.is_some() {
            self.completion_requery();
        }
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
        self.completion = None;
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
        self.completion = None;
        let buffer_id = self.panes[self.focused].buffer_id;
        self.panes.insert(self.focused + 1, EditorPane::new(buffer_id));
        self.focused += 1;
    }

    /// Close the focused editor pane, unless it is the last one.
    fn close_focused_pane(&mut self) {
        if self.panes.len() <= 1 {
            return;
        }
        self.completion = None;
        self.panes.remove(self.focused);
        self.clamp_focus();
    }

    // --- terminal area -----------------------------------------------------

    /// Show or hide the whole terminal area without killing any shell. Showing
    /// it focuses the terminal (spawning a first one if the area is empty);
    /// hiding it returns focus to the editor.
    fn toggle_terminal_area(&mut self) {
        self.completion = None;
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
        self.completion = None;
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

    // --- minibuffer --------------------------------------------------------

    /// Open the search prompt against the focused pane, saving its cursor as the
    /// search origin so a cancel can return there.
    fn open_search(&mut self) {
        self.completion = None;
        self.panes[self.focused].search_begin();
        self.minibuffer = Some(Minibuffer::search());
        self.focus = Focus::Minibuffer;
    }

    /// Open the go-to-line prompt against the focused pane.
    fn open_goto_line(&mut self) {
        self.completion = None;
        self.minibuffer = Some(Minibuffer::goto_line());
        self.focus = Focus::Minibuffer;
    }

    /// Handle a key while the minibuffer prompt is open. Enter commits, Esc
    /// cancels; everything else edits the input (and, in search mode, drives
    /// incremental matching and next/previous navigation).
    fn on_minibuffer_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let mode = self.minibuffer.as_ref().map(|m| m.mode);

        match key.code {
            KeyCode::Esc => return self.close_minibuffer_cancel(),
            KeyCode::Enter => return self.close_minibuffer_commit(),
            KeyCode::Backspace => {
                if let Some(m) = self.minibuffer.as_mut() {
                    m.backspace();
                }
                return self.minibuffer_input_changed();
            }
            _ => {}
        }

        // Search-only navigation between matches.
        if mode == Some(MiniMode::Search) {
            match key.code {
                KeyCode::Down => return self.panes[self.focused].search_next(),
                KeyCode::Up => return self.panes[self.focused].search_prev(),
                KeyCode::Char('f') if ctrl => return self.panes[self.focused].search_next(),
                _ => {}
            }
        }

        if let KeyCode::Char(c) = key.code
            && !ctrl
            && !alt
        {
            // Go-to-line only accepts digits; search accepts any character.
            let accept = mode != Some(MiniMode::GotoLine) || c.is_ascii_digit();
            if accept {
                if let Some(m) = self.minibuffer.as_mut() {
                    m.push(c);
                }
                self.minibuffer_input_changed();
            }
        }
    }

    /// Re-run incremental search against the focused pane after the query
    /// changed. No-op for go-to-line, which acts only on commit.
    fn minibuffer_input_changed(&mut self) {
        let Some(m) = self.minibuffer.as_ref() else {
            return;
        };
        if m.mode == MiniMode::Search {
            let query = m.input.clone();
            let ed = &mut self.panes[self.focused];
            let buffer = &self.buffers[ed.buffer_id];
            ed.search_update(buffer, &query);
        }
    }

    /// Commit the prompt: apply its action and return focus to the editor.
    fn close_minibuffer_commit(&mut self) {
        if let Some(m) = self.minibuffer.take() {
            match m.mode {
                MiniMode::Search => self.panes[self.focused].search_commit(),
                MiniMode::GotoLine => {
                    if let Ok(n) = m.input.trim().parse::<usize>()
                        && n >= 1
                    {
                        let ed = &mut self.panes[self.focused];
                        let buffer = &self.buffers[ed.buffer_id];
                        ed.goto_line(buffer, n);
                    }
                }
            }
        }
        self.focus = Focus::Editor;
    }

    /// Cancel the prompt: undo any in-progress effect and refocus the editor.
    fn close_minibuffer_cancel(&mut self) {
        if let Some(m) = self.minibuffer.take()
            && m.mode == MiniMode::Search
        {
            self.panes[self.focused].search_cancel();
        }
        self.focus = Focus::Editor;
    }

    // --- autocomplete ------------------------------------------------------

    /// Open the completion popup for the word before the focused pane's cursor.
    /// A no-op (leaves it closed) when there is no prefix or nothing matches.
    fn open_completion(&mut self) {
        let ed = &self.panes[self.focused];
        let buffer = &self.buffers[ed.buffer_id];
        let cursor = (ed.cursor.line, ed.cursor.col);
        self.completion = Completion::open(buffer, cursor);
    }

    /// Re-gather candidates for the focused pane's current cursor, closing the
    /// popup when the cursor has left the word or nothing matches.
    fn completion_requery(&mut self) {
        let ed = &self.panes[self.focused];
        let buffer = &self.buffers[ed.buffer_id];
        let cursor = (ed.cursor.line, ed.cursor.col);
        let keep = self
            .completion
            .as_mut()
            .is_some_and(|c| c.requery(buffer, cursor));
        if !keep {
            self.completion = None;
        }
    }

    /// Accept the selected candidate: swap the typed prefix for the full word and
    /// close the popup.
    fn completion_accept(&mut self) {
        if let Some(comp) = self.completion.take()
            && let Some(word) = comp.selected_word()
        {
            let ed = &mut self.panes[self.focused];
            let buffer = &mut self.buffers[ed.buffer_id];
            ed.complete_accept(buffer, comp.prefix_start, word);
        }
    }

    /// Dismiss the popup without changing the buffer.
    fn completion_dismiss(&mut self) {
        self.completion = None;
    }

    // --- diff view ---------------------------------------------------------

    /// Open the diff view, snapshotting git state, and route input to it.
    fn open_diff_view(&mut self) {
        self.completion = None;
        self.diff = Some(DiffView::open());
        self.focus = Focus::Diff;
    }

    /// Close the diff view and return focus to the editor, buffers untouched.
    fn close_diff_view(&mut self) {
        self.diff = None;
        self.focus = Focus::Editor;
    }

    /// Keys while the diff view is open (read-only). List focus: Up/Down select
    /// a file (its diff reloads), Enter/Right step into the diff. Diff focus:
    /// scroll, `n`/`p` jump between changes, Left returns to the list. `r`
    /// refreshes from git in either case.
    fn on_diff_key(&mut self, key: KeyEvent) {
        let Some(diff) = self.diff.as_mut() else {
            return;
        };
        if key.code == KeyCode::Char('r') {
            return diff.refresh();
        }
        if diff.focus_in_list() {
            match key.code {
                KeyCode::Up => diff.select_prev(),
                KeyCode::Down => diff.select_next(),
                KeyCode::Enter | KeyCode::Right => diff.enter_diff(),
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Up => diff.scroll_up(),
                KeyCode::Down => diff.scroll_down(),
                KeyCode::PageUp => diff.page_up(),
                KeyCode::PageDown => diff.page_down(),
                KeyCode::Char('n') => diff.next_hunk(),
                KeyCode::Char('p') => diff.prev_hunk(),
                KeyCode::Left => diff.back_to_list(),
                _ => {}
            }
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
        self.completion = None;
        self.focused = (self.focused + 1) % self.panes.len();
    }

    fn focus_prev(&mut self) {
        self.completion = None;
        self.focused = (self.focused + self.panes.len() - 1) % self.panes.len();
    }
}

/// Whether `key` is the diff-view toggle. `Ctrl+D` is the primary chord because
/// it is reliably delivered everywhere; `Ctrl+Shift+G` (VSCode's source-control
/// shortcut, "git" mnemonic) is an alias for terminals that support the Kitty
/// keyboard protocol — VTE/gnome-terminal can't distinguish it from `Ctrl+G`.
fn is_diff_toggle(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('g' | 'G') if ctrl && shift => true,
        KeyCode::Char('d') if ctrl && !shift => true,
        _ => false,
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
    keys.replace("Ctrl+", "^")
        .replace("Shift+", "⇧")
        .replace("Alt+", "M-")
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

/// Draw the completion popup as a bordered list anchored just below the cursor
/// (`anchor` is the cursor's screen position), flipping above when there is no
/// room below. Rows and width are bounded; the selected entry is highlighted.
fn render_completion_popup(frame: &mut Frame, anchor: (u16, u16), comp: &Completion) {
    let screen = frame.area();
    let rows = comp.candidates.len().min(MAX_CANDIDATES);
    if rows == 0 {
        return;
    }

    // Width fits the longest candidate; height fits the rows. Both include the
    // one-cell border on each side, and are clamped to the screen.
    let longest = comp
        .candidates
        .iter()
        .take(rows)
        .map(|c| c.chars().count())
        .max()
        .unwrap_or(0);
    let w = ((longest as u16).saturating_add(2)).min(screen.width).max(1);
    let h = ((rows as u16).saturating_add(2)).min(screen.height).max(1);

    let (ax, ay) = anchor;
    // Prefer below the cursor; flip above when the box would overflow the bottom.
    let bottom = screen.y + screen.height;
    let y = if ay + 1 + h <= bottom {
        ay + 1
    } else {
        ay.saturating_sub(h)
    };
    // Keep the box on screen horizontally.
    let max_x = screen.x + screen.width.saturating_sub(w);
    let x = ax.min(max_x);

    let lines: Vec<Line> = comp
        .candidates
        .iter()
        .take(rows)
        .enumerate()
        .map(|(i, cand)| {
            let style = if i == comp.selected {
                Style::new().bg(Color::Blue).fg(Color::White)
            } else {
                Style::new().fg(Color::Gray)
            };
            Line::styled(cand.clone(), style)
        })
        .collect();

    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::DarkGray));
    let area = Rect::new(x, y, w, h);
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
        KeyCode::Char('z') if ctrl => ed.undo(buffer),
        KeyCode::Char('y') if ctrl => ed.redo(buffer),
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

    fn app_with(text: &str) -> App {
        App::new(Buffer::from_str(text), std::env::temp_dir())
    }

    #[test]
    fn ctrl_f_opens_search_and_esc_restores_focus() {
        let mut app = app_with("alpha beta");
        app.on_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(app.focus, Focus::Minibuffer);
        assert!(app.minibuffer.is_some());
        // typing edits the query, not the buffer
        app.on_key(press(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(app.buffers[0].line_text(0), "alpha beta");
        // Esc cancels and refocuses the editor
        app.on_key(press(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Editor);
        assert!(app.minibuffer.is_none());
    }

    #[test]
    fn search_jumps_to_match_and_commit_keeps_focus() {
        let mut app = app_with("one two three two");
        app.on_key(press(KeyCode::Char('f'), KeyModifiers::CONTROL));
        for c in "two".chars() {
            app.on_key(press(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // the focused pane should now have the first "two" selected
        assert!(app.panes[0].has_selection());
        app.on_key(press(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Editor);
        assert!(app.minibuffer.is_none());
        // cursor sits at the matched text (col 4..7 -> "two")
        assert_eq!(app.panes[0].cursor.line, 0);
    }

    #[test]
    fn ctrl_g_goto_line_moves_cursor_and_clamps() {
        let mut app = app_with("l1\nl2\nl3\nl4");
        app.on_key(press(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(app.focus, Focus::Minibuffer);
        for c in "3".chars() {
            app.on_key(press(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(press(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Editor);
        assert_eq!(app.panes[0].cursor.line, 2);
    }

    #[test]
    fn ctrl_z_and_ctrl_y_undo_and_redo_focused_buffer() {
        let mut app = app_with("");
        for c in "abc".chars() {
            app.on_key(press(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.buffers[0].line_text(0), "abc");
        app.on_key(press(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.buffers[0].line_text(0), ""); // typed run undone as one step
        app.on_key(press(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(app.buffers[0].line_text(0), "abc");
    }

    #[test]
    fn goto_line_ignores_non_digit_input() {
        let mut app = app_with("l1\nl2");
        app.on_key(press(KeyCode::Char('g'), KeyModifiers::CONTROL));
        app.on_key(press(KeyCode::Char('x'), KeyModifiers::NONE)); // ignored
        app.on_key(press(KeyCode::Char('2'), KeyModifiers::NONE));
        assert_eq!(app.minibuffer.as_ref().unwrap().input, "2");
    }

    // --- autocomplete ------------------------------------------------------

    /// Move the focused pane's cursor to `(line, col)`.
    fn place_cursor(app: &mut App, line: usize, col: usize) {
        let p = &mut app.panes[app.focused];
        p.cursor.line = line;
        p.cursor.col = col;
        p.cursor.target_col = col;
    }

    /// Type a Ctrl+N trigger.
    fn trigger_completion(app: &mut App) {
        app.on_key(press(KeyCode::Char('n'), KeyModifiers::CONTROL));
    }

    #[test]
    fn completion_opens_with_matching_words() {
        // "al" before the cursor; alpha/alpine match.
        let mut app = app_with("alpha alpine al");
        place_cursor(&mut app, 0, 15); // end of the trailing "al"
        trigger_completion(&mut app);
        let comp = app.completion.as_ref().expect("popup should open");
        assert_eq!(comp.candidates, vec!["alpha".to_string(), "alpine".to_string()]);
        assert_eq!(comp.selected, 0);
    }

    #[test]
    fn completion_does_not_open_without_matches() {
        let mut app = app_with("alpha beta");
        place_cursor(&mut app, 0, 10); // after "beta", but it's the only word
        trigger_completion(&mut app);
        assert!(app.completion.is_none());
        assert_eq!(app.buffers[0].line_text(0), "alpha beta");
    }

    #[test]
    fn accept_replaces_prefix_and_positions_cursor() {
        let mut app = app_with("alpha al");
        place_cursor(&mut app, 0, 8); // end of trailing "al"
        trigger_completion(&mut app);
        assert!(app.completion.is_some());
        app.on_key(press(KeyCode::Enter, KeyModifiers::NONE)); // accept "alpha"
        assert_eq!(app.buffers[0].line_text(0), "alpha alpha");
        assert!(app.completion.is_none());
        // cursor sits at the end of the inserted word
        assert_eq!((app.panes[0].cursor.line, app.panes[0].cursor.col), (0, 11));
    }

    #[test]
    fn tab_accepts_and_does_not_insert_spaces() {
        let mut app = app_with("alpha al");
        place_cursor(&mut app, 0, 8);
        trigger_completion(&mut app);
        app.on_key(press(KeyCode::Tab, KeyModifiers::NONE)); // accept, not 4 spaces
        assert_eq!(app.buffers[0].line_text(0), "alpha alpha");
        assert!(app.completion.is_none());
    }

    #[test]
    fn enter_accepts_and_does_not_insert_newline() {
        let mut app = app_with("alpha al");
        place_cursor(&mut app, 0, 8);
        trigger_completion(&mut app);
        app.on_key(press(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.buffers[0].line_count(), 1); // no newline inserted
        assert_eq!(app.buffers[0].line_text(0), "alpha alpha");
    }

    #[test]
    fn dismiss_leaves_buffer_unchanged() {
        let mut app = app_with("alpha al");
        place_cursor(&mut app, 0, 8);
        trigger_completion(&mut app);
        app.on_key(press(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.completion.is_none());
        assert_eq!(app.buffers[0].line_text(0), "alpha al");
    }

    #[test]
    fn typing_narrows_and_backspace_widens_the_list() {
        let mut app = app_with("apple apricot ax ap");
        place_cursor(&mut app, 0, 19); // end of trailing "ap"
        trigger_completion(&mut app);
        assert_eq!(
            app.completion.as_ref().unwrap().candidates,
            vec!["apple".to_string(), "apricot".to_string()]
        );
        // type "p" -> prefix "app" -> only apple matches
        app.on_key(press(KeyCode::Char('p'), KeyModifiers::NONE));
        assert_eq!(
            app.completion.as_ref().unwrap().candidates,
            vec!["apple".to_string()]
        );
        // backspace -> prefix "ap" again -> both return
        app.on_key(press(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(
            app.completion.as_ref().unwrap().candidates,
            vec!["apple".to_string(), "apricot".to_string()]
        );
    }

    #[test]
    fn completion_closes_when_prefix_stops_matching() {
        let mut app = app_with("apple ap");
        place_cursor(&mut app, 0, 8);
        trigger_completion(&mut app);
        assert!(app.completion.is_some());
        // type "z" -> prefix "apz" matches nothing -> popup closes
        app.on_key(press(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(app.completion.is_none());
        assert_eq!(app.buffers[0].line_text(0), "apple apz"); // edit still applied
    }

    #[test]
    fn completion_closes_when_cursor_leaves_the_word() {
        let mut app = app_with("alpha al");
        place_cursor(&mut app, 0, 8);
        trigger_completion(&mut app);
        assert!(app.completion.is_some());
        // moving left within the word is fine, but moving before the prefix
        // start closes it; here move left twice to leave the "al".
        app.on_key(press(KeyCode::Left, KeyModifiers::NONE));
        app.on_key(press(KeyCode::Left, KeyModifiers::NONE));
        assert!(app.completion.is_none());
    }

    #[test]
    fn moving_selection_changes_the_accepted_word() {
        let mut app = app_with("alpha alpine al");
        place_cursor(&mut app, 0, 15);
        trigger_completion(&mut app);
        app.on_key(press(KeyCode::Down, KeyModifiers::NONE)); // select "alpine"
        app.on_key(press(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.buffers[0].line_text(0), "alpha alpine alpine");
    }
}
