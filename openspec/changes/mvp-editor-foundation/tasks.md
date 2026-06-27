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

- [x] 4.1 Track cursor position (line/column) with a remembered target column for vertical moves
- [x] 4.2 Arrow-key movement, clamped to line/buffer bounds, with viewport follow
- [x] 4.3 Insert printable characters at the cursor; Enter splits the line
- [x] 4.4 Backspace/Delete, including line-join across boundaries
- [x] 4.5 Selection: set anchor on `shift+arrow`, extend to cursor; clear on plain movement
- [x] 4.6 Typing or backspace with an active selection replaces/removes the selection first
- [x] 4.7 Track dirty state; `ctrl+s` writes the buffer to disk and marks it clean; show a dirty indicator

## 5. Vertical splits and focus (capability: pane-layout)

- [x] 5.1 Model the editing area as a layout of panes addressed by ID; panes reference buffers by buffer ID
- [x] 5.2 Vertical-split command: add a pane beside the current one, dividing available width
- [x] 5.3 Render each pane independently within its computed region
- [x] 5.4 Track a single focused pane; route key input only to it; indicate focus visually
- [x] 5.5 Focus-next command to move focus between panes
- [x] 5.6 Close-focused-pane command that reclaims the region and moves focus to a remaining pane

## 6. File tree sidebar (capability: file-tree)

- [x] 6.1 Add `walkdir`; build a directory tree model for the working directory
- [x] 6.2 Render the sidebar with directories distinguishable from files; reserve sidebar width in the layout
- [x] 6.3 Expand/collapse directories; maintain the list of visible entries
- [x] 6.4 Selection navigation (up/down) when the sidebar is focused; focus-sidebar command
- [x] 6.5 Activate a file entry to open it into the focused editor pane and move focus to that pane

## 7. Syntax highlighting (capability: syntax-highlighting)

- [x] 7.1 Add `tree-sitter` and at least one grammar (e.g. Rust); map file extension → language
- [x] 7.2 Highlight per visible line at render time (viewport-bounded work; persisted incremental tree noted as a future optimization)
- [x] 7.3 Apply a built-in color palette to highlight captures, re-highlighting only the visible range at render time
- [x] 7.4 Fall back to plain-text rendering (no error) when no grammar is available
- [x] 7.5 Verify typing stays responsive in a large highlighted file (no synchronous full re-parse)

## 8. Integrated terminal pane (capability: integrated-terminal)

- [x] 8.1 Add `portable-pty`; spawn the user's default shell on a PTY from a terminal pane
- [x] 8.2 Spawn a reader thread that pumps PTY output over a channel and wakes the event loop
- [x] 8.3 Integrate a VT parser (chose `vt100` per the design's open question) to maintain a cell grid from PTY output
- [x] 8.4 Render the terminal grid in the pane, including SGR colors, cursor moves, line clears, and scrolling
- [x] 8.5 Forward keystrokes from the focused terminal pane to the PTY
- [x] 8.6 Resize the PTY when the pane dimensions change
- [x] 8.7 Terminate the shell child process and release resources when the terminal pane is closed
- [x] 8.8 Confirm the editor and other panes stay responsive during continuous terminal output

## 9. Polish and verification

- [x] 9.1 Spec scenarios covered by 37 unit/integration tests (incl. end-to-end terminal); clippy clean; release build green
- [x] 9.2 Update `README.md` with build/run instructions and the current keybindings
- [x] 9.3 Confirm clean startup/teardown leaves the shell unchanged, including after a panic (panic hook restores; verified clean non-TTY failure)
