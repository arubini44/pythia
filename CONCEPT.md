# orui – Oracle PL/SQL IDE in Rust

A native, fast, cross-platform alternative to Allround Automations' PL/SQL Developer,
written in Rust with `egui` on the `wgpu` renderer.

---

## 1. Vision & Goals

- **Fast**: starts in under a second, stays responsive with large schemas (100k+ objects) and large result sets (millions of rows, virtualized).
- **Native & cross-platform**: Linux, Windows, macOS from one codebase. No JVM, no Electron.
- **Keyboard-first**: everything reachable via shortcuts and a command palette, with familiar PL/SQL Developer keybindings as the default.
- **Safe by default**: clear transaction state, production-connection warnings, confirmation for destructive DDL/DML.
- **Scriptable & extensible**: plugin/scripting hooks for custom tools later on.

### Non-Goals (for now)

- Supporting databases other than Oracle (keep the DB layer abstract enough to add them later).
- A full DBA console (RMAN, Data Guard, ASM management).
- A web or remote UI.

---

## 2. Technology Stack

| Area | Choice | Notes |
|---|---|---|
| Language | Rust (stable, edition 2024) | Cargo workspace |
| UI | `egui` + `eframe` | Immediate mode |
| Renderer | `wgpu` (via `eframe` wgpu backend) | Vulkan / Metal / DX12 / GL fallback |
| Docking | `egui_dock` | Tabbed, dockable panels |
| Tables | `egui_extras::TableBuilder` or a custom virtualized grid | Must handle millions of rows |
| Oracle driver | `oracle` crate (ODPI-C, needs Instant Client) | Look at `sibyl` for async. Thin-mode driver as a long-term option |
| Async runtime | `tokio` | DB work runs off the UI thread, results come back over channels |
| Text buffer | `ropey` | For large files and cheap edits |
| Lexing/parsing | Custom PL/SQL lexer + incremental parser (`logos` for lexing) | Or evaluate a tree-sitter PL/SQL grammar |
| Diffing | `similar` | Code and DDL compare |
| Search | `regex`, `grep-searcher`/`ignore`, `tantivy` (optional) | Local code index |
| Export formats | `csv`, `rust_xlsxwriter`, `calamine` (xlsx read), `serde_json`, `quick-xml` | |
| Config | `serde` + `toml`, `directories` | |
| Secrets | `keyring` | OS keychain for passwords |
| Logging | `tracing` + `tracing-subscriber` | |
| Testing | `cargo test`, `insta` (snapshots), Oracle Free in Docker (`gvenzl/oracle-free`) for integration tests | |

---

## 3. Architecture

```
orui/
├── crates/
│   ├── orui-app          # eframe entry point, window, docking layout, command palette
│   ├── orui-ui           # reusable widgets: code editor, data grid, tree view, diff view
│   ├── orui-db           # connection mgmt, sessions, query execution, metadata queries
│   ├── orui-plsql        # lexer, parser, AST, symbol tables, formatter
│   ├── orui-lsp-core     # completion, hover, go-to-definition, diagnostics (no UI)
│   ├── orui-debugger     # DBMS_DEBUG_JDWP / DBMS_DEBUG client, debug session state machine
│   ├── orui-data         # import/export, data compare, data generator
│   ├── orui-compare      # schema/DDL compare and sync script generation
│   ├── orui-search       # code search index, data search
│   └── orui-config       # settings, profiles, keybindings, themes
└── tests/                # integration tests against Oracle Free container
```

### Core principles

