//! The text buffer: a [`ropey::Rope`] plus the file it came from and its dirty
//! state. The rope gives us cheap line indexing and O(log n) edits, which keeps
//! editing snappy even on large files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ropey::Rope;

#[derive(Debug)]
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    dirty: bool,
}

impl Buffer {
    /// An empty, unnamed buffer.
    pub fn empty() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            dirty: false,
        }
    }

    /// Load a file from disk into a buffer.
    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path)?;
        Ok(Self {
            rope: Rope::from_str(&text),
            path: Some(path.to_path_buf()),
            dirty: false,
        })
    }

    /// Number of lines. A trailing newline yields a final empty line, matching
    /// how editors display files.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines().max(1)
    }

    /// The display text of a line, without its trailing newline.
    pub fn line_text(&self, idx: usize) -> String {
        if idx >= self.rope.len_lines() {
            return String::new();
        }
        let line = self.rope.line(idx).to_string();
        line.trim_end_matches(['\n', '\r']).to_string()
    }

    /// Number of characters on a line, excluding the trailing newline.
    pub fn line_len_chars(&self, idx: usize) -> usize {
        self.line_text(idx).chars().count()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_has_one_line() {
        let b = Buffer::empty();
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.line_text(0), "");
        assert!(!b.is_dirty());
    }

    #[test]
    fn line_text_strips_newline() {
        let b = Buffer {
            rope: Rope::from_str("hello\nworld\n"),
            path: None,
            dirty: false,
        };
        assert_eq!(b.line_text(0), "hello");
        assert_eq!(b.line_text(1), "world");
        assert_eq!(b.line_len_chars(0), 5);
        // trailing newline produces a final empty line
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line_text(2), "");
    }
}
