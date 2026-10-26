# Changelog

All notable changes to Pythia are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Program windows: *Compile all* has a tooltip saying that it compiles the
  specification, then the body, and stops at the first error (#78).
- **Debug…** next to **Test…** on procedures, functions and package
  members: it opens the test block in a SQL window and debugs it right away,
  stopping on entry to the subprogram (any overload) without a breakpoint;
  its unit is compiled with debug information first. In a program window,
  right-clicking a subprogram's name in the spec, or a public one in the
  body, offers Test… and Debug… too (#71).

### Changed
- SQL and program windows: the editor fills its tab to the edges instead
  of sitting in a 22 px gray frame, which leaves more room for code. The
  rows above it (connection picker, Compile…) have a strip of their own,
  like the results below (#86).

### Fixed
- Windows: `pythia.exe` carries Pythia's icon and version, so the taskbar,
  the Start menu and desktop shortcuts, Explorer, Installed apps and
  associated files show Pythia's icon rather than the default one, and Task
  Manager and "Open with" list it as Pythia (#77).
- Test… on an overloaded package subprogram tests that overload, not
  always the first one: in the object browser, and in a program window,
  where the overload is the declaration right-clicked in the spec, or the
  one with the same parameters as the implementation in the body (#85).
- Clicking an entry in the completion list inserts it. The click closed the
  list without inserting anything and left the editor without focus; only
  the keyboard worked (#72).
- Session details: open cursors show their whole statement, not the first
  60 characters V$OPEN_CURSOR keeps; selecting one shows it in full below
  the list (#73).
- Session details of a session that has ended no longer show the waits,
  statistics, open cursors and locks of the session that got its SID since.
  A blocker missing from the session list (shown with its SID only in the
  blocking tree) offers no details, kill or trace with a guessed serial#,
  and isn't labelled idle (#75).
- Monitor: the object a session waits on, the row it waits for and the
  tables of TM locks are named for every schema, not only for tables the
  monitoring user has privileges on (they were looked up in ALL_OBJECTS)
  (#76).
- Trace analyzer: statements run again through a cached cursor are counted
  each time. Executions, fetches and rows after the first were dropped
  once the session cached the cursor (#79).
- MySQL errors read `ERROR 1046 (3D000): …` without an Oracle error number
  in front (`ORA-01046: ERROR 1046 …`) in SQL windows, program windows,
  object views and the table editor (#82).

## [0.4.1] - 2026-10-26

### Added
- Instant Client packages: each release also has `pythia-instantclient_…`
  packages, built on the Oracle Instant Client, for servers that require
  Native Network Encryption, Oracle 11.2 and OS or wallet logins. They need
  the Instant Client (or an Oracle Client) on the library path; see the
  README. *Help → About Pythia* shows which driver a build uses, and the
  pure-Rust build's error for such a server points to them (#66).

### Fixed
- Net service names: tnsnames.ora is also found where Oracle's own clients
  find it: `TNS_ADMIN` and the Oracle homes in the Windows registry, and
  `network/admin` of a full client (`ORACLE_HOME\bin` or `$ORACLE_HOME/lib`
  on the library path). A name Pythia can't look up says what it searched,
  not "no configuration directory specified" (#67).

## [0.4.0] - 2026-10-26

### Added
- MySQL connections (experimental): pick *Type: MySQL* when connecting.
  SQL windows, explain plan, completion, the object browser and object views
  work; tools that rely on Oracle's dictionary or PL/SQL are hidden on
  MySQL connections. `pythia_db::Dialect` says what a connection's database
  supports, and `DbRequest::Run` runs tool code on any database.
- MySQL connections through SSH, as with HeidiSQL's SSH tunnel: *Connect
  through SSH* in the connect dialog, with a password, a key file (OpenSSH
  or PuTTY `.ppk`) or the SSH agent (Pageant on Windows). All sessions of a
  connection share one SSH connection, which logs in again when it was
  lost. A server's host key has to be in `~/.ssh/known_hosts` or trusted
  once in the dialog; a changed key fails.
- MySQL monitoring dashboard (Monitor → Dashboard): findings (lock waits
  and their blocker, metadata lock waits, connections near
  `max_connections`, buffer pool hit ratio, long transactions, replication
  stopped or lagging), KPI tiles and trend charts from `SHOW GLOBAL
  STATUS`, connections by user, host and database, running statements,
  open transactions, the largest databases and tables, and the top
  statements from `performance_schema` with *Analyze*. What needs the
  `PROCESS` privilege or `SELECT` on `performance_schema` says so.

### Fixed
- When the window system went away mid-session (WSLg's compositor
  crashed), Pythia started again with OpenGL and then reported that
  neither Vulkan nor OpenGL works. It now says the window system ended
  the connection (#60).
- Settings: the Formatter page was taller than an 800 px window, which
  pushed the close button off screen. Pages scroll inside a window that
  fits the screen (#62).
- Light theme: black showed outside the tabs' rounded corners and in the
  gaps between the dock's areas and the bars; it's the theme's background
  now (#64).

## [0.3.0] - 2026-10-23

### Added
- Directories (#43), listed with their paths in the object browser below
  profiles, with New directory…, Edit directory…, Grants… (read, write and
  execute) and Drop…. The form takes the name and the path on the server
  (`CREATE OR REPLACE DIRECTORY`, which keeps the grants) and warns
  without CREATE ANY DIRECTORY. New: Directory… joins File → New Object.
- Scheduler job form (#42), from New job… on the object browser's
  Scheduler jobs folder, File → New Object, or New job… and Edit… in the
  Scheduler Jobs window. It sets what the job runs (a PL/SQL block or a
  stored procedure, picked from the schema), its start, a repeat interval
  from presets (every few minutes or hours, daily, weekly on some days,
  monthly) or typed as a calendar expression, its end, enabled, drop after
  its last run and comments, and shows the next five runs as the database
  computes them. A new job is DBMS_SCHEDULER.CREATE_JOB, a change sets the
  changed attributes; Open in SQL window ends such blocks with `/`.
- Profiles (#41), listed in the object browser below users and roles,
  with New profile…, Edit profile… and Drop…. The form sets each password
  and resource limit to default, unlimited or a value, says what default
  stands for and lists the users with the profile; a change alters the
  limits that changed. Dropping a profile gives its users DEFAULT.
- Type form: methods (member and static procedures and functions,
  constructors, a map or order method) and "not instantiable". A type with
  methods opens with its body too, a stub per method (#40).
- User and role form: quotas on any number of tablespaces, the admin
  option of granted roles and system privileges, a user's default roles,
  and object privileges (with grant option for users) to add and revoke
  (#39).
- Table editor: a Partitions tab (#37). A new table can be partitioned by
  range, interval, list or hash, with its key, partitions (and a MAXVALUE
  or DEFAULT one) and tablespaces; a copy of a partitioned table is
  partitioned the same way. An existing table shows its partitions and
  takes new ones; truncating and dropping partitions run only with Allow
  dropping on the Script tab.
- Creating objects without writing DDL (#29). The object browser's
  right-click menu offers **New …** on every folder, a **New** submenu on
  objects (a table like this one, an index, trigger or view on a table, a
  synonym for an object) and **New index… / Add constraint… / New
  trigger…** on a table's groups; File → New Object and the command palette
  have the same, for the schema the browser shows.
  - **Table builder**: the table editor starts empty (or as a copy of a
    table). Columns with a type picker, primary key and identity column
    (12c+) checkboxes, not null, default and comment; primary, unique and
    foreign keys (referenced table picked, its key filled in) and checks
    with names generated from the table's; indexes, with one click for
    unindexed foreign keys; tablespace or a global temporary table. The
    script is `CREATE TABLE` and what follows it.
  - Forms for **materialized views** (build, refresh method and schedule,
    query rewrite), **database links** (password hidden in the preview; a
    change recreates the link, as `ALTER DATABASE LINK` needs a privilege
    of its own), **users** (authentication, tablespaces, quota, profile,
    lock, roles and system privileges) and **roles**, new and existing.
    Users and roles are listed in the object browser.
  - **Triggers, procedures, functions, packages and types** start from a
    form (events and timing, parameters, return type, `AUTHID`, attributes
    or collection type); **Open in editor** hands the source to a program
    window, where compiling creates the unit. A package's body gets a stub
    per subprogram. The form warns when the name exists.
  - What was created shows up selected in the object browser.
- Result grid: drag a header onto another to move its column, and freeze
  the row numbers and the columns up to one (right-click its header) so
  they stay in place when scrolling sideways (#33). The header menu's
  Columns submenu shows or hides each column and resets the order or the
  widths. Selection, arrow keys, paste, copy and export follow the order
  shown.
- Toolbar: a Debug button next to Execute (Ctrl+F9). It is enabled only
  with the cursor in an anonymous block of a connected SQL window, and when
  disabled says why; in a program window it points to Test… for a block
  that calls the unit.
- Debugger: a record whose type is declared in a package compiled without
  debug information comes without contents (Oracle's agent hands it over
  as an opaque value). The variables view (and the value on hover) now
  says which package it is and how to get the contents.

### Changed
- **No Oracle Instant Client needed any more.** Pythia connects with a
  pure-Rust driver (our fork of rust-oracledb, pinned: upstream fixes not
  released yet, proxy logins, statements starting with a `--` comment,
  cancelling a running statement, also on TLS connections (#53), and LOB
  queries that failed once no longer failing with ORA-01008 afterwards,
  #48). Packages no longer depend on libaio.
  JSON and VECTOR columns are shown (the Instant Client driver couldn't
  fetch them). `TIMESTAMP WITH TIME ZONE` values with a region show the
  region's name (`… Europe/Zurich`, #50), and SQL literals of such values
  keep it. For TLS, the connect string can name the wallet folder with a
  private CA's certificate (`?wallet_location=`, #54). Not covered (#13): 11.2 databases, servers that require Native
  Network Encryption, and OS and wallet logins; for those, build with
  `--features pythia-db/thick` against the Instant Client.
- Table editor: renaming a column of an existing table renames it
  (`ALTER TABLE … RENAME COLUMN`), so it keeps its data, keys and indexes,
  instead of adding a new column next to the old one (#38).
- The table editor applies its script in its window (like the other
  forms) and says which statement failed; Open in SQL window is still
  there. It can add foreign keys, and its Null column is now Not null.
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
- Object browser: another schema's private database links were missing
  on 18c and 21c although their details showed; they're read from
  DBA_DB_LINKS where the user may (#51).
- The grants editor read ALL_TAB_PRIVS only, so a DBA saw none or some
  of the grants on another schema's object. It reads DBA_TAB_PRIVS when
  it can (#47).
- Drop… on a scheduler job in the object browser failed with ORA-00950
  (there is no DROP JOB); it drops the job through DBMS_SCHEDULER (#46).
- A window learned whether a transaction is open, and got the statement's
  DBMS_OUTPUT, only after the statement's result. For a moment after an
  INSERT it showed no transaction (#44).
- Closing a SQL window with an uncommitted transaction (its tab's ×,
  Close Window, Close Other Windows) rolled the transaction back without
  asking. It now asks: Commit and close, Roll back and close, or Cancel
  (#35).
- The object browser, project tree and saved connections list didn't
  highlight the selected row unless the mouse was over it (#31).
- Hiding or showing a result grid column reset the widths of all the
  others; the grid now remembers each column's width.
- Result grid columns could stay as narrow as their header when the grid
  was drawn before the first rows arrived.
- Generate body on a package spec with a schema (`create package hr.pkg`)
  ended the body with `end hr.pkg;`, which doesn't compile.
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

[Unreleased]: https://github.com/ortic/pythia/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/ortic/pythia/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/ortic/pythia/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ortic/pythia/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/ortic/pythia/releases/tag/v0.2.0
[0.1.0]: https://github.com/ortic/pythia/releases/tag/v0.1.0
