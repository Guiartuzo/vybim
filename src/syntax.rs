//! Syntax highlighting via tree-sitter.
//!
//! Highlighting is computed per visible line at render time, so the work is
//! bounded by the viewport rather than the file size — typing stays responsive
//! even in a large file. (A persisted incremental parse tree would be a future
//! optimization; per-line highlighting trades exact multi-line constructs for
//! simplicity and robustness.)

use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// The highlight capture names we recognize, paired with a palette. The event
/// stream refers to captures by their index in this list.
const HIGHLIGHTS: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "escape",
    "function",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// A highlighted run within a line: a byte range and the style to apply.
pub type Span = (usize, usize, Style);

pub struct Syntax {
    config: HighlightConfiguration,
    styles: Vec<Style>,
}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax").finish_non_exhaustive()
    }
}

impl Syntax {
    /// Build a highlighter for a file, based on its extension, or `None` if no
    /// grammar is bundled for it.
    pub fn for_path(path: &Path) -> Option<Syntax> {
        match path.extension()?.to_str()? {
            "rs" => Some(Self::rust()),
            _ => None,
        }
    }

    fn rust() -> Syntax {
        let mut config = HighlightConfiguration::new(
            tree_sitter_rust::LANGUAGE.into(),
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            "",
            "",
        )
        .expect("bundled Rust highlight query is valid");
        config.configure(HIGHLIGHTS);

        let styles = HIGHLIGHTS.iter().map(|name| style_for(name)).collect();
        Syntax { config, styles }
    }

    /// Compute the highlight spans (byte ranges into `line`) for one line of
    /// text. Lines are highlighted independently, which keeps work bounded.
    pub fn highlight_line(&self, line: &str) -> Vec<Span> {
        let mut spans = Vec::new();
        let mut highlighter = Highlighter::new();
        let events = match highlighter.highlight(&self.config, line.as_bytes(), None, |_| None) {
            Ok(events) => events,
            Err(_) => return spans,
        };

        let mut stack: Vec<usize> = Vec::new();
        for event in events {
            match event {
                Ok(HighlightEvent::HighlightStart(h)) => stack.push(h.0),
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    if let Some(&idx) = stack.last()
                        && start < end
                    {
                        spans.push((start, end, self.styles[idx]));
                    }
                }
                Err(_) => break,
            }
        }
        spans
    }
}

/// Map a capture name to a palette color, by longest matching prefix.
fn style_for(name: &str) -> Style {
    let base = Style::new();
    if name.starts_with("comment") {
        return base.fg(Color::DarkGray).add_modifier(Modifier::ITALIC);
    }
    if name.starts_with("keyword") {
        return base.fg(Color::Magenta);
    }
    if name.starts_with("string") || name == "escape" {
        return base.fg(Color::Green);
    }
    if name.starts_with("function") || name == "constructor" {
        return base.fg(Color::Blue);
    }
    if name.starts_with("type") {
        return base.fg(Color::Yellow);
    }
    if name.starts_with("constant") || name == "number" {
        return base.fg(Color::Cyan);
    }
    if name == "attribute" || name == "label" {
        return base.fg(Color::LightYellow);
    }
    if name.starts_with("variable.builtin") || name == "constant.builtin" {
        return base.fg(Color::Red);
    }
    if name.starts_with("operator") || name.starts_with("punctuation") {
        return base.fg(Color::Gray);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn unsupported_extension_has_no_grammar() {
        assert!(Syntax::for_path(Path::new("notes.txt")).is_none());
        assert!(Syntax::for_path(Path::new("README")).is_none());
    }

    #[test]
    fn rust_files_get_a_grammar() {
        assert!(Syntax::for_path(Path::new("main.rs")).is_some());
    }

    #[test]
    fn keyword_and_comment_are_highlighted_distinctly() {
        let syntax = Syntax::for_path(Path::new("x.rs")).unwrap();
        // "fn" is a keyword; the spans should cover it with the keyword color.
        let spans = syntax.highlight_line("fn main() {}");
        assert!(!spans.is_empty(), "expected some highlight spans");
        let keyword_style = style_for("keyword");
        let covers_fn = spans
            .iter()
            .any(|(s, e, style)| *s == 0 && *e == 2 && *style == keyword_style);
        assert!(covers_fn, "expected 'fn' to be highlighted as a keyword");
    }

    #[test]
    fn comment_line_highlights_as_comment() {
        let syntax = Syntax::for_path(Path::new("x.rs")).unwrap();
        let spans = syntax.highlight_line("// hello");
        let comment_style = style_for("comment");
        assert!(spans.iter().any(|(_, _, style)| *style == comment_style));
    }
}
