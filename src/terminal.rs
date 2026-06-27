//! Terminal lifecycle: raw mode, the alternate screen, and panic-safe teardown.
//!
//! NyxVim takes over the terminal on startup and must always hand it back in a
//! usable state — on a normal quit, on an error, and even on a panic.

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};

/// The concrete terminal type the rest of the app draws to.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Whether we pushed keyboard-enhancement flags at startup, so teardown only
/// pops what it pushed (popping an empty stack would desync the terminal).
static ENHANCED_KEYBOARD: AtomicBool = AtomicBool::new(false);

/// Enter raw mode and the alternate screen, install the panic hook, and return
/// a ready-to-draw terminal. The caller is responsible for calling [`restore`].
pub fn init() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    enable_enhanced_keyboard(&mut stdout);
    install_panic_hook();
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Restore the terminal to its pre-launch state: cooked mode, main screen, and
/// any pushed keyboard-enhancement flags released.
pub fn restore() -> io::Result<()> {
    let mut stdout = io::stdout();
    if ENHANCED_KEYBOARD.swap(false, Ordering::SeqCst) {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    disable_raw_mode()?;
    execute!(stdout, LeaveAlternateScreen)?;
    Ok(())
}

/// Ask the terminal to report modifier chords unambiguously (so `shift+arrow`
/// and the split chord arrive with their modifiers intact). Terminals that do
/// not support the Kitty keyboard protocol are left on standard reporting — the
/// app keeps working, just without the disambiguation.
fn enable_enhanced_keyboard(stdout: &mut Stdout) {
    if supports_keyboard_enhancement().unwrap_or(false)
        && execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    {
        ENHANCED_KEYBOARD.store(true, Ordering::SeqCst);
    }
}

/// Wrap the existing panic hook so the terminal is restored before the panic
/// message is printed — otherwise the message would render into raw mode and be
/// unreadable, and the user's shell would be left broken.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        original(info);
    }));
}
