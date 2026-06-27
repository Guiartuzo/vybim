# NyxVim

A minimalist, **modeless** terminal code editor — fast and snappy like `nano`/`vim`,
but with a familiar VSCode-style editing model (arrow keys move, `Shift+Arrow`
selects, `Ctrl+S` saves), a file-tree sidebar, split panes, syntax highlighting,
and an integrated terminal.

> Status: **MVP foundation.** AI integration — the long-term differentiator — is
> deliberately deferred to a future change. So are LSP, a command palette, and a
> plugin system.

## Build & run

NyxVim is written in Rust. With a Rust toolchain installed (`rustup`):

```bash
# Run, opening a file:
cargo run --release -- path/to/file.rs

# Run in the current directory (empty buffer, sidebar shows the working dir):
cargo run --release

# Build a release binary at target/release/nyxvim:
cargo build --release

# Run the test suite:
cargo test
```

The sidebar is rooted at the current working directory.

## Keybindings

Modeless: you are always typing into the focused pane.

### Global
| Key | Action |
| --- | --- |
| `Ctrl+Q` | Quit |
| `Ctrl+B` | Toggle focus between the sidebar and the editor |

### Editor pane
| Key | Action |
| --- | --- |
| Arrows | Move the cursor |
| `Shift`+Arrows | Extend a selection |
| Printable keys | Insert text (replacing any selection) |
| `Enter` | Split the line |
| `Backspace` / `Delete` | Delete (joining lines at boundaries) |
| `Tab` | Insert four spaces |
| `Ctrl+S` | Save |
| `Ctrl+\` | Split the pane vertically |
| `Ctrl+T` | Open an integrated terminal pane |
| `Ctrl+W` | Close the focused pane |
| `Alt+Left` / `Alt+Right` | Move focus between panes |

### Sidebar (file tree)
| Key | Action |
| --- | --- |
| `Up` / `Down` | Move the selection |
| `Right` / `Left` | Expand / collapse a directory |
| `Enter` | Open a file (into the focused pane) or toggle a directory |

### Terminal pane
When a terminal pane is focused, keystrokes are forwarded to the shell
(including `Ctrl+C`). The global and pane-management chords above are intercepted
by NyxVim and do not reach the shell.

## Architecture

```
main.rs          entry point: open file, set up terminal, run
app.rs           central App state + the AppEvent loop (input + PTY output)
terminal.rs      raw mode / alternate screen / panic-safe teardown
buffer.rs        ropey-backed text buffer (load, edit, save)
pane.rs          editor pane: cursor, selection, scrolling, rendering
file_tree.rs     lazily-loaded file-tree sidebar
syntax.rs        tree-sitter syntax highlighting (per visible line)
terminal_pane.rs integrated terminal: PTY + vt100 grid + reader thread
```

Buffers live in a central store on `App`; panes reference them by id. This ID
indirection keeps shared state simple (no `Rc<RefCell<>>`).

## Known limitations (MVP)

- Syntax highlighting is computed per visible line, so multi-line constructs
  (block comments, multi-line strings) aren't tracked across line boundaries.
- Tabs in files are not width-expanded for cursor placement (NyxVim inserts
  spaces for `Tab`).
- Splits are vertical only; no tabs, search/replace UI, or horizontal splits.

## Development

This project is developed with [OpenSpec](https://github.com/) change proposals.
The founding MVP is specified under `openspec/changes/mvp-editor-foundation/`
(proposal, design, specs, tasks).
