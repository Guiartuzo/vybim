# syntax-highlighting

## Purpose

tree-sitter-based syntax highlighting of editor panes, mapping syntax to themed colors, with a plain-text fallback for unsupported languages and re-highlighting that keeps typing responsive.

## Requirements

### Requirement: tree-sitter syntax highlighting
Editor panes SHALL highlight buffer contents using tree-sitter for languages with an available grammar, mapping syntax nodes to themed colors.

#### Scenario: Highlight a supported language
- **WHEN** a file whose language has a bundled grammar is displayed
- **THEN** tokens are colored according to their syntactic role (keywords, strings, comments, etc.)

#### Scenario: Unsupported language falls back to plain text
- **WHEN** a file has no available grammar
- **THEN** the file is rendered as readable plain text without highlighting and without error

### Requirement: Incremental re-highlight on edit
Highlighting SHALL update to reflect edits without re-parsing the entire buffer on every keystroke where the grammar supports incremental parsing.

#### Scenario: Edit updates highlighting
- **WHEN** the user edits a highlighted buffer
- **THEN** the affected region's highlighting updates to reflect the new text

#### Scenario: Highlighting does not block typing
- **WHEN** the user types rapidly in a large highlighted file
- **THEN** input remains responsive and highlighting catches up without freezing the editor
