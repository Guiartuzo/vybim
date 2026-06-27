## Context

NyxVim is a greenfield project — no existing code, this is its first change. The author is new to Rust, so this design optimizes for *learnable sequencing* and *leaning on mature crates* over hand-rolling. The product north star is "snappy like nano/vim": instant startup, sub-perceptible keystroke latency, modeless editing. The architecture must therefore keep the hot path (key → state update → redraw) cheap and avoid per-keystroke whole-buffer work.

The MVP shell mirrors a minimal IDE: a file-tree sidebar, vertically split editor panes, tree-sitter syntax highlighting, and a PTY-backed integrated terminal. AI and LSP are explicitly future work.

## Goals / Non-Goals

**Goals:**
- A single-binary, native, instant-start terminal editor.
- Modeless editing that feels obvious to a VSCode/nano user (arrows, shift+arrow select, ctrl+s).
- An architecture a first-time Rust author can build incrementally, where each milestone is independently runnable.
- Reuse battle-tested crates for every genuinely hard subsystem (rope, terminal emulation, parsing).

**Non-Goals:**
- AI, LSP, vim modal grammar, command palette, plugins, tabs, horizontal splits, search/replace UI.
- Guaranteed cross-platform support in v1 (Linux-first; portable crates chosen so this isn't a dead end).
- A general-purpose widget framework — only the components this MVP needs.

## Decisions

**Rendering: `ratatui` + `crossterm`.**
Immediate-mode TUI with a backend that handles raw mode, alt screen, and input. Rationale: `ratatui` is the most active Rust TUI library and pairs with `crossterm` for portable terminal control. Alternative considered: raw `crossterm` only (more control, far more boilerplate and manual diffing) — rejected to keep velocity for a first Rust project. Immediate-mode also sidesteps a retained widget tree, which reduces shared-mutable-state pain.

**Text buffer: `ropey`.**
A rope handles large files and mid-buffer edits in O(log n) and provides cheap line/char indexing for the viewport. Rationale: proven in Helix; avoids the naive `Vec<String>` performance cliff. Alternative: `Vec<String>` lines (simpler, fine for small files) — rejected because it undermines the "snappy on big files" goal and would need replacing later.

**App architecture: central `App` state + immediate-mode render, single event loop.**
One owned `App` struct holds all state (open buffers, layout tree, sidebar, focus). The loop: block on `crossterm` event → mutate `App` → render from `App`. Rationale: a single owner sidesteps the `Rc<RefCell<>>` graph that traps Rust beginners; panes reference buffers by index/ID into `App`-owned collections rather than by shared pointers.

**Pane model: a layout holding panes addressed by ID; buffers stored centrally.**
Panes are `EditorPane { buffer_id, viewport, cursor, selection }` or `TerminalPane`. Buffers live in an `App`-owned slab/`Vec` keyed by ID. Rationale: decouples "what's shown where" from "the data," lets two panes view one buffer later, and keeps borrows simple (look up by ID, mutate, drop borrow). MVP ships vertical splits only; the layout abstraction leaves room for more later.

**Syntax highlighting: `tree-sitter` with incremental parsing, computed off the hot path.**
Maintain a per-buffer `tree-sitter` tree; on edit, feed the edit to the tree for incremental re-parse and re-highlight only the visible range at render time. Rationale: incremental parsing keeps typing responsive on large files. Alternative: regex/syntect line highlighting — rejected for weaker correctness and worse incremental behavior. Grammars are bundled per supported language; unknown languages fall back to plain text.

**Integrated terminal: `portable-pty` (PTY/process) + `alacritty_terminal` (VT parsing/grid).**
`portable-pty` spawns the shell on a PTY and exposes read/write; `alacritty_terminal` parses the byte stream into a cell grid we render into the pane. A dedicated reader thread pumps PTY output over a channel to the UI thread, which wakes the event loop. Rationale: terminal emulation is a deep problem; `alacritty_terminal` is a mature, correct VT implementation. Alternative: `vt100` (lighter, simpler) — acceptable fallback if `alacritty_terminal`'s API proves heavy; noted as an open question. Writing our own VT parser is out of scope.

**Concurrency: UI on the main thread; PTY I/O on a worker thread; communicate via channels + a wakeup event.**
Rationale: keeps the editor responsive during streaming terminal output. The event loop selects over input events and an internal "redraw/message" channel.

## Risks / Trade-offs

- **Shared mutable state across panes/buffers (the classic Rust beginner wall)** → Mitigate with the ID/slab indirection above instead of `Rc<RefCell<>>`; keep `App` as the single owner and pass narrow mutable borrows.
- **Integrated terminal is the highest-complexity piece** → Sequence it last; integrate `alacritty_terminal` against a known-good VT, and keep `vt100` as a lighter fallback. Treat it as the milestone most likely to slip.
- **Per-keystroke highlighting cost on large files** → Only re-highlight the visible range; rely on tree-sitter incremental edits; never re-parse the whole buffer synchronously on a keystroke.
- **Selection + multibyte/grapheme correctness** → Index by char/grapheme via `ropey` utilities, not bytes; remember target column for vertical movement to avoid cursor drift.
- **Event-loop wakeups across threads** → Use `crossterm`'s event stream plus an internal channel and a wakeup mechanism so PTY output and input both unblock the loop without busy-polling.
- **First-Rust-project scope risk** → Each milestone (skeleton → file view → editing → splits → tree → highlighting → terminal) is independently runnable, so the project is always demoable and learning compounds.

## Open Questions

- `alacritty_terminal` vs `vt100` for the terminal grid — decide once the rendering integration is prototyped; pick the lighter one that meets the rendering scenarios.
- Theme source for highlighting (built-in palette vs. tree-sitter highlight-query capture names) — start with a fixed built-in palette.
- Save semantics for newly created/unnamed buffers — MVP can assume files are opened from disk; a "save as" prompt may be deferred.
