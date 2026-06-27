# pane-layout

## Purpose

Vertical split panes that divide the editing area, with exactly one focused pane receiving input, focus movement between panes, and pane close.

## Requirements

### Requirement: Vertical split panes
The layout SHALL support splitting the editing area into multiple side-by-side (vertical split) panes that divide the available width.

#### Scenario: Create a vertical split
- **WHEN** the user issues the vertical-split command
- **THEN** the editing area is divided into an additional pane placed beside the current one, each rendering its own content

#### Scenario: Panes share width
- **WHEN** a vertical split exists
- **THEN** the available width is allocated across the panes and each pane renders independently within its region

### Requirement: Focus management
Exactly one pane SHALL be focused at a time, and input SHALL be routed to the focused pane. The user SHALL be able to move focus between panes.

#### Scenario: Switch focus between panes
- **WHEN** the user issues the focus-next command
- **THEN** focus moves to the adjacent pane and is visually indicated

#### Scenario: Input targets the focused pane
- **WHEN** a key is pressed while a pane is focused
- **THEN** the key is handled by that pane and does not affect unfocused panes

### Requirement: Close a pane
The user SHALL be able to close the focused pane, with focus moving to a remaining pane.

#### Scenario: Close focused pane
- **WHEN** the user closes the focused pane and other panes remain
- **THEN** the pane is removed, its region is reclaimed by the layout, and focus moves to a remaining pane