- [x] UI thread never blocks: every DB call runs in a worker task, the UI gets messages over channels (`DbRequest` / `DbEvent`)
- [ ] Each editor tab can own a **dedicated session** or share one (like PL/SQL Developer's session modes: multi-session, dual-session, single-session)
- [ ] Every long-running operation can be cancelled (`OCIBreak` / `Connection::break_execution`)
- [ ] Metadata cache per connection with incremental refresh (based on `LAST_DDL_TIME`)
- [x] Command pattern for all user actions, so we get keybindings, the command palette and macros from one place

---

## 4. Milestones

| Milestone | Scope |
|---|---|
| **M0 – Skeleton** | Workspace, eframe+wgpu window, docking, settings, logging |
| **M1 – Connect & Query** | Connection manager, SQL window, result grid, transactions |
| **M2 – Editor** | Custom code editor, syntax highlighting, basic completion |
| **M3 – Object Browser** | Object tree, view/edit DDL, compile, errors |
| **M4 – Program Units** | Program window, compile with errors, PL/SQL parser and outline |
| **M5 – Data Tools** | Export, import, data search, editable grid |
| **M6 – Debugger** | Breakpoints, stepping, variable inspection |
| **M7 – Compare** | Schema compare, data compare, sync scripts |
| **M8 – Productivity** | Code search, refactoring, formatter, templates, explain plan |
| **M9 – Polish & Release** | Packaging, installers, docs, performance work |

---

## 5. Features

### 5.1 Application Shell (M0)

- [ ] Cargo workspace with crates listed above
- [x] `eframe` app with the `wgpu` backend, GL fallback
- [ ] Dockable layout (`egui_dock`): object browser, editors, output, results, debugger panels
- [x] Save/restore layout and open tabs per workspace
- [x] Main menu, toolbar, status bar (connection, session, transaction state, cursor pos, encoding)
- [x] Command palette (Ctrl+Shift+P) with fuzzy search over all commands
- [ ] Configurable keybindings; ship a PL/SQL Developer compatible preset
- [ ] Light/dark themes, custom theme files (TOML)
- [ ] Font selection with ligatures off by default, HiDPI scaling
- [ ] Settings dialog (global, per connection, per project)
- [x] Crash handler and panic logging
- [ ] Single-instance mode with file open forwarding (optional)

### 5.2 Connection Management (M1)

- [ ] Connection dialog: user/password, TNS alias, EZConnect, full descriptor
- [ ] Read `tnsnames.ora` / `sqlnet.ora` (`TNS_ADMIN`, `ORACLE_HOME`)
- [ ] Wallet / external authentication (`/@alias`)
- [x] Connect as SYSDBA / SYSOPER / SYSBACKUP
- [ ] Proxy user connect (`user[proxy]`)
- [ ] Saved connections organized in folders, with colors and tags (e.g. red = production)
- [ ] Passwords in the OS keychain (`keyring`), never in plain text
- [ ] Production safety flags: read-only mode, confirm on commit, confirm DML/DDL
- [ ] Multiple simultaneous connections
- [ ] Session modes: multi-session / dual-session / single-session
- [ ] Connection health check, auto-reconnect prompt
- [ ] Set NLS settings, `CURRENT_SCHEMA`, module/action/client info on connect
- [ ] Login script per connection (like `login.sql`)
- [x] Detect Instant Client and give a helpful message if it's missing

### 5.3 Code Editor Widget (M2)

Custom widget, because egui's `TextEdit` doesn't scale to large files or IDE features.

- [ ] Rope-backed buffer (`ropey`) with undo/redo history (grouped edits)
- [ ] Virtualized rendering (only visible lines laid out), smooth scrolling
- [ ] Line numbers, gutter with markers (breakpoints, errors, bookmarks, folding)
- [ ] Multi-cursor and column/block selection (Alt+drag)
- [ ] Code folding (blocks, `BEGIN/END`, `IF`, `LOOP`, `CASE`, comments)
- [ ] Bracket and keyword matching (`BEGIN`↔`END`, `IF`↔`END IF`)
- [ ] Find/replace in file with regex, whole word, case sensitivity, in-selection
- [ ] Go to line, bookmarks (numbered like PL/SQL Developer: Ctrl+Shift+0..9)
- [ ] Auto-indent and smart tab
- [ ] Keyword case conversion (upper/lower/init cap) live or on demand
- [ ] Comment/uncomment selection
- [ ] Minimap (optional)
- [ ] Whitespace / EOL visualization
- [ ] Encoding support (UTF-8, Windows-1252, ...) and line ending detection
- [ ] Large file mode (>50MB): reduced features, still responsive
- [ ] Split view of the same buffer
- [ ] Clipboard with rich and plain text
- [ ] IME / Unicode input support

### 5.4 Syntax Highlighting & Language Intelligence (M2/M4)

- [ ] PL/SQL + SQL lexer (`logos`): keywords, identifiers, quoted identifiers, strings incl. `q'[...]'`, numbers, comments, hints `/*+ */`, bind variables, substitution variables `&var`
- [ ] Highlighting for SQL*Plus commands (`SET`, `PROMPT`, `@`, `DEFINE`, `/`)
- [ ] Incremental re-lexing on edit (only dirty lines)
- [ ] Recursive-descent PL/SQL parser producing an AST with error recovery
- [x] Statement splitting (handles `/`, `;`, PL/SQL blocks, `CREATE ... AS` units)
- [ ] Code outline / structure view (package specs, bodies, procedures, functions, types, cursors, variables)
- [ ] Code completion:
  - [ ] Keywords
  - [ ] Schema objects (tables, views, packages, synonyms) from metadata cache
  - [ ] Columns after `alias.` with alias resolution from `FROM`/`JOIN`
  - [ ] Package members after `pkg.`
  - [ ] Local variables, parameters, record fields
  - [ ] Procedure parameter hints (signature help)
- [ ] Hover info: object type, column datatype, parameter list, comments
- [ ] Go to definition (Ctrl+click), including into package bodies on the DB
- [ ] Find references within the file and across the schema (via `ALL_IDENTIFIERS` / PL/Scope when enabled)
- [ ] Live diagnostics: parse errors, unknown identifiers (optional)
- [ ] Hyperlinks for object names (open in object browser)
- [ ] Code templates / snippets with placeholders (`sel` → `SELECT * FROM ...`)
- [ ] Code formatter (PL/SQL Beautifier equivalent) with configurable rules
- [ ] Code assistant settings: delay, case, min chars

### 5.5 SQL Window (M1)

- [x] Execute statement under cursor / selection / whole script (F8 / Ctrl+Enter)
- [x] Multiple result sets in tabs
- [ ] Bind variable prompt dialog (remember last values)
- [ ] Substitution variable prompt (`&var`, `&&var`)
- [x] Execution timing, rows affected, DBMS_OUTPUT capture
- [x] Cancel running statement
- [x] Auto-commit toggle, commit/rollback buttons, pending transaction indicator
- [ ] Warn on closing with an open transaction
- [ ] Query history per connection (searchable, persisted)
- [ ] Fetch modes: first N rows, fetch next page, fetch all
- [ ] "Execute to file" for huge results
- [ ] Error highlighting: jump to the error position in the statement
- [ ] Explain plan for current statement (F5)

### 5.6 Result Grid / Data Grid (M1/M5)

- [ ] Virtualized grid for millions of rows (row and column virtualization)
- [ ] Column resize, reorder, hide, freeze
- [ ] Sort and filter client-side; option to push sort/filter into SQL
- [ ] Data type aware rendering: NULL distinct, dates per NLS format, numbers aligned right
- [ ] LOB viewers: CLOB text, BLOB hex/image, XMLType, JSON pretty view
- [ ] Single record view (transposed)
- [ ] Editable grid (`SELECT ... FOR UPDATE` / ROWID based): insert, update, delete rows
- [ ] Cell editors per type (date picker, number, LOB editor dialog)
- [ ] Copy as: TSV, CSV, INSERT statements, Markdown, JSON, with headers
- [ ] Paste from clipboard into grid (Excel ranges)
- [ ] Cell, row, and column selection; aggregates of selection (sum, count, avg, min, max) in the status bar
- [ ] Find in grid
- [ ] Highlight changed cells before commit
- [ ] Foreign key navigation: jump to parent/child rows

### 5.7 Program Window (M4)

- [ ] Open/edit packages, procedures, functions, types, triggers, Java sources
- [ ] Spec and body in one window with tabs
- [ ] Compile (F8) with error list, click error → jump to line
- [ ] Compile with debug info (`PLSQL_OPTIMIZE_LEVEL`, `ALTER ... COMPILE DEBUG`)
- [ ] Show compiler warnings (`PLSQL_WARNINGS`)
- [ ] Detect when the DB version changed since it was opened (conflict warning)
- [ ] Save to file / load from file
- [ ] Recompile invalid objects (schema-wide, dependency order)

### 5.8 Test Window (M4/M6)

- [ ] Generate a test script for a procedure/function (anonymous block with bind variables for params)
- [ ] Variable grid: name, type, value, in/out
- [ ] Run, run with debugger
- [ ] Show DBMS_OUTPUT and out parameters, ref cursors as result grids
- [ ] Save/load test scripts (`.tst`), compatible with PL/SQL Developer format if feasible

### 5.9 Command Window (M5)

- [ ] SQL*Plus-like command window
- [ ] Support common SQL*Plus commands: `SET`, `SHOW`, `DESC`, `SPOOL`, `PROMPT`, `DEFINE`, `@`, `@@`, `EXEC`, `VAR`, `PRINT`, `CONNECT`, `WHENEVER`
- [ ] Run deployment scripts with progress and stop-on-error
- [ ] Spool output to file

### 5.10 Object Browser / Code Browser (M3)

- [ ] Tree: connection → schema → object type → objects
- [ ] Object types: tables, views, materialized views, sequences, synonyms, packages, procedures, functions, types, triggers, indexes, constraints, DB links, jobs/scheduler, queues, directories, users, roles, profiles
- [ ] "My objects" / all schemas / filter by schema
- [ ] Filters: name pattern, status (invalid), modified since, custom saved filters
- [ ] Invalid objects highlighted
- [ ] Context menu: view, edit, compile, drop, rename, describe, query data, generate DDL, copy name, grant
- [ ] Expand table → columns, indexes, constraints, triggers, FKs, references-by
- [ ] Expand package → procedures/functions with parameters (outline from parser)
- [ ] Dependencies view (uses / used by)
- [ ] Drag object name into the editor (with options: name, `SELECT`, column list, call template)
- [ ] Recent objects and favorites
- [ ] Quick object finder (Ctrl+Shift+O style fuzzy search)
- [ ] Metadata cache with background refresh

### 5.11 Object Editors (M3/M5)

- [ ] View and generate DDL (`DBMS_METADATA`) with options (storage, tablespace, constraints)
- [ ] Table editor: columns, keys, indexes, checks, comments, partitions → generates `ALTER` script
- [ ] Sequence, synonym, view, index, user, role editors
- [ ] Grants editor
- [ ] Preview generated DDL before applying

### 5.12 Debugger (M6)

- [ ] Research: implement `DBMS_DEBUG_JDWP` (JDWP protocol client in Rust, supported going forward) and/or `DBMS_DEBUG` (deprecated, but what PL/SQL Developer uses) – decide on a primary backend
- [ ] Privilege check and setup assistant (`DEBUG CONNECT SESSION`, `DEBUG ANY PROCEDURE`, ACL for JDWP)
- [ ] Debug session lifecycle: target session + debugger session
- [ ] Breakpoints in gutter: line, conditional, hit count, enable/disable
- [ ] Breakpoints persisted per object
- [ ] Step into / over / out, run to cursor, continue, abort
- [ ] Break on exception (all / specific ORA codes)
- [ ] Call stack panel with navigation
- [ ] Variables panel: locals, parameters, package globals
- [ ] Inspect records, collections, objects (tree view)
- [ ] Watches with expressions
- [ ] Modify variable values while paused
- [ ] Hover over variable shows value while paused
- [ ] DBMS_OUTPUT during debugging
- [ ] Auto-compile with debug info before starting
- [ ] Debug triggers and code called from other sessions (attach mode)

### 5.13 Code Search (M8)

- [ ] Search source across schemas (`ALL_SOURCE`) with regex, case, whole word
- [ ] Filter by object type, schema, name pattern
- [ ] Also search views (`ALL_VIEWS.TEXT`), triggers, job definitions
- [ ] Results grouped by object with line context; click → open at line
- [ ] Optional local index (`tantivy`) for instant search across large schemas
- [ ] Search in local files / project folders
- [ ] Replace across files (local files only, with preview)

### 5.14 Data Search (M5)

- [ ] Search a value across all tables/columns in selected schemas
- [ ] Filter by column data type (strings, numbers, dates), table name patterns
- [ ] Match modes: exact, contains, regex (`REGEXP_LIKE`), case-insensitive
- [ ] Run in parallel with throttling, show progress, cancellable
- [ ] Results: table, column, matching row count; click → open query with filter
- [ ] Find columns by name across schemas

### 5.15 Data Compare (M7)

- [ ] Compare tables between two connections/schemas (same or different DB)
- [ ] Key selection: PK, unique key, or user-defined columns
- [ ] Column mapping and exclusion
- [ ] Row-level diff: only in source, only in target, different (cell-level highlight)
- [ ] Filters (WHERE clause per table)
- [ ] Streamed comparison of sorted sets for large tables (avoid loading everything into memory)
- [ ] Generate sync script (INSERT/UPDATE/DELETE) or apply directly
- [ ] Save compare profiles

### 5.16 Schema / Object Compare (M7)

- [ ] Compare object definitions between two schemas/DBs
- [ ] Normalize DDL before compare (whitespace, storage clauses, schema prefix)
- [ ] Side-by-side diff view with inline highlighting
- [ ] Generate migration script in dependency order
- [ ] Compare DB object vs local file (version control workflow)

### 5.17 Data Export (M5)

- [ ] Export query result or tables to CSV, TSV, XLSX, JSON, XML, HTML, Markdown, SQL INSERT statements
- [ ] Export tables tool: multiple tables, WHERE per table, include DDL
- [ ] Options: delimiter, quoting, header, date/number formats, NULL representation, encoding
- [ ] Streaming export (no full result in memory)
- [ ] LOB handling: inline, separate files, skip
- [ ] Export user objects (DDL for a schema) to files, one file per object
- [ ] Progress and cancel

### 5.18 Data Import (M5)

- [ ] Import from CSV/TSV, XLSX, JSON
- [ ] Preview with detected types; column mapping to target table
- [ ] Create target table from file structure
- [ ] Transformations: trim, date format, constant values, SQL expressions
- [ ] Batch inserts via array DML, configurable commit interval
- [ ] Error handling: stop, skip row, log bad rows to file
- [ ] Run SQL scripts as import (large `INSERT` scripts)
- [ ] Optional: SQL*Loader control file generation

### 5.19 Explain Plan & Performance (M8)

- [ ] Explain plan as a tree grid (cost, cardinality, bytes, predicates)
- [ ] Actual execution plan via `DBMS_XPLAN.DISPLAY_CURSOR` with `GATHER_PLAN_STATISTICS`
- [ ] SQL Monitor report viewer (if licensed)
- [ ] Statistics of the last execution (`V$MYSTAT` deltas: consistent gets, physical reads, ...)
- [ ] PL/SQL profiler (`DBMS_HPROF`) with result view, hot lines in the editor
- [ ] Trace file helper (enable 10046 for session)

### 5.20 Session & Database Monitoring (M8)

- [ ] Session browser (`V$SESSION`): filters, current SQL, waits, locks, open cursors
- [ ] Kill session / cancel SQL (with confirmation)
- [ ] Lock tree / blocking sessions
- [ ] Long operations (`V$SESSION_LONGOPS`)
- [ ] Tablespace usage overview
- [ ] DBMS_SCHEDULER jobs: list, run, enable/disable, history

### 5.21 Refactoring & Productivity (M8)

- [ ] Rename local variable / parameter
- [ ] Extract procedure from selection
- [ ] Generate: column list, `INSERT`/`UPDATE` template for table, record type, `%ROWTYPE` declarations
- [ ] Generate package body stubs from spec
- [ ] Synchronize spec and body signatures
- [ ] Macro recorder (replay command sequences)
- [ ] Editor tabs: pin, reorder, close others, reopen closed
- [ ] Project / workspace: group files, objects, and connections

### 5.22 Version Control Integration (M8/M9)

- [ ] Projects map to folders, objects saved as files (`.pks`, `.pkb`, `.sql`, ...)
- [ ] Git status in file tree (via `gix`)
- [ ] Diff working copy vs HEAD, vs DB version
- [ ] Deploy changed files to a connection

### 5.23 Reports (M9)

- [ ] Saved parameterized SQL reports
- [ ] Built-in reports (invalid objects, object sizes, grants, ...)
- [ ] Export report to HTML/XLSX
- [ ] Simple charts for query results (bar/line) – optional

### 5.24 Extensibility (Later)

- [ ] Plugin API (WASM via `wasmtime` or Lua via `mlua`)
- [ ] User-defined tools (external commands with variables like `$OBJECT`, `$FILE`)
- [ ] Custom menu entries and toolbar buttons
- [ ] Optional AI assistant integration (explain SQL, generate queries) – opt-in, never sends data without consent

---

## 6. Non-Functional Requirements

### Performance
- [ ] Cold start < 1s (excluding DB connect)
- [ ] Typing latency < 16ms in a 50k line package body
- [ ] Scroll through 1M rows at 60fps
- [ ] Object browser for a 100k object schema loads lazily
- [ ] Benchmarks in CI (`criterion`) for lexer, parser, grid rendering

### Reliability & Safety
- [ ] Never lose unsaved editor content: autosave to a recovery folder
- [ ] Transaction state always visible; warn before disconnect/exit with uncommitted work
- [ ] All destructive actions confirm, with extra confirm on production-flagged connections
- [ ] No passwords in logs, history, or crash reports

### Quality
- [ ] Unit tests for lexer, parser, formatter (snapshot tests with `insta`)
- [ ] Integration tests against Oracle Free 23ai in Docker
- [ ] CI: build + test on Linux, Windows, macOS
- [ ] `clippy` and `rustfmt` enforced

### Packaging
- [ ] Windows MSI/installer, macOS `.dmg` (signed/notarized), Linux AppImage / `.deb` / Flatpak
- [ ] Bundled or guided Instant Client installation
- [ ] Auto-update check (optional, opt-in)

### Accessibility & i18n
- [ ] Full keyboard navigation
- [ ] AccessKit support (egui has it built in)
- [ ] Translatable UI strings (English first, German second)

---

## 7. Risks & Open Questions

- [ ] **Debugger protocol**: `DBMS_DEBUG` is deprecated, `DBMS_DEBUG_JDWP` needs a JDWP client implementation and network ACLs. Spike both early (before M6) to check feasibility
- [ ] **Instant Client dependency**: `oracle` crate needs ODPI-C + Instant Client. Is a pure-Rust thin driver (TNS protocol) worth it long-term?
- [ ] **Editor widget effort**: a custom high-quality code editor in egui is a big task. Evaluate existing crates (`egui_code_editor`, `egui-textedit` extensions) vs building our own
- [ ] **PL/SQL grammar coverage**: the grammar is huge; aim for a "good enough" error-tolerant parser for outline, completion, and navigation rather than a full validating parser
- [ ] **egui text rendering**: check font fallback, CJK, and subpixel quality on all platforms
- [ ] **Licensing**: choose a license (MIT/Apache-2.0 vs commercial); check Oracle Instant Client redistribution terms
- [ ] **File format compatibility**: should we import PL/SQL Developer `.tst`, `.sql`, preferences, and connection lists?

---

## 8. First Steps

- [x] Set up the Cargo workspace and CI
- [x] eframe + wgpu window with egui_dock layout
- [ ] Spike: connect with the `oracle` crate, run a query on a worker thread, show the result in a virtualized grid
- [ ] Spike: minimal custom code editor with ropey and a PL/SQL lexer for highlighting
- [ ] Spike: `DBMS_DEBUG` and `DBMS_DEBUG_JDWP` proof of concept (set breakpoint, step, read a variable)
- [x] Docker compose file with Oracle Free for local dev and tests
