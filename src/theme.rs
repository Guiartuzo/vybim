//! The UI theme: a single source of truth for every chrome color NyxVim draws.
//!
//! Before this module, ~70 `Color::` literals were scattered across eight files,
//! and the "blue if focused, dark-gray if not" selection rule was copy-pasted in
//! several places. The [`Theme`] gathers those into **semantic tokens** named for
//! their role (text, border, selection, …) rather than their concrete color, so
//! the whole editor can be reskinned by changing one place — see [`Theme::default`].
//!
//! Scope: this owns *UI chrome* only. Syntax-highlight colors (`syntax.rs`) and
//! terminal content colors (vt100 conversion in `terminal_pane.rs`) are separate
//! color spaces and are intentionally left out.
//!
//! `Theme` is `Copy` (every field is a `Copy` `Color`/`BorderType`), so render
//! code takes it by value or by a cheap `&Theme` without borrow-checker friction.

use ratatui::style::{Color, Style};
use ratatui::widgets::BorderType;

/// Semantic UI color tokens. Each field names a *role*; the concrete colors live
/// only in [`Theme::default`] (and any future palette constructor).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Primary foreground text (buffer text, focused line number).
    pub text: Color,
    /// Muted foreground: borders-as-text, hints, placeholders, unfocused gutter.
    pub text_muted: Color,
    /// Accent for titles, group headers, and active markers.
    pub accent: Color,
    /// Block borders and inter-region dividers.
    pub border: Color,
    /// Background of the line the cursor is on.
    pub cursor_line: Color,
    /// Background of selected *editor text* (distinct from a selected list row).
    pub selection: Color,
    /// Foreground of a selected row in a *focused* region.
    pub focus_fg: Color,
    /// Background of a selected row in a *focused* region (the primary highlight;
    /// also used for the focused modal/help border).
    pub focus_bg: Color,
    /// Foreground of a selected row / muted list item in an *unfocused* region.
    pub inactive_fg: Color,
    /// Background of a selected row in an *unfocused* region, and of chrome
    /// surfaces such as the footer.
    pub inactive_bg: Color,
    /// Background of the minibuffer prompt row.
    pub prompt_bg: Color,
    /// Added line: foreground / background.
    pub diff_add_fg: Color,
    pub diff_add_bg: Color,
    /// Deleted line: foreground / background.
    pub diff_del_fg: Color,
    pub diff_del_bg: Color,
    /// The empty opposite side of a diff change.
    pub diff_gap_bg: Color,
}

impl Default for Theme {
    /// The default theme. These values reproduce NyxVim's pre-theme colors
    /// exactly, so introducing the theme is a no-op on screen.
    fn default() -> Self {
        Self {
            text: Color::White,
            text_muted: Color::DarkGray,
            accent: Color::Yellow,
            border: Color::DarkGray,
            cursor_line: Color::Indexed(236),
            selection: Color::Indexed(24),
            focus_fg: Color::White,
            focus_bg: Color::Blue,
            inactive_fg: Color::Gray,
            inactive_bg: Color::DarkGray,
            prompt_bg: Color::Black,
            diff_add_fg: Color::Green,
            diff_add_bg: Color::Indexed(22),
            diff_del_fg: Color::Red,
            diff_del_bg: Color::Indexed(52),
            diff_gap_bg: Color::Indexed(235),
        }
    }
}

impl Theme {
    /// Border style for bordered blocks. A single themed choice so the whole
    /// editor's border look (square vs rounded) lives in one place.
    pub fn border_type(&self) -> BorderType {
        BorderType::Plain
    }

    /// Style for a selected row whose region's focus state is `focused`. Collapses
    /// the focused-vs-inactive selection rule into one place. (Sites whose
    /// unfocused branch keeps a different foreground build the style from the
    /// tokens directly instead.)
    pub fn list_row(&self, focused: bool) -> Style {
        if focused {
            Style::new().bg(self.focus_bg).fg(self.focus_fg)
        } else {
            Style::new().bg(self.inactive_bg).fg(self.inactive_fg)
        }
    }
}
