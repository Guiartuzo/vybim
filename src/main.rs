mod app;
mod buffer;
mod file_tree;
mod pane;
mod syntax;
mod terminal;
mod terminal_area;
mod terminal_pane;

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
