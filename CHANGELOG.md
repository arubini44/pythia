# Changelog

All notable changes to Pythia are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed
- Toolbar: follows the focused window. Commit and Rollback are only enabled
  with a pending transaction (and then stand out), never open a session,
  and say when auto-commit is on; Execute becomes Compile in program windows
  and is disabled where nothing runs; disabled buttons say why. New: a
  labelled New SQL button with Open file/Reopen closed, Save, a connection
  selector (switch this window's connection, connect, disconnect), an
  auto-commit toggle and Format.
- Object views: views and code objects open on their DDL (tables still on
  their columns), and the next object of a type opens on the tab you chose
  last. Only the shown tab loads, on a metadata session of its own, so
  views and edit forms no longer wait behind the object browser,
  completion or compiles. A view's columns skip the primary key lookup,
  and its edit form takes WITH READ ONLY / WITH CHECK OPTION from the
  view's text where the database keeps it there (23ai) instead of
  searching all constraints. `RUST_LOG=pythia_db=debug` logs how long each
  metadata query took.

### Added
- Editors: a context menu. On a database object it offers what the object
  browser offers (query, edit, test, describe, edit table, grants, generate,
  compile, favorites, copy name) and Show in object browser; on a local
  name, go to declaration and find references; plus cut, copy, paste,
  select all, toggle comment and format. On a package member (`pkg.proc`)
  Edit and Ctrl+click open the package at that subprogram.
- Invalid objects: the status bar shows how many there are (current schema
  or all user schemas, refreshed on connect, after compiles and DDL and on
  an interval). Clicking it opens a window listing them with their first
  error; it recompiles selected or all objects in dependency order over
  several sessions (Settings → Invalid objects), with progress, cancel and
  what is still invalid afterwards, and marks objects you may not compile.
  SYSDBA connections can use UTL_RECOMP. The browser's recompile button
  uses it too.
- Navigation in packages: in a program window the body knows the spec's
  declarations and the other way round (go to definition, hover,
  completion, references), Ctrl+click on a call picks the overload its
  arguments fit, a named argument (`p_id =>`) leads to the callee's
  parameter, labels (`exit outer_loop`, `goto done`) are navigable, and
  hover shows the comment above a declaration. New commands (palette,
  Settings → Keyboard, editor menu): Back/Forward (Alt+Left/Right, mouse
  buttons), Switch Spec/Body (Ctrl+Shift+B), Go to Symbol (Ctrl+F12), Peek
  Definition (Alt+F12) and Find References (Shift+F12) as a list to step
  through.
- Debugger: step through the debugged block itself (it now has debug
  information), and stepping out of the outermost unit continues there; the
  SQL window marks the block's line.
- Debugger: the call stack names the subprogram (`orders_api.load_order:31`)
  and the package initialization section.
- Formatter: profiles (named rule sets to switch between, export and import
  as TOML to share a team style), learning the rules from the focused
  editor or the current schema's stored source (the differences are shown
  with the preview before you apply them), and formatting automatically on
  save and compile, on paste and when a statement's `;` is typed (only
  what changes is replaced, as one undo step, and the caret stays put).
  New rules: CASE arms, subqueries, parameter lists and INSERT column and
  VALUES lists each fit on one line or always break, and THEN/LOOP on their
  own line.

### Fixed
- Windows: no console window with log output next to the app (also none for
  `git` and other helper programs). Started from a terminal, the logs still
  show there. Problems at startup are shown in a message box, and About has
  an "Open log folder" button.
- Connect dialog: without the Oracle Instant Client it lists the steps for
  this platform, with the download page as a working link (copying the old
  one picked up a trailing comma and led to a 404) and the Visual C++
  redistributable on Windows.
- Debugger: a step no longer stops again on the line it started from.
- Debugger: a step that ended at a breakpoint no longer stops later, after
  continuing, where the step would have ended.
- Debugger: breakpoints reached while stepping count their hits; stepping
  from a breakpoint doesn't count it twice.
- Debugger: records in index-by tables show their fields, elements are named
  by their key (`('L1')`), and watches accept text keys.
- Debugger: SQL collection types show as not readable instead of NULL.
- Debugger: after pausing in code without debug information (e.g.
  `DBMS_SESSION.SLEEP`), the editor shows the nearest frame with source.

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
- Code folding, numbered bookmarks and snippets with placeholders; folds
  and bookmarks are saved with the workspace.
- Rename a local symbol in place (Shift+F6).
- SQL\*Plus commands get their own highlighting.
- Files keep their encoding (UTF-8, UTF-8 BOM, Windows-1252) and line
  endings.
- Configurable keyboard shortcuts (Settings → Keyboard) and a code font
  from a font file.

**SQL windows**
- Execute the statement under the cursor, the selection or a script;
  SQL\*Plus `exec`, `prompt`, `define`, `desc`; bind and substitution
  variable prompts; DBMS_OUTPUT; statement history.
- Virtualized result grid with sorting, selection, aggregates, record view,
  value viewer and export to CSV, TSV, JSON, Excel, INSERT, Markdown and HTML.
- In-place editing of results with optimistic concurrency checks.
- Explain plan as a tree and "Run with statistics" with the actual plan and
  session statistics.
- Bar and line charts for any result.
- Foreign key navigation: open a row's parent or child rows.
- Hide columns; paste Excel ranges into editable results; edit long text
  and dates (with a calendar) in a dialog.
- Execute a query straight into a file (CSV, TSV, JSON, INSERT, Markdown)
  with progress and cancel.

**PL/SQL development**
- Object browser, object views (columns, constraints, indexes, grants,
  dependencies, DDL), program windows with spec/body and compile errors,
  generated statements, test scripts, recompile invalid objects.
- Object browser: favorites, recently opened objects, filters for invalid
  or recently changed objects, indexes/constraints/triggers/referencing
  tables under tables, drag names into editors.
- Program windows: Update spec copies changed signatures from the body.
- Form editors for sequences, views, synonyms, indexes and grants.
- PL/SQL debugger over DBMS_DEBUG_JDWP: breakpoints, stepping, call stack and
  variables including records and collections.
- Debugger: watches, values on hover, conditional and hit-count
  breakpoints, enabling/disabling breakpoints, break on exceptions.
- Profiler (DBMS_HPROF) for blocks and calls.
- Find in database source with regular expressions.
- Find in source searches views and job actions and filters by kind and
  name; data search finds columns by name.

**Tools**
- Monitoring: dashboard with findings and live charts, sessions with a
  blocking tree, session details, kill/cancel, AWR/ASH reports (when
  licensed) and a tkprof-style trace analyzer.
- Schema compare with structural table diffs and ordered sync scripts; data
  compare by key.
- Import from CSV and Excel with type inference into new or existing tables.
- Import: per-column trim, date format and SQL expressions; skip and log
  bad rows.
- Export tables tool with a WHERE per table, DDL, delimiter and NULL text.
- Project panel: folders under git with status, diff with the last commit
  or with the database, schema export to one file per object, deployment
  of changed files.
- Scheduler jobs window: run, stop, enable, disable, drop, run history.
- Saved user reports with parameters.

[Unreleased]: https://github.com/ortic/pythia/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ortic/pythia/releases/tag/v0.1.0
