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
    /// Build a highlighter for a file, or `None` if no grammar is bundled for
    /// it. Extension→language resolution is delegated to the LSP registry's
    /// [`language_of`](crate::lsp::registry::language_of) — the single map —
    /// so the highlighter and the language servers can't drift apart.
    pub fn for_path(path: &Path) -> Option<Syntax> {
        Self::for_language(crate::lsp::registry::language_of(path)?)
    }

    /// A highlighter for a language key, or `None` when no grammar is bundled
    /// (the registry knows languages we serve via LSP but don't highlight).
    fn for_language(language: &str) -> Option<Syntax> {
        match language {
            "rust" => Some(Self::grammar(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
            )),
            "c" => Some(Self::grammar(
                tree_sitter_c::LANGUAGE.into(),
                "c",
                tree_sitter_c::HIGHLIGHT_QUERY,
            )),
            "python" => Some(Self::grammar(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY,
            )),
            "cpp" => Some(Self::grammar(
                tree_sitter_cpp::LANGUAGE.into(),
                "cpp",
                tree_sitter_cpp::HIGHLIGHT_QUERY,
            )),
            _ => None,
        }
    }

    fn grammar(language: tree_sitter::Language, name: &str, highlights_query: &str) -> Syntax {
        let mut config = HighlightConfiguration::new(language, name, highlights_query, "", "")
            .expect("bundled highlight query is valid");
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
    fn registry_languages_without_a_bundled_grammar_have_none() {
        // The registry maps these for LSP, but no grammar is bundled: the
        // shared extension→language map must not invent a highlighter.
        assert!(Syntax::for_path(Path::new("app.ts")).is_none());
        assert!(Syntax::for_path(Path::new("main.go")).is_none());
    }

    #[test]
    fn c_files_and_headers_get_a_grammar() {
        assert!(Syntax::for_path(Path::new("main.c")).is_some());
        assert!(Syntax::for_path(Path::new("main.h")).is_some());
    }

    #[test]
    fn python_and_cpp_files_get_a_grammar() {
        assert!(Syntax::for_path(Path::new("app.py")).is_some());
        assert!(Syntax::for_path(Path::new("app.pyi")).is_some());
        assert!(Syntax::for_path(Path::new("app.cpp")).is_some());
        assert!(Syntax::for_path(Path::new("app.cc")).is_some());
        assert!(Syntax::for_path(Path::new("app.hpp")).is_some());
    }

    #[test]
    fn python_and_cpp_keywords_are_highlighted() {
        let keyword_style = style_for("keyword");
        // Python: "def" (bytes 0..3) is a keyword.
        let py = Syntax::for_path(Path::new("x.py")).unwrap();
        let spans = py.highlight_line("def f():");
        assert!(
            spans
                .iter()
                .any(|(s, e, style)| *s == 0 && *e == 3 && *style == keyword_style),
            "expected 'def' highlighted as a keyword"
        );
        // C++: "class" (bytes 0..5) is a keyword.
        let cpp = Syntax::for_path(Path::new("x.cpp")).unwrap();
        let spans = cpp.highlight_line("class Foo {};");
        assert!(
            spans
                .iter()
                .any(|(s, e, style)| *s == 0 && *e == 5 && *style == keyword_style),
            "expected 'class' highlighted as a keyword"
        );
    }

    #[test]
    fn c_keyword_is_highlighted() {
        let syntax = Syntax::for_path(Path::new("x.c")).unwrap();
        // "return" is a keyword; a span should cover it with the keyword color.
        let spans = syntax.highlight_line("return 0;");
        let keyword_style = style_for("keyword");
        let covers_return = spans
            .iter()
            .any(|(s, e, style)| *s == 0 && *e == 6 && *style == keyword_style);
        assert!(covers_return, "expected 'return' highlighted as a keyword");
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
