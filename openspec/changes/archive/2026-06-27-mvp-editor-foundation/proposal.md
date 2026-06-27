## Why

There is no terminal code editor that feels as instant as `nano`/`vim` while offering a modeless, VSCode-style editing model and a familiar IDE shell (file tree, splits, integrated terminal). NyxVim aims to fill that gap. This change establishes the foundational MVP: a usable, snappy editor shell. AI integration — the long-term differentiator — is deliberately deferred so the core editing experience is proven first.

## What Changes

- Bootstrap a new Rust binary crate (NyxVim's first code) with the render/event loop.
- Add an **app shell**: terminal raw mode, alternate screen, a single render/event loop, and clean quit + teardown.
- Add **modeless text editing**: open and display a file, scrollable viewport, arrow-key cursor movement, `shift+arrow` selection, text insertion/deletion, and `ctrl+s` save.
- Add **vertical split panes** with focus switching between panes.
- Add a **file tree sidebar** that lists the working directory and opens a selected file into the focused pane.
- Add **syntax highlighting** via tree-sitter in editor panes.
- Add an **integrated terminal pane** backed by a real PTY, rendering shell output inside a pane (effectively a minimal terminal emulator).
- Establish the crate/dependency baseline: `ratatui` + `crossterm`, `ropey`, `tree-sitter`, `walkdir`, `portable-pty` + `alacritty_terminal`.

Non-goals (future changes): AI integration, LSP, vim modal editing, command palette, plugin system, horizontal splits, tabs, search/replace UI.

## Capabilities

### New Capabilities
- `app-shell`: Terminal lifecycle (raw mode, alternate screen, teardown), the main render/event loop, global key dispatch, and quit.
- `text-editing`: Opening, displaying, scrolling, modeless editing (movement, selection, insert/delete), and saving a file buffer.
- `pane-layout`: Vertical split panes, focus management, and routing input/rendering to the focused pane.
- `file-tree`: A sidebar listing the working directory tree and opening files into the focused pane.
- `syntax-highlighting`: tree-sitter-based highlighting of editor pane contents.
- `integrated-terminal`: A PTY-backed terminal pane that runs a shell and renders its output.

### Modified Capabilities
<!-- None — this is the project's first change; no existing specs. -->

## Impact

- **New code**: First Rust crate for the project (`Cargo.toml`, `src/`). No existing code to modify.
- **Dependencies**: `ratatui`, `crossterm`, `ropey`, `tree-sitter` (+ grammar crates), `walkdir`, `portable-pty`, `alacritty_terminal`.
- **Risk areas**: The integrated terminal pane (ANSI/PTY handling, concurrency) is the highest-complexity piece and is sequenced last. Shared mutable state across panes/buffers is the primary Rust-learning hurdle for a first-time Rust author.
- **Platforms**: Linux first (primary dev environment); cross-platform terminal/PTY handling kept in mind via portable crates but not a v1 guarantee.
