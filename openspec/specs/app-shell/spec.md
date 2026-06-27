# app-shell

## Purpose

The terminal lifecycle and the main render/event loop: take over the terminal on startup, restore it cleanly on exit (including on panic), run a single non-busy loop, and dispatch keys to the focused component with a global quit.

## Requirements

### Requirement: Terminal lifecycle management
The application SHALL enter raw mode and the alternate screen on startup, and SHALL fully restore the terminal (cooked mode, main screen, visible cursor) on exit, including on panic.

#### Scenario: Clean startup
- **WHEN** NyxVim launches
- **THEN** the terminal enters raw mode and the alternate screen, leaving the user's prior shell scrollback untouched

#### Scenario: Clean teardown on quit
- **WHEN** the user quits NyxVim
- **THEN** raw mode is disabled, the alternate screen is left, and the cursor is restored, returning the shell to its pre-launch state

#### Scenario: Teardown on panic
- **WHEN** the application panics
- **THEN** the terminal is still restored to a usable state before the process exits

### Requirement: Render and event loop
The application SHALL run a single loop that polls input events, updates application state, and redraws the screen, and SHALL redraw only in response to events or state changes (no busy spin).

#### Scenario: Idle does not consume CPU
- **WHEN** no input is received
- **THEN** the application blocks on input and does not continuously redraw

#### Scenario: Input triggers redraw
- **WHEN** an input event is received
- **THEN** application state is updated and the screen is redrawn to reflect it

### Requirement: Global key dispatch and quit
The application SHALL dispatch key events to the focused component and SHALL provide a global quit command.

#### Scenario: Quit command exits
- **WHEN** the user presses the global quit key (e.g. `ctrl+q`)
- **THEN** the application begins teardown and exits

#### Scenario: Keys route to focused component
- **WHEN** a non-global key is pressed
- **THEN** the event is delivered to the currently focused component rather than handled globally
