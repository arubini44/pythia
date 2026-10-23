# Pythia

A fast, native Oracle PL/SQL IDE written in Rust (egui on wgpu). It covers the
daily work of an Oracle developer — writing and running SQL, editing and
compiling PL/SQL, debugging, browsing, editing data, comparing schemas and
watching what the database is doing — in one responsive, keyboard-first app
for Linux, Windows and macOS.

![SQL window with object browser and result grid](docs/screenshots/sql-window.png)

## Features

**Editor**
- Rope-backed code editor built for large packages: syntax highlighting,
  multi-cursor, block selection, find/replace with regex, bracket and
  `BEGIN`/`END` matching, comment toggling, case conversion, line moves.
- Context-aware completion: columns after `alias.` (joins, CTEs, inline
  views), package members with signatures, `%ROWTYPE` and cursor records,
  locals and parameters, schema objects and public synonyms.
- Signature help, hover, Ctrl+click / F12 go to definition (also into the
  database), references, live syntax diagnostics and an outline.
- Rename a local variable, parameter, alias or CTE in place (Shift+F6).
- Code folding, numbered bookmarks (Ctrl+Shift+1..9 / Ctrl+1..9) and
  snippets with placeholders (type a trigger, press Tab).

![Completion with live dictionary data](docs/screenshots/completion.png)

**SQL windows**
- Run the statement under the cursor (Ctrl+Enter), the selection or the
  whole script (F8); SQL\*Plus `exec`, `prompt`, `define`, `desc`, `&vars`.
- Bind and substitution variables are prompted and remembered.
- Results in a virtualized grid: sorting, rectangular selection, aggregates,
  record view, value viewer, copy and export (CSV, TSV, JSON, Excel,
  INSERT statements, Markdown, HTML). Columns can be resized, hidden,
  moved by dragging their header and frozen at the left.
- **Follow foreign keys**: right-click a row to open its parent row or its
  child rows in any table referencing it.
- **Chart any result** as bars or lines: the x axis defaults to the first text
  or date column, numeric columns become series; the chart follows the grid's
  filter and sort.
- **Edit results in place** — no `FOR UPDATE` locks: changes are checked
  against the fetched values when applied, so concurrent modifications are
  reported instead of overwritten. "Show SQL" previews the DML.
- **Execute to file** streams a query of any size into CSV, TSV, JSON,
  INSERT statements or Markdown, with live row count and cancel.
- Explain plan (F5), DBMS_OUTPUT, statement history (Ctrl+E).

![Editing a result](docs/screenshots/grid-editing.png)

**PL/SQL development**
- Object browser with lazy, virtualized folders, invalid markers, compile,
  drop, describe, query, test; favorites and recently opened objects;
  filters for invalid or recently changed objects; tables expand to
  columns, indexes, constraints, triggers and referencing tables; users
  and roles.
- Create objects from the browser's right-click menu (or File → New
  Object): a table builder (columns with a type picker, primary and
  foreign keys, checks, indexes, identity columns, range, interval, list
  or hash partitioning, temporary tables, a copy of another table), and
  forms for views, materialized views, sequences, synonyms, indexes,
  database links, scheduler jobs, users, roles, profiles and directories,
  with a preview of the generated DDL. Triggers, procedures, functions, packages and types start
  from a form that writes their first source for a program window.
- Form editors for existing sequences, views, materialized views, synonyms,
  indexes, database links, scheduler jobs, users, roles, profiles,
  directories and grants.
- Program windows with spec and body, compile with clickable errors
  (gutter markers and squiggles), unsaved-change tracking, save to file.
- **Test…** generates a ready-to-run block for any procedure or function
  (IN parameters as binds, OUT parameters printed).
- **Debugger** (DBMS_DEBUG_JDWP): breakpoints with conditions and hit
  counts, break on exception, step over/into/out (also through the debugged
  block itself), call stack with subprograms, variables of any frame
  including records, `%ROWTYPE` and collections, watches, values on hover.
  Units with breakpoints are compiled with debug information; Step into
  enters only units that have it (program windows can compile with it,
  Settings → Debugger, and the panel says when a step went over a unit
  without).
- **Profiler** (DBMS_HPROF): profile a block or call and see self and
  total time per subprogram and SQL statement; double-click jumps to the
  code. Needs `EXECUTE` on `DBMS_HPROF`; profiling commits.
- Find in database source (Ctrl+Alt+F) with regular expressions.
- **Formatter** (Ctrl+Shift+F) with a live preview of its rules, profiles
  to share a team style, rules learned from existing code (an editor or a
  whole schema), formatting on save, on paste or when a statement's `;` is
  typed, and the AI for statements the rules can't lay out (only
  whitespace and case may change).

![Debugger stopped at a breakpoint](docs/screenshots/debugger.png)

