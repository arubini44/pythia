# Changelog

All notable changes to orui are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

**Editor**
- Code folding, numbered bookmarks and snippets with placeholders; folds
  and bookmarks are saved with the workspace.
- Rename a local symbol in place (Shift+F6).
- SQL\*Plus commands get their own highlighting.
- Files keep their encoding (UTF-8, UTF-8 BOM, Windows-1252) and line
  endings.
- Configurable keyboard shortcuts (Settings → Keyboard) and a code font
  from a font file.

**Results**
- Bar and line charts for any result.
- Foreign key navigation: open a row's parent or child rows.
- Hide columns; paste Excel ranges into editable results; edit long text
  and dates (with a calendar) in a dialog.
- Execute a query straight into a file (CSV, TSV, JSON, INSERT, Markdown)
  with progress and cancel.

**Tools**
- Project panel: folders under git with status, diff with the last commit
  or with the database, schema export to one file per object, deployment
  of changed files.
- Profiler (DBMS_HPROF) for blocks and calls.
- Scheduler jobs window: run, stop, enable, disable, drop, run history.
- Form editors for sequences, views, synonyms, indexes and grants.
- Export tables tool with a WHERE per table, DDL, delimiter and NULL text.
- Import: per-column trim, date format and SQL expressions; skip and log
  bad rows.
- Saved user reports with parameters.
- Find in source searches views and job actions and filters by kind and
  name; data search finds columns by name.
- Object browser: favorites, recently opened objects, filters for invalid
  or recently changed objects, indexes/constraints/triggers/referencing
  tables under tables, drag names into editors.
- Debugger: watches, values on hover, conditional and hit-count
  breakpoints, enabling/disabling breakpoints, break on exceptions.
- Program windows: Update spec copies changed signatures from the body.

### Fixed
- DBMS_OUTPUT of a debugged block reaches the Output tab.
- Light theme: text fields in dialogs were white on white.
- ORA-01805 (time zone regions with Instant Client Basic Light) is
  explained.

## [0.1.0] - 2026-10-22

First release.

### Added

**Application**
- Native egui/wgpu application with an OpenGL fallback, dockable layout,
  command palette (Ctrl+Shift+P), PL/SQL Developer compatible keys, dark and
  light themes, settings window, workspace and editor contents restored on
  start, crash logging.
- Multiple simultaneous connections; every window has its own session and
  transaction. Connection manager with folders, colors and production
  markers, read-only connections, passwords in the OS keychain,
  `tnsnames.ora` aliases.
- Open and save SQL/PL/SQL files, drag and drop files, reopen closed windows.

**Editor**
- Rope-backed code editor: virtualized rendering, multi-cursor, block
  selection, find/replace with regular expressions, bracket and keyword
  matching, auto-indent, comment toggling, case conversion, line moves.
- Error-tolerant PL/SQL parser and language engine: completion of columns
  (aliases, joins, CTEs, inline views), package members, records, locals and
  dictionary objects; signature help, hover, go to definition, references,
  live syntax diagnostics and an outline.
- Code formatter (Ctrl+Shift+F) with configurable rules and a live preview.

**SQL windows**
- Execute the statement under the cursor, the selection or a script;
  SQL\*Plus `exec`, `prompt`, `define`, `desc`; bind and substitution
  variable prompts; DBMS_OUTPUT; statement history.
- Virtualized result grid with sorting, selection, aggregates, record view,
  value viewer and export to CSV, TSV, JSON, Excel, INSERT, Markdown and HTML.
- In-place editing of results with optimistic concurrency checks.
- Explain plan as a tree and "Run with statistics" with the actual plan and
  session statistics.

**PL/SQL development**
- Object browser, object views (columns, constraints, indexes, grants,
  dependencies, DDL), program windows with spec/body and compile errors,
  generated statements, test scripts, recompile invalid objects.
- PL/SQL debugger over DBMS_DEBUG_JDWP: breakpoints, stepping, call stack and
  variables including records and collections.
- Find in database source with regular expressions.

**Tools**
- Monitoring: dashboard with findings and live charts, sessions with a
  blocking tree, session details, kill/cancel, AWR/ASH reports (when
  licensed) and a tkprof-style trace analyzer.
- Schema compare with structural table diffs and ordered sync scripts; data
  compare by key.
- Import from CSV and Excel with type inference into new or existing tables.

[Unreleased]: https://github.com/ortic/orui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ortic/orui/releases/tag/v0.1.0
