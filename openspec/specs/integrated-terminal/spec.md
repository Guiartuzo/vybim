# integrated-terminal

## Purpose

An integrated terminal pane backed by a real PTY: spawn the user's shell, forward keystrokes, parse output into a terminal grid, render it, resize with the pane, and run concurrently without blocking the UI.

## Requirements

### Requirement: PTY-backed terminal pane
The application SHALL provide a terminal pane that spawns the user's shell on a pseudo-terminal (PTY), forwards keystrokes to it, and reads its output.

#### Scenario: Open a terminal pane
- **WHEN** the user opens a terminal pane
- **THEN** the user's default shell is spawned on a PTY and its prompt is displayed in the pane

#### Scenario: Run a command
- **WHEN** the terminal pane is focused and the user types a command and presses Enter
- **THEN** the keystrokes are forwarded to the shell and the command's output appears in the pane

#### Scenario: PTY resizes with the pane
- **WHEN** the terminal pane's dimensions change
- **THEN** the PTY is resized so the shell wraps and renders to the pane's current width and height

### Requirement: Terminal output rendering
The terminal pane SHALL interpret the shell's output as a terminal grid, handling at minimum cursor movement, line clearing, scrolling, and SGR text colors.

#### Scenario: Render colored output
- **WHEN** a program emits ANSI color escape sequences
- **THEN** the pane renders the corresponding colors rather than raw escape codes

#### Scenario: Render cursor and clears
- **WHEN** a program moves the cursor or clears the screen via escape sequences
- **THEN** the pane reflects those operations in its rendered grid

### Requirement: Terminal session lifecycle
The terminal pane SHALL run the shell concurrently with the editor without blocking the UI, and SHALL clean up the child process when the pane is closed.

#### Scenario: Editor stays responsive
- **WHEN** a long-running command produces continuous output
- **THEN** the editor UI and other panes remain responsive while output streams

#### Scenario: Close terminates the shell
- **WHEN** the user closes the terminal pane
- **THEN** the shell child process is terminated and its resources are released
