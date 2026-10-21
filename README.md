# orui

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
  INSERT statements, Markdown, HTML).
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
  drop, describe, query, test.
- Program windows with spec and body, compile with clickable errors
  (gutter markers and squiggles), unsaved-change tracking, save to file.
- **Test…** generates a ready-to-run block for any procedure or function
  (IN parameters as binds, OUT parameters printed).
- **Debugger** (DBMS_DEBUG_JDWP): breakpoints with conditions and hit
  counts, break on exception, step over/into/out, call stack, variables
  including records and collections, watches, values on hover.
- Find in database source (Ctrl+Alt+F) with regular expressions.

![Debugger stopped at a breakpoint](docs/screenshots/debugger.png)

**Monitoring**
- Dashboard with plain-language findings ("session 175 blocks 2 sessions —
  likely an uncommitted transaction"), KPIs and live charts.
- Sessions with a hierarchical blocking tree, session details, kill/cancel.
- AWR / ASH reports (only when the Diagnostics Pack is enabled) and a
  built-in tkprof-style trace analyzer.

![Monitoring dashboard](docs/screenshots/monitor-dashboard.png)

**Data and schema tools**
- Schema compare with structural table diffs and safe, ordered sync
  scripts; data compare by key with sync DML.
- Import CSV/Excel: drop a file, check inferred types (decimal commas,
  date formats), import into a new or existing table with array DML.

**Scheduler jobs**
- All DBMS_SCHEDULER jobs with state, schedule, last and next run; run now,
  stop, enable, disable, drop; recent runs with errors.

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
release](https://github.com/ortic/orui/releases).

orui talks to Oracle through the **Oracle Instant Client**, which Oracle's
license doesn't allow us to bundle. Install it once:

1. Download *Basic* for your platform from
   <https://www.oracle.com/database/technologies/instant-client/downloads.html>.
   *Basic Light* works too, but it only has the small time zone file:
   `TIMESTAMP WITH TIME ZONE` values with region names (e.g. scheduler job
   times) then fail with ORA-01805.
2. Unpack it, e.g. to `/opt/oracle/instantclient_23` or `C:\oracle\instantclient_23`.
3. Make it findable:
   - **Linux:** `sudo apt install libaio1t64` (or `libaio1` on older
     distributions), then either set `LD_LIBRARY_PATH=/opt/oracle/instantclient_23`
     or register it once with
     `echo /opt/oracle/instantclient_23 | sudo tee /etc/ld.so.conf.d/oracle.conf && sudo ldconfig`.
   - **Windows:** add the directory to `PATH` (System Properties → Environment
     Variables) and install the Visual C++ redistributable it lists.
   - **macOS:** set `DYLD_LIBRARY_PATH` or copy the libraries next to the app
     as described in Oracle's instructions.
4. Optional: put your `tnsnames.ora` into `network/admin` inside the Instant
   Client directory or point `TNS_ADMIN` at it; orui then offers the aliases
   when connecting.

If the client can't be loaded, orui says so when you connect and shows these
steps.

## Building from source

Requirements: Rust (stable) and the Instant Client as above.

```sh
cargo build --release
LD_LIBRARY_PATH=/path/to/instantclient ./target/release/orui
```

Packages are built with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager)
(`cargo install cargo-packager --locked`):

```sh
cargo packager --release -p orui-app            # all formats of this platform
cargo packager --release -p orui-app -f deb     # just one
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
export LD_LIBRARY_PATH=/path/to/instantclient
ORUI_TEST_DB=orui/orui@localhost:1521/FREEPDB1 cargo test
```

| Variable | Used by |
|---|---|
| `ORUI_TEST_DB` | integration tests (app user) |
| `ORUI_TEST_DB_DBA` | monitor, compare and debugger tests (`system/oracle@…`) |
| `ORUI_DEBUG_HOST` | debugger tests: how the database reaches this machine (`host.containers.internal` for a local container) |
| `ORUI_SCREENSHOT_DIR` | renders the real UI offscreen into PNGs (`cargo test -p orui-app screenshots`) |

The debugger needs `DEBUG CONNECT SESSION`, `DEBUG ANY PROCEDURE` and a JDWP
network ACL; `orui_debugger::target::setup_script` prints what a DBA has to
run.

### Workspace

| Crate | Purpose |
|---|---|
| `orui-app` | the application: documents, docking, commands, dialogs |
| `orui-ui` | code editor, data grid, icon set |
| `orui-db` | sessions on worker threads, dictionary queries |
| `orui-plsql` | lexer, statement splitter, error-tolerant parser |
| `orui-lsp-core` | completion, hover, navigation, outline, diagnostics |
| `orui-data` | export and import |
| `orui-debugger` | JDWP debugger and panel |
| `orui-monitor` | dashboard, sessions, AWR/ASH, trace analyzer |
| `orui-compare` | schema and data compare |
| `orui-config` | settings and saved connections |

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
