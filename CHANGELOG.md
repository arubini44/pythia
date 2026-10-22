# Changelog

All notable changes to Pythia are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Toolbar: a Debug button next to Execute (Ctrl+F9). It is enabled only
  with the cursor in an anonymous block of a connected SQL window, and when
  disabled says why; in a program window it points to Test… for a block
  that calls the unit.
- Debugger: a record whose type is declared in a package compiled without
  debug information comes without contents (Oracle's agent hands it over
  as an opaque value). The variables view (and the value on hover) now
  says which package it is and how to get the contents.

### Changed
- Auto-commit is a setting (Settings → Query, off by default) instead of a
  toolbar toggle per window. It applies to all SQL windows, including those
  already open, but never to production connections, where Commit asks
  first. Session → Toggle Auto-Commit switches the setting and shows its
  state; the status bar still says when it's on.
- SQL windows stay on the connection they were opened on. Moving one to
  another open connection (the picker above the editor and the toolbar's
  connection menu) is off by default and can be turned on in Settings →
  Query. A window without an open connection can still pick one.

### Fixed
- The connection picker above a SQL window switched connections with a
  pending transaction, dropping the window's session without asking. Like
  the toolbar's menu, it now offers other connections only after a commit
  or rollback.
- The object browser's Compile and the invalid objects window's recompile
  ran a plain `ALTER … COMPILE`, which takes the session's compiler
  settings and so dropped debug information. They keep each unit's settings
  now (`REUSE SETTINGS`), like `UTL_RECOMP` and automatic recompilation
  already did. The browser's Compile also uses the right statement for
  public synonyms and Java classes.
- Result toolbar in narrow SQL windows: the row count no longer runs into
  the buttons. The buttons get the room they need first; the filter and the
  row count share what's left (truncated, hidden when there's no room, the
  status bar has the count too), and without room for them the buttons show
  icons only, labels as tooltips.
- Charts: with many categories in a narrow chart, the x axis labelled only
  the first one. Labels now thin out evenly (every 2nd, 3rd, … category),
  as many as fit.
- Oracle 11.2 to 21c (#16, tested against 11.2 XE, 18c XE, 21c XE and
  23ai Free). Queries no longer use `FETCH FIRST` or dictionary columns
  that came with 12.1, so find object, completion and go to definition,
  find in source, the reports, the Scheduler jobs and invalid objects
  windows, the Constraints tab, table parts in the browser, schema and
  data compare, the AI's table lookups and the monitor (instance,
  sessions, top SQL, AWR) work on 11.2. What needs 18c says so before:
  *Cancel current SQL* in the monitor, the profiler, restarting a
  sequence. The monitor shows the full release from 18c on.
- Parameter types: `%ROWTYPE` parameters name their table (18c and later
  showed `ROWTYPE`), package record and collection types keep the type
  name, and on 11g a package with a parameterless procedure no longer
  breaks member completion.
- Killing a session that is busy in a call no longer drops the monitor's
  own connection (ORA-00031).
- Import: new column names are cut at 128 bytes, not characters.
- README: which Instant Client supports which database version.

## [0.2.0] - 2026-10-22

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
- Completion: the list shows up to 15 suggestions (Settings → Code
  assistance → Visible rows), as many as fit below the line, and opens
  above the line when there is more room there instead of covering it. It
  is as wide as its entries need, up to a limit; long names and details are
  cut with "…" instead of running into each other. A pane next to the list
  shows what hover shows for the selected entry: a column's type,
  nullability and comment, a subprogram's signatures and doc comment, a
  snippet's text.

### Added
- AI assistant (#1). Settings → AI configures several models at once, one
  of them the default, with a Test button: the Anthropic Messages API,
  OpenAI-compatible chat completions, local model servers (Ollama,
  llama.cpp, LM Studio; no key) and command line tools (prompt on stdin,
  answer on stdout). API keys are kept in the OS keychain. A side panel
  (View → AI Assistant) holds the conversation; questions take the focused
  window's connection and statement along. From editors (AI menu, context
  menu, palette): generate SQL from a description with the schema's real
  tables, columns and foreign keys; explain, fix (with the errors) or
  rewrite the statement under the cursor or the selection, then replace it
  or insert the answer's code; analyze the execution plan. "Analyze with
  AI" / "Fix with AI" buttons on plans, run statistics, errors, compile
  errors, top SQL, session details, AWR/ASH reports and traces. Requests
  carry table definitions, keys, indexes, statistics, plans and source;
  result rows and bind values only when allowed in Settings → AI, and each
  answer shows what was sent and what was held back. On the Claude API, a
  request Claude declines is answered by the fallback model Anthropic
  recommends for it.
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
  own line. AI: Format Unsupported Statements hands the statements the
  formatter keeps as written (unsupported constructs, failed safety checks)
  to the AI with the same rules; an answer is applied only if it changes
  nothing but whitespace and case, as one undo step.
- Database links: the object view has a Details tab (connects as, host,
  created; another schema's private link from DBA_DB_LINKS where you may
  read it) and Test link, also in the context menu, which queries over the
  link and says whether it works.
- Debugger: selecting a frame in the call stack shows its variables, and
  watches and hover values in its unit come from that frame (its package's
  globals included). Every stop selects the innermost frame again;
  breakpoint conditions are always evaluated where the breakpoint is.
- Debugger: Settings → Debugger → Compile with debug information makes
  program windows compile with debug information, so Step into enters
  those units without a breakpoint in them. Off by default, as Oracle runs
  such code less optimized. Oracle's notes about the option (PLW-06015,
  PLW-06013) aren't listed with the compile messages. Units that already
  have debug information are no longer recompiled before a debug run.
- Debugger: when Step into goes over a unit without debug information, the
  panel says so (for the first call in the session, when the unit is
  loaded; later calls can't be told apart from code without calls).

### Fixed
- Object browser: public database links and public synonyms are listed,
  under a PUBLIC entry in the schema list. Database links are also read
  from ALL_DB_LINKS, for versions whose ALL_OBJECTS leaves them out. Drop
  works for public synonyms and links and for links with dots in their
  name.
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
  `DBMS_SESSION.SLEEP`), the editor shows the nearest frame with source,
  and the variables and watches are that frame's.
- Debugger: clicking a frame in the call stack didn't open its source.
- Debugger: globals of a package the session hasn't instantiated yet (e.g.
  a spec loaded for its types, whose constants the compiler inlined where
  they are read) read as NULL. Oracle's agent has no values for them until
  the package is first used; the watch now says so. NULL globals of an
  instantiated package still read as NULL.
- Completion: after a short list (e.g. filtered down to three matches),
  later lists no longer stay that small until the app restarts.

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

[Unreleased]: https://github.com/ortic/pythia/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/ortic/pythia/releases/tag/v0.2.0
[0.1.0]: https://github.com/ortic/pythia/releases/tag/v0.1.0
