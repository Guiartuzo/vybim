# file-tree

## Purpose

A sidebar listing the working directory tree with expandable/collapsible directories, a movable selection, and the ability to open a file into the focused editor pane.

## Requirements

### Requirement: Sidebar directory tree
The application SHALL display a file tree sidebar listing the working directory's files and subdirectories, with expandable/collapsible directories.

#### Scenario: List the working directory
- **WHEN** NyxVim launches in a directory
- **THEN** the sidebar lists that directory's entries, with directories distinguishable from files

#### Scenario: Expand and collapse directories
- **WHEN** the user expands a collapsed directory in the tree
- **THEN** its child entries are shown; collapsing hides them again

### Requirement: Navigate and open from the tree
The user SHALL be able to move a selection through the tree and open a selected file into the focused editor pane.

#### Scenario: Move selection
- **WHEN** the user presses up/down while the tree is focused
- **THEN** the selection moves to the previous/next visible entry

#### Scenario: Open a file into the focused pane
- **WHEN** the user activates (selects/opens) a file entry
- **THEN** that file is opened into the focused editor pane and focus moves to that pane

### Requirement: Toggle sidebar focus and visibility
The user SHALL be able to move focus to/from the sidebar.

#### Scenario: Focus the sidebar
- **WHEN** the user issues the focus-sidebar command
- **THEN** the sidebar becomes focused and arrow keys navigate the tree rather than editing a buffer
