## 1. Project bootstrap

- [x] 1.1 Run `cargo init` to create the binary crate, set crate metadata in `Cargo.toml`, and verify `cargo run` prints/exits cleanly
- [x] 1.2 Add core dependencies: `ratatui`, `crossterm`, `ropey`
- [x] 1.3 Add a `.gitignore` for Rust (`/target`) and make the initial commit

## 2. App shell (capability: app-shell)

- [x] 2.1 Enter raw mode + alternate screen on startup; restore terminal on normal exit
- [x] 2.2 Install a panic hook that restores the terminal before printing the panic
- [x] 2.3 Implement the main loop: block on a `crossterm` event, update state, redraw (no busy spin)
- [x] 2.4 Add global key dispatch and a quit command (`ctrl+q`) that triggers clean teardown
- [x] 2.5 Define the central `App` state struct as the single owner of all app state

## 3. Open and display a file (capability: text-editing)

- [x] 3.1 Load a file path into a `ropey::Rope` buffer (central ID-keyed store deferred to 5.1 where panes need it)
- [x] 3.2 Render the buffer in a pane with line-based viewport rendering
- [x] 3.3 Implement viewport scrolling that keeps the cursor visible (vertical and horizontal)
- [x] 3.4 Accept an initial file path as a CLI argument to open on launch

## 4. Modeless editing (capability: text-editing)

- [ ] 4.1 Track cursor position (line/column) with a remembered target column for vertical moves
- [ ] 4.2 Arrow-key movement, clamped to line/buffer bounds, with viewport follow
- [ ] 4.3 Insert printable characters at the cursor; Enter splits the line
- [ ] 4.4 Backspace/Delete, including line-join across boundaries
- [ ] 4.5 Selection: set anchor on `shift+arrow`, extend to cursor; clear on plain movement
- [ ] 4.6 Typing or backspace with an active selection replaces/removes the selection first
- [ ] 4.7 Track dirty state; `ctrl+s` writes the buffer to disk and marks it clean; show a dirty indicator

## 5. Vertical splits and focus (capability: pane-layout)

- [ ] 5.1 Model the editing area as a layout of panes addressed by ID; panes reference buffers by buffer ID
- [ ] 5.2 Vertical-split command: add a pane beside the current one, dividing available width
- [ ] 5.3 Render each pane independently within its computed region
- [ ] 5.4 Track a single focused pane; route key input only to it; indicate focus visually
- [ ] 5.5 Focus-next command to move focus between panes
- [ ] 5.6 Close-focused-pane command that reclaims the region and moves focus to a remaining pane

## 6. File tree sidebar (capability: file-tree)

- [ ] 6.1 Add `walkdir`; build a directory tree model for the working directory
- [ ] 6.2 Render the sidebar with directories distinguishable from files; reserve sidebar width in the layout
- [ ] 6.3 Expand/collapse directories; maintain the list of visible entries
- [ ] 6.4 Selection navigation (up/down) when the sidebar is focused; focus-sidebar command
- [ ] 6.5 Activate a file entry to open it into the focused editor pane and move focus to that pane

## 7. Syntax highlighting (capability: syntax-highlighting)

- [ ] 7.1 Add `tree-sitter` and at least one grammar (e.g. Rust); map file extension → language
- [ ] 7.2 Maintain a per-buffer parse tree; feed edits incrementally on each modification
- [ ] 7.3 Apply a built-in color palette to highlight captures, re-highlighting only the visible range at render time
- [ ] 7.4 Fall back to plain-text rendering (no error) when no grammar is available
- [ ] 7.5 Verify typing stays responsive in a large highlighted file (no synchronous full re-parse)

## 8. Integrated terminal pane (capability: integrated-terminal)

- [ ] 8.1 Add `portable-pty`; spawn the user's default shell on a PTY from a terminal pane
- [ ] 8.2 Spawn a reader thread that pumps PTY output over a channel and wakes the event loop
- [ ] 8.3 Integrate a VT parser (`alacritty_terminal`, with `vt100` as fallback) to maintain a cell grid from PTY output
- [ ] 8.4 Render the terminal grid in the pane, including SGR colors, cursor moves, line clears, and scrolling
- [ ] 8.5 Forward keystrokes from the focused terminal pane to the PTY
- [ ] 8.6 Resize the PTY when the pane dimensions change
- [ ] 8.7 Terminate the shell child process and release resources when the terminal pane is closed
- [ ] 8.8 Confirm the editor and other panes stay responsive during continuous terminal output

## 9. Polish and verification

- [ ] 9.1 Manual pass against each spec's scenarios (open, edit, select, save, split, tree, highlight, terminal)
- [ ] 9.2 Update `README.md` with build/run instructions and the current keybindings
- [ ] 9.3 Confirm clean startup/teardown leaves the shell unchanged, including after a panic
