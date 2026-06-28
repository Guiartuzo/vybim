mod app;
mod buffer;
mod complete;
mod diff_view;
mod file_find;
mod file_tree;
mod git;
mod minibuffer;
mod pane;
mod syntax;
mod terminal;
mod terminal_area;
mod terminal_pane;
mod theme;

use app::App;
use buffer::Buffer;

fn main() -> std::io::Result<()> {
    // Open the file given on the command line, or start with an empty buffer.
    let buffer = match std::env::args().nth(1) {
        Some(path) => Buffer::from_path(path)?,
        None => Buffer::empty(),
    };
    let root = std::env::current_dir()?;

    let mut tui = terminal::init()?;
    let result = App::new(buffer, root).run(&mut tui);
    // Always restore the terminal, even if the run loop returned an error.
    terminal::restore()?;
    result
}
