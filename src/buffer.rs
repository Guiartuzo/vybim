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

    /// A buffer from an in-memory string, with no backing file. Currently used
    /// by tests; kept as a general constructor.
    #[allow(dead_code)]
    pub fn from_str(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
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

    /// Total number of characters in the buffer.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// Convert a (line, column) position into an absolute character index.
    /// `col` is a count of characters into the line, excluding the newline.
    pub fn char_idx(&self, line: usize, col: usize) -> usize {
        self.rope.line_to_char(line) + col
    }

    /// Convert an absolute character index back into a (line, column) position.
    pub fn line_col(&self, char_idx: usize) -> (usize, usize) {
        let idx = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(idx);
        let col = idx - self.rope.line_to_char(line);
        (line, col)
    }

    /// Insert `text` at an absolute character index and mark the buffer dirty.
    pub fn insert(&mut self, char_idx: usize, text: &str) {
        self.rope.insert(char_idx, text);
        self.dirty = true;
    }

    /// Remove the half-open character range `[start, end)` and mark dirty.
    pub fn remove(&mut self, start: usize, end: usize) {
        if start < end {
            self.rope.remove(start..end);
            self.dirty = true;
        }
    }

    /// Write the buffer back to its file, clearing the dirty flag. Returns
    /// `Ok(false)` if the buffer has no associated path (nothing written).
    pub fn save(&mut self) -> io::Result<bool> {
        let Some(path) = self.path.clone() else {
            return Ok(false);
        };
        let file = fs::File::create(&path)?;
        self.rope.write_to(io::BufWriter::new(file))?;
        self.dirty = false;
        Ok(true)
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
        let b = Buffer::from_str("hello\nworld\n");
        assert_eq!(b.line_text(0), "hello");
        assert_eq!(b.line_text(1), "world");
        assert_eq!(b.line_len_chars(0), 5);
        // trailing newline produces a final empty line
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line_text(2), "");
    }

    #[test]
    fn char_idx_and_line_col_round_trip() {
        let b = Buffer::from_str("ab\ncde");
        assert_eq!(b.char_idx(0, 0), 0);
        assert_eq!(b.char_idx(1, 0), 3); // after "ab\n"
        assert_eq!(b.char_idx(1, 2), 5);
        assert_eq!(b.line_col(5), (1, 2));
        assert_eq!(b.line_col(0), (0, 0));
    }

    #[test]
    fn save_writes_file_and_clears_dirty() {
        use std::io::Read;
        let path =
            std::env::temp_dir().join(format!("nyxvim_save_test_{}.txt", std::process::id()));
        std::fs::write(&path, "old").unwrap();

        let mut b = Buffer::from_path(&path).unwrap();
        b.insert(b.len_chars(), "!");
        assert!(b.is_dirty());

        assert!(b.save().unwrap());
        assert!(!b.is_dirty());

        let mut written = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut written)
            .unwrap();
        assert_eq!(written, "old!");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_without_path_is_noop() {
        let mut b = Buffer::from_str("text");
        assert!(!b.save().unwrap());
    }
}
