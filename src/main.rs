mod app;
mod buffer;
mod terminal;
mod view;

use app::App;
use buffer::Buffer;
use view::EditorView;

fn main() -> std::io::Result<()> {
    // Open the file given on the command line, or start with an empty buffer.
    let buffer = match std::env::args().nth(1) {
        Some(path) => Buffer::from_path(path)?,
        None => Buffer::empty(),
    };

    let mut tui = terminal::init()?;
    let result = App::new(EditorView::new(buffer)).run(&mut tui);
    // Always restore the terminal, even if the run loop returned an error.
    terminal::restore()?;
    result
}
