# text-editing

## Purpose

Modeless text editing over a file buffer: open and display with scrolling, move the cursor with arrow keys, select with shift+arrow, insert and delete text across line boundaries, and save to disk with dirty tracking.

## Requirements

### Requirement: Open and display a file
The editor SHALL open a file into a text buffer and display its contents, and SHALL handle files larger than the viewport via scrolling.

#### Scenario: Open an existing file
- **WHEN** a file path is opened
- **THEN** its contents are loaded into a buffer and rendered in the pane starting at the top

#### Scenario: Viewport scrolls to follow the cursor
- **WHEN** the cursor moves beyond the visible region of the pane
- **THEN** the viewport scrolls so the cursor remains visible

### Requirement: Modeless cursor movement
The editor SHALL be modeless: the buffer is always editable, and arrow keys move the cursor without any mode switch.

#### Scenario: Arrow keys move the cursor
- **WHEN** the user presses an arrow key
- **THEN** the cursor moves one cell in that direction, clamped to the bounds of the line and buffer

#### Scenario: Vertical movement preserves column intent
- **WHEN** the user moves up or down across lines of differing length
- **THEN** the cursor lands at the nearest valid column to its remembered target column

### Requirement: Text insertion and deletion
The editor SHALL insert typed characters at the cursor and SHALL support backspace and delete, including across line boundaries.

#### Scenario: Insert a character
- **WHEN** the user types a printable character
- **THEN** the character is inserted at the cursor and the cursor advances

#### Scenario: Backspace at line start joins lines
- **WHEN** the user presses backspace at column zero of a non-first line
- **THEN** the line is joined with the previous line and the cursor moves to the join point

#### Scenario: Enter splits a line
- **WHEN** the user presses Enter
- **THEN** the line is split at the cursor and the cursor moves to the start of the new line

### Requirement: Selection via shift+arrow
The editor SHALL allow extending a text selection by holding shift while pressing arrow keys, and SHALL clear the selection on an unmodified movement.

#### Scenario: Extend selection
- **WHEN** the user holds shift and presses an arrow key
- **THEN** the selection anchor is set (if unset) and the selection extends to the new cursor position

#### Scenario: Collapse selection on plain movement
- **WHEN** a selection exists and the user presses an arrow key without shift
- **THEN** the selection is cleared and the cursor moves normally

#### Scenario: Typing replaces selection
- **WHEN** a selection exists and the user types a character or presses backspace
- **THEN** the selected text is removed and the input is applied at the resulting cursor position

### Requirement: Save the buffer
The editor SHALL write the buffer back to its file on the save command and SHALL track unsaved (dirty) state.

#### Scenario: Save to disk
- **WHEN** the user presses the save key (`ctrl+s`)
- **THEN** the buffer contents are written to the file and the buffer is marked clean

#### Scenario: Dirty indicator
- **WHEN** the buffer has unsaved edits
- **THEN** the pane indicates a modified/dirty state until the next successful save