**Monitoring**
- Dashboard with plain-language findings ("session 175 blocks 2 sessions —
  likely an uncommitted transaction"), KPIs and live charts.
- Sessions with a hierarchical blocking tree, session details, kill/cancel.
- AWR / ASH reports (only when the Diagnostics Pack is enabled) and a
  built-in tkprof-style trace analyzer.

![Monitoring dashboard](docs/screenshots/monitor-dashboard.png)

**AI assistant**
- Bring your own model: the Anthropic API, any OpenAI-compatible API, a
  local model server (Ollama, llama.cpp, LM Studio) or a command line tool
  (prompt on stdin, answer on stdout). Several can be set up, one is the
  default; API keys live in the OS keychain.
- Generate SQL from a description, using the real tables, columns and
  foreign keys of the connected schema; explain, fix (with the compile or
  run errors) or rewrite the statement under the cursor or the selection,
  then replace it or insert the answer's code with one click.
- Plain-language plan analysis (EXPLAIN PLAN, or the actual plan of a run
  with statistics) with likely problems and concrete fixes; "Analyze with
  AI" on top SQL, session details, AWR/ASH reports and traces.
- A side panel for open-ended questions about the focused window. Table
  definitions, statistics, plans and source go with requests; result rows
  and bind values only if you allow it in Settings → AI, and each answer
  shows what was sent.

**Data and schema tools**
- Schema compare with structural table diffs and safe, ordered sync
  scripts; data compare by key with sync DML.
- Export tables: pick tables, a WHERE per table, CSV/TSV/JSON/INSERT/
  Markdown, one file per table plus DDL, streamed with progress.
- Import CSV/Excel: drop a file, check inferred types (decimal commas,
  date formats), import into a new or existing table with array DML.
  Per column: trim, date format, SQL expression (`upper(:value)`,
  constants); bad rows can be skipped and logged to `<file>.bad.csv`.

**Reports**
- Built-in reports (invalid objects, unindexed foreign keys, largest
  segments, top SQL, locks, jobs, …) open as editable queries.
- Save any query as your own report; bind and substitution variables are
  asked for when it runs.

**Scheduler jobs**
- All DBMS_SCHEDULER jobs with state, schedule, last and next run; run now,
  stop, enable, disable, drop; recent runs with errors.
- A form for new jobs and a job's settings: a PL/SQL block or a stored
  procedure, start and end, the repeat interval from presets or as a
  calendar expression, with the next runs it gives.

![Scheduler jobs](docs/screenshots/scheduler-jobs.png)

**Projects and version control**
- Open a folder of SQL and PL/SQL files as a project: git status in the
  tree, a list of changed files, diff with the last commit.
- Compare a file with its object in the database, side by side.
- Export a schema as one file per object (`tables/`, `packages/*.pks|pkb`,
  …) to put it under version control.
- Deploy changed files: a reviewed script of `@file` lines in dependency
  order.

![Comparing a file with the database](docs/screenshots/project-compare-db.png)

**Safety**
- Every window has its own session and transaction (multi-session); the
  status bar always shows pending transactions; production connections are
  marked red and ask before commit; read-only connections; passwords live
  in the OS keychain.

## Installing

Packages for Linux (`.deb`, AppImage), Windows (MSI, setup `.exe`) and
macOS (`.dmg`) are attached to each [GitHub
release](https://github.com/ortic/pythia/releases).

Pythia connects to Oracle directly, with a pure-Rust driver: **no Oracle
client libraries are needed**. Easy Connect strings (`host:port/service`)
work as they are; for net service names, point `TNS_ADMIN` at the directory
with your `tnsnames.ora` (or put it into `$ORACLE_HOME/network/admin`), and
Pythia offers the aliases when connecting.

**Database versions:** Pythia is developed against Oracle 23ai and tested
against 18c XE and 21c XE. A few features need a newer release: the
profiler, *Cancel current SQL* in the monitor and restarting a sequence need
18c; reading trace files through the database needs 12.2.

Proxy authentication works with the usual `proxy_user[target_user]` user
name. TLS (TCPS) works, cancelling a running statement included; the
driver's tests run over TLS too, while Pythia itself hasn't been tried on a
TLS server yet. For a server certificate from a private CA, put the CA's
certificate as `ewallet.pem` in a folder and name it in the connect string:
`tcps://host:2484/service?wallet_location=/path/to/folder`, or
`(SECURITY=(MY_WALLET_DIRECTORY=/path/to/folder))` in tnsnames.ora.
`TIMESTAMP WITH TIME ZONE` values with a region show the region's name
(`2026-07-01 12:00:00 Europe/Zurich`); its offset comes from the IANA time
zone rules built into Pythia, which can differ from the server's time zone
file for very old dates and for rule changes newer than that file.

**When you need the Oracle Instant Client:** Oracle 11.2 databases (12.1 is
untested), servers that *require* Native Network Encryption
(`SQLNET.ENCRYPTION_SERVER = REQUIRED`; use TLS instead where you can), and
OS or wallet logins (`/@alias`). Build Pythia with `cargo build --release --features
pythia-db/thick` and install the [Instant
Client](https://www.oracle.com/database/technologies/instant-client/downloads.html)
(*Basic*; on Linux also `libaio`), on the library path (`LD_LIBRARY_PATH`,
`PATH` on Windows).

## Building from source

Requirements: Rust (stable).

```sh
cargo build --release
./target/release/pythia
```

The database driver is our fork of
[rust-oracledb](https://github.com/ortic/rust-oracledb) (branch `pythia`:
upstream fixes that aren't released yet, and cancelling a running call),
pinned to a commit in `Cargo.toml`.

Packages are built with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager)
(`cargo install cargo-packager --locked`):

```sh
cargo packager --release -p pythia-app            # all formats of this platform
cargo packager --release -p pythia-app -f deb     # just one
```

The output lands in `target/packages`. Releases are built by
`.github/workflows/release.yml` when a `v*` tag is pushed.

## Development

A local Oracle Free database for development and tests:

```sh
docker compose up -d        # or: podman compose up -d
```

Tests that need the database are skipped unless configured:

```sh
PYTHIA_TEST_DB=pythia/pythia@localhost:1521/FREEPDB1 cargo test
```

`--features pythia-db/thick` runs them against the Instant Client instead
(set `LD_LIBRARY_PATH`); `cargo run -p pythia-db --example thin_probe
--features thick` compares the two drivers.

| Variable | Used by |
|---|---|
| `PYTHIA_TEST_DB` | integration tests (app user) |
| `PYTHIA_TEST_DB_DBA` | monitor, compare and debugger tests (`system/oracle@…`) |
| `PYTHIA_DEBUG_HOST` | debugger tests: how the database reaches this machine (`host.containers.internal` for a local Podman container; with Docker on Linux, the compose network's gateway from `docker network inspect pythia_default`) |
| `PYTHIA_SCREENSHOT_DIR` | renders the real UI offscreen into PNGs (`cargo test -p pythia-app screenshots`) |

The debugger needs `DEBUG CONNECT SESSION`, `DEBUG ANY PROCEDURE` and a JDWP
network ACL; `pythia_debugger::target::setup_script` prints what a DBA has to
run. `crates/pythia-debugger/tests/scenario.rs` steps through a larger
scenario (packages, records, collections, an object type, a trigger).

The tests set up what they need on a new database: the demo schema
several app tests use, and, with `PYTHIA_TEST_DB_DBA` set,
`SELECT_CATALOG_ROLE` and the debugger privileges for the test user
(without a DBA, grant those yourself). CI runs the whole suite against a
fresh Oracle Free container.

### Workspace

| Crate | Purpose |
|---|---|
| `pythia-app` | the application: documents, docking, commands, dialogs |
| `pythia-ui` | code editor, data grid, icon set |
| `pythia-db` | sessions on worker threads, dictionary queries |
| `pythia-plsql` | lexer, statement splitter, error-tolerant parser |
| `pythia-lsp-core` | completion, hover, navigation, outline, diagnostics |
| `pythia-data` | export and import |
| `pythia-debugger` | JDWP debugger and panel |
| `pythia-monitor` | dashboard, sessions, AWR/ASH, trace analyzer |
| `pythia-compare` | schema and data compare |
| `pythia-ai` | AI backends (HTTP, command line), prompts and the data policy |
| `pythia-config` | settings and saved connections |

See [CONCEPT.md](CONCEPT.md) for the full plan and status.

## Keyboard

| Key | Action |
|---|---|
| Ctrl+Enter / F8 | Execute statement / selection or script |
| F5 | Explain plan |
| Shift+Esc | Cancel running statement |
| F10 / Shift+F10 | Commit / rollback |
| Ctrl+Space | Completion |
| F12, Ctrl+click | Go to definition |
| Ctrl+E | Statement history |
| Ctrl+Shift+O | Find database object |
| Ctrl+Alt+F | Find in database source |
| Ctrl+Shift+P | Command palette |
| Ctrl+F9 | Debug block under cursor |
| Ctrl+B | Toggle breakpoint |
| F9 / F6 / F7 / Shift+F7 / Shift+F9 | Continue / step over / into / out / stop |
| Ctrl+O / Ctrl+S | Open / save file |
| Shift+F6 | Rename symbol |
| Ctrl+Shift+[ / ] | Fold / unfold |
| Ctrl+Shift+1..9, Ctrl+1..9 | Set / jump to bookmark |
| Ctrl+, | Settings |

All application shortcuts can be changed in Settings → Keyboard.
