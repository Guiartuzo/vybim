//! An integrated terminal pane: a real shell running on a pseudo-terminal,
//! its output parsed into a cell grid by `vt100` and rendered into the pane.
//!
//! A dedicated reader thread pumps the PTY's output into the app's event
//! channel so the UI wakes and redraws as the shell produces output, without
//! the main loop blocking or busy-spinning.

use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::thread;

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::AppEvent;

pub struct TerminalPane {
    pub id: usize,
    parser: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rows: u16,
    cols: u16,
}

impl std::fmt::Debug for TerminalPane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPane")
            .field("id", &self.id)
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .finish_non_exhaustive()
    }
}

impl TerminalPane {
    /// Spawn the user's shell on a new PTY. Output is delivered to `tx` tagged
    /// with `id`; an exit is signalled with [`AppEvent::PtyExit`].
    pub fn spawn(id: usize, tx: Sender<AppEvent>) -> std::io::Result<TerminalPane> {
        let rows = 24;
        let cols = 80;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(to_io)?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut cmd = CommandBuilder::new(shell);
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        let child = pair.slave.spawn_command(cmd).map_err(to_io)?;
        // The slave handle is no longer needed by us once the child holds it.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(to_io)?;
        let writer = pair.master.take_writer().map_err(to_io)?;

        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(AppEvent::PtyExit(id));
                        break;
                    }
                    Ok(n) => {
                        if tx.send(AppEvent::PtyOutput(id, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(AppEvent::PtyExit(id));
                        break;
                    }
                }
            }
        });

        Ok(TerminalPane {
            id,
            parser: vt100::Parser::new(rows, cols, 0),
            master: pair.master,
            writer,
            child,
            rows,
            cols,
        })
    }

    /// Feed raw shell output into the terminal grid.
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Resize the grid and PTY to match a pane region, if it changed.
    fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.parser.screen_mut().set_size(rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Translate a key press into terminal input bytes and send them to the
    /// shell.
    pub fn send_key(&mut self, key: KeyEvent) {
        if let Some(bytes) = key_to_bytes(key) {
            let _ = self.writer.write_all(&bytes);
            let _ = self.writer.flush();
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        self.resize(area.height, area.width);
        let screen = self.parser.screen();

        let mut lines: Vec<Line> = Vec::with_capacity(area.height as usize);
        for row in 0..area.height {
            let mut spans: Vec<Span> = Vec::new();
            let mut run = String::new();
            let mut run_style = Style::new();

            for col in 0..area.width {
                let (text, style) = match screen.cell(row, col) {
                    Some(cell) => {
                        let contents = cell.contents();
                        let ch = if contents.is_empty() {
                            " ".to_string()
                        } else {
                            contents.to_string()
                        };
                        (ch, cell_style(cell))
                    }
                    None => (" ".to_string(), Style::new()),
                };

                if style == run_style {
                    run.push_str(&text);
                } else {
                    if !run.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut run), run_style));
                    }
                    run = text;
                    run_style = style;
                }
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, run_style));
            }
            lines.push(Line::from(spans));
        }

        frame.render_widget(Text::from(lines), area);

        if focused && !screen.hide_cursor() {
            let (crow, ccol) = screen.cursor_position();
            frame.set_cursor_position((area.x + ccol, area.y + crow));
        }
    }
}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        // Terminate the shell when the pane is closed.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::new();
    if let Some(fg) = convert_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = convert_color(cell.bgcolor()) {
        style = style.bg(bg);
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn convert_color(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Translate a crossterm key event into the bytes a terminal expects.
fn key_to_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                // Control characters: Ctrl+A..Ctrl+Z map to 0x01..0x1a, etc.
                let upper = c.to_ascii_uppercase();
                if ('@'..='_').contains(&upper) {
                    return Some(vec![(upper as u8) - b'@']);
                }
            }
            let mut buf = [0u8; 4];
            Some(c.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        KeyCode::Enter => Some(b"\r".to_vec()),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        _ => None,
    }
}

fn to_io(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_translation_for_common_keys() {
        let k = |code, mods| KeyEvent::new(code, mods);
        assert_eq!(
            key_to_bytes(k(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(b"a".to_vec())
        );
        // Ctrl+C -> ETX (0x03)
        assert_eq!(
            key_to_bytes(k(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
        assert_eq!(
            key_to_bytes(k(KeyCode::Enter, KeyModifiers::NONE)),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            key_to_bytes(k(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_to_bytes(k(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(vec![0x7f])
        );
    }

    #[test]
    fn vt_colors_convert() {
        assert_eq!(convert_color(vt100::Color::Default), None);
        assert_eq!(convert_color(vt100::Color::Idx(4)), Some(Color::Indexed(4)));
        assert_eq!(
            convert_color(vt100::Color::Rgb(1, 2, 3)),
            Some(Color::Rgb(1, 2, 3))
        );
    }

    /// End-to-end: spawn a real shell, run a command, and confirm its output
    /// reaches the parsed screen via the reader thread + vt100 pipeline.
    #[test]
    fn shell_command_output_reaches_screen() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let (tx, rx) = mpsc::channel();
        let mut term = TerminalPane::spawn(0, tx).expect("spawn shell");
        term.resize(24, 80);

        for ch in "echo nyxhello".chars() {
            term.send_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        term.send_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(AppEvent::PtyOutput(_, bytes)) => {
                    term.process(&bytes);
                    if term.parser.screen().contents().contains("nyxhello") {
                        found = true;
                        break;
                    }
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(found, "expected shell echo output to appear on screen");
    }
}
