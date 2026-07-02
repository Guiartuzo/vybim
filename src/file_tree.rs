//! The file-tree sidebar: a lazily-loaded directory tree with a movable
//! selection.
//!
//! Each directory's children are read only when it is first expanded, so
//! opening Vybim in a large repository stays instant. The tree is rendered by
//! flattening the expanded nodes into a list of visible rows.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Borders, Paragraph};
use walkdir::WalkDir;

use crate::theme::Theme;

#[derive(Debug)]
struct Node {
    name: String,
    path: PathBuf,
    is_dir: bool,
    /// Depth below the (hidden) root; the root's children are depth 1.
    depth: usize,
    expanded: bool,
    /// `None` until the directory is first expanded.
    children: Option<Vec<Node>>,
}

impl Node {
    fn new(path: PathBuf, is_dir: bool, depth: usize) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self {
            name,
            path,
            is_dir,
            depth,
            expanded: false,
            children: None,
        }
    }
}

/// Read a directory's immediate children, directories first then files, each
/// group sorted by name.
fn read_children(dir: &Path, depth: usize) -> Vec<Node> {
    let mut nodes: Vec<Node> = WalkDir::new(dir)
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| Node::new(e.path().to_path_buf(), e.file_type().is_dir(), depth))
        .collect();
    nodes.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    nodes
}

#[derive(Debug)]
pub struct FileTree {
    /// The working directory. It is not itself shown; its children are the
    /// top-level rows.
    root: Node,
    selected: usize,
}

impl FileTree {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let path = root.as_ref().to_path_buf();
        let mut root = Node::new(path.clone(), true, 0);
        root.expanded = true;
        root.children = Some(read_children(&path, 1));
        Self { root, selected: 0 }
    }

    /// The currently visible rows, in display order.
    fn visible(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        if let Some(children) = &self.root.children {
            for child in children {
                collect_visible(child, &mut out);
            }
        }
        out
    }

    #[allow(dead_code)] // public query, exercised by tests
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.visible().get(self.selected).map(|n| n.path.clone())
    }

    pub fn select_next(&mut self) {
        let count = self.visible().len();
        if count > 0 && self.selected + 1 < count {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn expand_selected(&mut self) {
        if let Some(node) = self.nth_visible_mut(self.selected)
            && node.is_dir
            && !node.expanded
        {
            node.expanded = true;
            load_if_needed(node);
        }
    }

    pub fn collapse_selected(&mut self) {
        if let Some(node) = self.nth_visible_mut(self.selected)
            && node.is_dir
            && node.expanded
        {
            node.expanded = false;
        }
        self.clamp_selection();
    }

    /// Act on the selected row: toggle a directory, or return a file's path to
    /// open.
    pub fn activate(&mut self) -> Option<PathBuf> {
        let (is_dir, path) = {
            let node = self.nth_visible_mut(self.selected)?;
            (node.is_dir, node.path.clone())
        };
        if is_dir {
            if let Some(node) = self.nth_visible_mut(self.selected) {
                node.expanded = !node.expanded;
                if node.expanded {
                    load_if_needed(node);
                }
            }
            self.clamp_selection();
            None
        } else {
            Some(path)
        }
    }

    /// Find the nth visible node mutably, walking the tree in display order.
    fn nth_visible_mut(&mut self, target: usize) -> Option<&mut Node> {
        let mut counter = 0;
        let children = self.root.children.as_mut()?;
        for child in children.iter_mut() {
            if let Some(found) = nth_visible_in(child, &mut counter, target) {
                return Some(found);
            }
        }
        None
    }

    fn clamp_selection(&mut self) {
        let count = self.visible().len();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, focused: bool, theme: &Theme) {
        let visible = self.visible();
        let mut lines: Vec<Line> = Vec::with_capacity(visible.len());
        for (i, node) in visible.iter().enumerate() {
            let indent = "  ".repeat(node.depth.saturating_sub(1));
            let marker = if node.is_dir {
                if node.expanded { "▾ " } else { "▸ " }
            } else {
                "  "
            };
            let name = if node.is_dir {
                format!("{}/", node.name)
            } else {
                node.name.clone()
            };
            let text = format!("{indent}{marker}{name}");

            let style = if i == self.selected {
                if focused {
                    Style::new().bg(theme.focus_bg).fg(theme.focus_fg)
                } else {
                    // Unfocused selection tints only the background (no fg).
                    Style::new().bg(theme.inactive_bg)
                }
            } else {
                Style::new()
            };
            lines.push(Line::styled(text, style));
        }

        // A rounded box on all sides; the file list renders inside it. The
        // Paragraph's `.block(...)` clips the lines to the block's inner area.
        let block = theme.block(Borders::ALL);
        frame.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    }
}

fn collect_visible<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
    out.push(node);
    if node.is_dir
        && node.expanded
        && let Some(children) = &node.children
    {
        for child in children {
            collect_visible(child, out);
        }
    }
}

