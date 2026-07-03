//! The text buffer: a [`ropey::Rope`] plus the file it came from and its dirty
//! state. The rope gives us cheap line indexing and O(log n) edits, which keeps
//! editing snappy even on large files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ropey::Rope;

/// A single reversible primitive edit. `Insert` reverts by removing the same
/// span; `Remove` carries the removed text so it can be reinserted.
#[derive(Debug)]
enum EditOp {
    Insert { at: usize, text: String },
    Remove { at: usize, text: String },
}

/// One undo step: a run of contiguous primitive edits, plus the cursor char
/// index before the run (the undo target) and after it (the redo target).
#[derive(Debug)]
struct EditGroup {
    ops: Vec<EditOp>,
    cursor_before: usize,
    cursor_after: usize,
}

impl EditGroup {
    /// Whether `next` continues this group's run (so it coalesces) rather than
    /// starting a fresh undo step. Decided purely by op positions:
    /// - insert run: the next insert begins where the last one ended;
    /// - delete run: a backspace (removal ending where the last began) or a
    ///   delete-forward (removal at the same index);
    /// - selection replace: a remove immediately followed by an insert at the
    ///   same index (typing over a selection).
    ///
    /// Any other transition (notably insert -> delete) breaks the group.
    fn accepts(&self, next: &EditOp) -> bool {
        let Some(last) = self.ops.last() else {
            return true;
        };
        match (last, next) {
            (EditOp::Insert { at, text }, EditOp::Insert { at: nat, .. }) => {
                *nat == at + text.chars().count()
            }
            (
                EditOp::Remove { at, .. },
                EditOp::Remove {
                    at: nat,
                    text: ntext,
                },
            ) => *nat + ntext.chars().count() == *at || *nat == *at,
            (EditOp::Remove { at, .. }, EditOp::Insert { at: nat, .. }) => *nat == *at,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub struct Buffer {
    rope: Rope,
    path: Option<PathBuf>,
    dirty: bool,
    /// Finalized undo steps (most recent on top).
    undo: Vec<EditGroup>,
    /// Steps that have been undone and can be re-applied (most recent on top).
    redo: Vec<EditGroup>,
    /// The currently growing, not-yet-finalized edit group.
    open: Option<EditGroup>,
    /// Cursor index reported by the pane for the next edit; becomes
    /// `cursor_before` when a new group opens.
    pending_before: usize,
}

impl Buffer {
    /// An empty, unnamed buffer.
    pub fn empty() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            pending_before: 0,
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
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            pending_before: 0,
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
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            pending_before: 0,
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
        self.record(EditOp::Insert {
            at: char_idx,
            text: text.to_string(),
        });
    }

    /// Remove the half-open character range `[start, end)` and mark dirty.
    pub fn remove(&mut self, start: usize, end: usize) {
        if start < end {
            let text = self.rope.slice(start..end).to_string();
            self.rope.remove(start..end);
            self.dirty = true;
            self.record(EditOp::Remove { at: start, text });
        }
    }

    /// Report the pane cursor index for the edit about to happen. It becomes the
    /// `cursor_before` of a freshly opened group; ignored while a contiguous run
    /// continues.
    pub fn begin_edit(&mut self, cursor: usize) {
        self.pending_before = cursor;
    }

    /// Append `op` to the open group, coalescing it into the current run when it
    /// continues, or finalizing the run and starting a new group otherwise.
    /// Opening a new group discards the redo stack (the branch has diverged).
    fn record(&mut self, op: EditOp) {
        let contiguous = self.open.as_ref().is_some_and(|g| g.accepts(&op));
        if !contiguous {
            self.finalize();
            self.redo.clear();
            self.open = Some(EditGroup {
                ops: Vec::new(),
                cursor_before: self.pending_before,
                cursor_after: self.pending_before,
            });
        }
        let group = self.open.as_mut().expect("group just opened");
        group.cursor_after = match &op {
            EditOp::Insert { at, text } => at + text.chars().count(),
            EditOp::Remove { at, .. } => *at,
        };
        group.ops.push(op);
    }

    /// Close the open edit group onto the undo stack so it becomes a single
    /// undoable unit. A no-op when nothing is in progress.
    pub fn finalize(&mut self) {
        if let Some(group) = self.open.take()
            && !group.ops.is_empty()
        {
            self.undo.push(group);
        }
    }

    /// Revert the most recent edit group, returning the cursor char index to
    /// restore (the position before that edit), or `None` if there is nothing to
    /// undo. Inverse edits bypass recording so they don't pollute the history.
    pub fn undo(&mut self) -> Option<usize> {
        self.finalize();
        let group = self.undo.pop()?;
        for op in group.ops.iter().rev() {
            match op {
                EditOp::Insert { at, text } => {
                    self.rope.remove(*at..*at + text.chars().count());
                }
                EditOp::Remove { at, text } => self.rope.insert(*at, text),
            }
        }
        self.dirty = true;
        let target = group.cursor_before;
        self.redo.push(group);
        Some(target)
    }

    /// Re-apply the most recently undone edit group, returning the cursor char
    /// index to restore (the position after that edit), or `None` if there is
    /// nothing to redo.
    pub fn redo(&mut self) -> Option<usize> {
        let group = self.redo.pop()?;
        for op in &group.ops {
            match op {
                EditOp::Insert { at, text } => self.rope.insert(*at, text),
                EditOp::Remove { at, text } => {
                    self.rope.remove(*at..*at + text.chars().count());
                }
            }
        }
        self.dirty = true;
        let target = group.cursor_after;
        self.undo.push(group);
        Some(target)
    }

    /// Write the buffer back to its file, clearing the dirty flag. Returns
    /// `Ok(false)` if the buffer has no associated path (nothing written).
    pub fn save(&mut self) -> io::Result<bool> {
        // Close any in-progress edit so the saved content is one undoable unit.
        self.finalize();
        let Some(path) = self.path.clone() else {
            return Ok(false);
        };
        // Atomic save: write a sibling temp file, fsync, then rename over the
        // target — a crash mid-write can never leave a truncated file. The
        // temp lives in the same directory so the rename stays one-filesystem.
        let mut tmp = path.clone();
        let name = tmp
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        tmp.set_file_name(format!(
            ".{}.vybim-{}.tmp",
            name.display(),
            std::process::id()
        ));
        let written = (|| {
            let mut w = io::BufWriter::new(fs::File::create(&tmp)?);
            self.rope.write_to(&mut w)?;
            let file = w.into_inner().map_err(io::Error::from)?;
            file.sync_all()?;
            // Keep the target's permissions rather than the temp's defaults.
            if let Ok(meta) = fs::metadata(&path) {
                let _ = fs::set_permissions(&tmp, meta.permissions());
            }
            fs::rename(&tmp, &path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written?;
        self.dirty = false;
        Ok(true)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The full document text, for LSP document synchronization.
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// The number of UTF-16 code units from the start of `line` to `char_col`
    /// characters in. LSP columns are UTF-16 code units, not chars/bytes; this
    /// is the conversion at the protocol boundary. Out-of-range inputs clamp.
    pub fn utf16_col(&self, line: usize, char_col: usize) -> usize {
        let last = self.rope.len_lines().saturating_sub(1);
        let line = line.min(last);
        let slice = self.rope.line(line);
        // Clamp to the line's *content* length, excluding the trailing newline.
        let cc = char_col.min(self.line_len_chars(line));
        slice.char_to_utf16_cu(cc)
    }

    /// The character column corresponding to `utf16_col` UTF-16 code units into
    /// `line` — the inverse of [`utf16_col`](Self::utf16_col). Out-of-range
    /// inputs clamp.
    pub fn char_col_from_utf16(&self, line: usize, utf16_col: usize) -> usize {
        let last = self.rope.len_lines().saturating_sub(1);
        let line = line.min(last);
        let slice = self.rope.line(line);
        // Clamp to the line's content length in UTF-16 units (newline excluded).
        let max_u16 = slice.char_to_utf16_cu(self.line_len_chars(line));
        let u = utf16_col.min(max_u16);
        slice.utf16_cu_to_char(u)
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
        let path = std::env::temp_dir().join(format!("vybim_save_test_{}.txt", std::process::id()));
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
    fn save_is_atomic_and_leaves_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("vybim_atomic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "old").unwrap();

        let mut b = Buffer::from_path(&path).unwrap();
        b.insert(0, "new ");
        assert!(b.save().unwrap());

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new old");
        // The sibling temp file was renamed away, not left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stray temp files: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn undo_redo_return_cursor_targets() {
        let mut b = Buffer::from_str("");
        b.begin_edit(0);
        b.insert(0, "abc"); // one group: before 0, after 3
        assert_eq!(b.undo(), Some(0)); // cursor_before
        assert_eq!(b.line_text(0), "");
        assert_eq!(b.redo(), Some(3)); // cursor_after
        assert_eq!(b.line_text(0), "abc");
    }

    #[test]
    fn undo_then_new_edit_clears_redo() {
        let mut b = Buffer::from_str("");
        b.begin_edit(0);
        b.insert(0, "abc");
        b.undo();
        b.begin_edit(0);
        b.insert(0, "x"); // diverging edit clears the redo stack
        assert_eq!(b.redo(), None);
        assert_eq!(b.line_text(0), "x");
    }

    #[test]
    fn undo_redo_empty_stacks_are_none() {
        let mut b = Buffer::from_str("seed");
        assert_eq!(b.undo(), None);
        assert_eq!(b.redo(), None);
        assert_eq!(b.line_text(0), "seed");
    }

    #[test]
    fn save_without_path_is_noop() {
        let mut b = Buffer::from_str("text");
        assert!(!b.save().unwrap());
    }
}
