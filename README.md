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

![Completion with live dictionary data](docs/screenshots/completion.png)

**SQL windows**
- Run the statement under the cursor (Ctrl+Enter), the selection or the
  whole script (F8); SQL\*Plus `exec`, `prompt`, `define`, `desc`, `&vars`.
- Bind and substitution variables are prompted and remembered.
- Results in a virtualized grid: sorting, rectangular selection, aggregates,
  record view, value viewer, copy and export (CSV, TSV, JSON, Excel,
  INSERT statements, Markdown, HTML).
- **Edit results in place** — no `FOR UPDATE` locks: changes are checked
  against the fetched values when applied, so concurrent modifications are
  reported instead of overwritten. "Show SQL" previews the DML.
- Explain plan (F5), DBMS_OUTPUT, statement history (Ctrl+E).

![Editing a result](docs/screenshots/grid-editing.png)

**PL/SQL development**
- Object browser with lazy, virtualized folders, invalid markers, compile,
  drop, describe, query, test.
- Program windows with spec and body, compile with clickable errors
  (gutter markers and squiggles), unsaved-change tracking, save to file.
- **Test…** generates a ready-to-run block for any procedure or function
  (IN parameters as binds, OUT parameters printed).
- **Debugger** (DBMS_DEBUG_JDWP): breakpoints, step over/into/out, call
  stack, variables including records and collections.
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

**Safety**
- Every window has its own session and transaction (multi-session); the
  status bar always shows pending transactions; production connections are
  marked red and ask before commit; read-only connections; passwords live
  in the OS keychain.

## Building and running

Requirements: Rust (stable), and the [Oracle Instant
Client](https://www.oracle.com/database/technologies/instant-client.html)
(Basic or Basic Light) at runtime.

```sh
cargo build --release
# Linux: point the loader at Instant Client
LD_LIBRARY_PATH=/path/to/instantclient ./target/release/orui
```

On Windows put the Instant Client directory on `PATH`, on macOS on
`DYLD_LIBRARY_PATH`. orui explains what's missing if it can't load the
client.

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
| Ctrl+, | Settings |