fn nth_visible_in<'a>(
    node: &'a mut Node,
    counter: &mut usize,
    target: usize,
) -> Option<&'a mut Node> {
    if *counter == target {
        return Some(node);
    }
    *counter += 1;
    if node.is_dir
        && node.expanded
        && let Some(children) = node.children.as_mut()
    {
        for child in children.iter_mut() {
            if let Some(found) = nth_visible_in(child, counter, target) {
                return Some(found);
            }
        }
    }
    None
}

/// Load a directory node's children if they haven't been read yet.
fn load_if_needed(node: &mut Node) {
    if node.children.is_none() {
        node.children = Some(read_children(&node.path, node.depth + 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a temp directory:
    ///   root/
    ///     a.txt
    ///     z.txt
    ///     sub/
    ///       b.txt
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "vybim_tree_{}_{}",
            std::process::id(),
            // a counter so repeated calls don't collide
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("z.txt"), "z").unwrap();
        std::fs::write(root.join("sub/b.txt"), "b").unwrap();
        root
    }

    fn names(tree: &FileTree) -> Vec<String> {
        tree.visible().iter().map(|n| n.name.clone()).collect()
    }

    #[test]
    fn lists_dirs_first_then_files() {
        let root = fixture();
        let tree = FileTree::new(&root);
        assert_eq!(names(&tree), vec!["sub", "a.txt", "z.txt"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn expand_and_collapse_directory() {
        let root = fixture();
        let mut tree = FileTree::new(&root);
        // "sub" is selected first (index 0)
        tree.expand_selected();
        assert_eq!(names(&tree), vec!["sub", "b.txt", "a.txt", "z.txt"]);
        tree.collapse_selected();
        assert_eq!(names(&tree), vec!["sub", "a.txt", "z.txt"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn navigation_and_activate_file_returns_path() {
        let root = fixture();
        let mut tree = FileTree::new(&root);
        tree.select_next(); // a.txt
        assert_eq!(tree.selected_path().unwrap().file_name().unwrap(), "a.txt");
        let opened = tree.activate().expect("file should yield a path");
        assert_eq!(opened.file_name().unwrap(), "a.txt");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn activate_directory_toggles_and_returns_none() {
        let root = fixture();
        let mut tree = FileTree::new(&root);
        // selected is "sub"
        assert!(tree.activate().is_none());
        assert_eq!(names(&tree), vec!["sub", "b.txt", "a.txt", "z.txt"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn selection_clamps_after_collapse() {
        let root = fixture();
        let mut tree = FileTree::new(&root);
        tree.expand_selected(); // sub, b.txt, a.txt, z.txt
        tree.select_next();
        tree.select_next();
        tree.select_next(); // z.txt at index 3
        tree.selected = 3;
        // collapse from a non-sub row shouldn't panic; collapse sub via reselect
        tree.selected = 0;
        tree.collapse_selected();
        // selection still valid
        assert!(tree.selected_path().is_some());
        std::fs::remove_dir_all(&root).ok();
    }
}
