# Tabularis Oracle driver

> ⚠️ **Work in progress** — this plugin is under active development. APIs,
> behavior and feature coverage may change, and things may break. Use at your
> own risk.

A [Tabularis](https://github.com/TabularisDB/tabularis) database driver plugin
for **Oracle Database** (12c and newer, including Oracle Free/XE, Autonomous
Database and Amazon RDS for Oracle).

The plugin is a standalone executable that speaks Tabularis' JSON-RPC protocol
over stdin/stdout. It is built on [ODPI-C] via the [`oracle`] crate: the C
sources are bundled and compiled at build time, so **no Oracle SDK is needed to
build** — but the **Oracle Instant Client is required at runtime** (see below).

[ODPI-C]: https://github.com/oracle/odpi
[`oracle`]: https://crates.io/crates/oracle

## Runtime requirement: Oracle Instant Client

Oracle's wire protocol is proprietary, so every driver goes through Oracle's
own client library. Install the free [Oracle Instant Client "Basic" or "Basic
Light"](https://www.oracle.com/database/technologies/instant-client/downloads.html)
package and make it findable in one of two ways:

1. Put its directory on the system `PATH` (Windows) / `LD_LIBRARY_PATH`
   (Linux) / standard library path (macOS), **or**
2. Set **Oracle Client library directory** in the plugin's settings
   (Settings → Installed Plugins → Oracle → gear icon) to the Instant Client
   folder.

> [!IMPORTANT]
> On **Linux**, `LD_LIBRARY_PATH` must be set as a **system-wide environment
> variable**, not in your shell profile: Tabularis launched from the desktop
> (menu, launcher, `.desktop` file) never reads `.bashrc`/`.zshrc`, so a
> variable exported there is invisible to it. Set it in `/etc/environment`
> (`LD_LIBRARY_PATH=/opt/oracle/instantclient_23_5`) or in
> `~/.config/environment.d/oracle.conf` on systemd desktops, then log out and
> back in. Alternatively, register the directory with the loader once and skip
> the variable entirely:
>
> ```bash
> echo /opt/oracle/instantclient_23_5 | sudo tee /etc/ld.so.conf.d/oracle-instantclient.conf
> sudo ldconfig
> ```
>
> The same caveat likely applies to GUI-launched apps on the other platforms
> (`PATH` on Windows must be the *system* one, and macOS strips
> `DYLD_LIBRARY_PATH` from GUI apps) — on those, prefer the plugin's
> **Oracle Client library directory** setting.

If the library cannot be found, connections fail with a clear DPI-1047 hint
rather than a cryptic loader error.

## Connecting

| Field | Value |
|-------|-------|
| **Host** / **Port** | Listener host and port (default `1521`) |
| **Database** | The **service name**, e.g. `FREEPDB1`, `XEPDB1`, `ORCLPDB1` |
| **Username / Password** | Credentials (`system` works for local test containers) |
| **SSL mode** `require` | Uses `tcps://` (e.g. Autonomous Database on port 1522) |

Power-user forms are also accepted in the **Database** field, in which case
Host/Port are ignored:

- A full **EZConnect** string: `//db.example.com:1521/ORCLPDB1` or
  `tcps://adb.region.oraclecloud.com:1522/xxx_high`
- A full **connect descriptor**: `(DESCRIPTION=(ADDRESS=...)...)`
- A **TNS alias** resolved via `tnsnames.ora`

Schemas are first-class: the schema selector lists all users visible to the
connection (`ALL_USERS`), and metadata is read through the `ALL_*` catalog
views, so you see exactly what your account has rights on.

### Local test database

```bash
just demo-db   # docker run gvenzl/oracle-free:slim  (system / oracle / FREEPDB1)
```

## Feature coverage

| Area | Status |
|------|--------|
| `test_connection`, `ping` (native OCI ping) | ✅ |
| Schemas, tables (+comments), columns (+PK/identity), indexes, foreign keys | ✅ |
| Views: list, definition, columns, create/replace/drop | ✅ |
| Stored procedures & functions: list, parameters, source | ✅ |
| Query execution with `OFFSET/FETCH` pagination + total count (12c+) | ✅ |
| `EXPLAIN PLAN` (via `DBMS_XPLAN.DISPLAY`) | ✅ |
| PL/SQL blocks (`BEGIN...END;`, `CREATE PROCEDURE`, ...) | ✅ |
| Insert / update / delete rows (bound parameters) | ✅ |
| Schema snapshot + batch columns/FKs (ER diagram, one catalog query per schema) | ✅ |
| `CREATE TABLE` DDL (`DBMS_METADATA.GET_DDL`), add/modify column, create/drop index, add/drop foreign key | ✅ |
| `ON UPDATE` actions on foreign keys | ❌ (Oracle has no ON UPDATE) |

Implementation notes:

- Identifiers are quoted ANSI-style (`"NAME"`) exactly as the catalog returns
  them, so mixed-case objects work.
- `NUMBER` values are fetched as text and re-parsed: integers stay exact
  (beyond-i64 integers are returned as strings instead of losing precision),
  decimals become JSON numbers.
- `RAW` and `BLOB` values are returned base64-encoded; `CLOB`/`LONG` as text.
- The session is cached inside the plugin process and reused across calls
  (guarded by an OCI ping), because opening an Oracle session is expensive.
- Trailing semicolons are stripped from plain SQL (OCI rejects them) but kept
  for PL/SQL blocks, which need them.

## Build & test

Requires a Rust toolchain and a C compiler (for the bundled ODPI-C). No Oracle
software is needed to build or run the unit tests.

```bash
just test          # cargo test — unit tests for SQL builders, parsing, RPC
just build         # debug build
just release       # optimized release build
just lint          # clippy -D warnings
just dev-install   # build + copy binary + manifest into the Tabularis plugins dir
just repl          # local JSON-RPC REPL over stdio
```

### Manual JSON-RPC smoke test

```bash
echo '{"jsonrpc":"2.0","method":"get_tables","params":{"params":{"host":"localhost","port":1521,"database":"FREEPDB1","username":"system","password":"oracle"},"schema":null},"id":1}' \
  | ./target/debug/oracle-plugin
```

## Installing

`just dev-install` copies `oracle-plugin` and `manifest.json` into the
Tabularis plugins folder:

- **Linux:** `~/.local/share/tabularis/plugins/oracle/`
- **macOS:** `~/Library/Application Support/com.debba.tabularis/plugins/oracle/`
- **Windows:** `%APPDATA%\debba\tabularis\data\plugins\oracle\`

Restart Tabularis (or toggle the plugin in Settings) and **Oracle** appears in
the Database Type list.

## Architecture

```
src/
├── main.rs              # stdio JSON-RPC loop
├── rpc.rs               # method routing + response helpers
├── client.rs            # connect-string building, session cache, query/execute
├── settings.rs          # initialize-time settings (Instant Client location)
├── models.rs            # ConnectionParams
├── error.rs             # PluginError (incl. friendly DPI-1047 hint)
├── handlers/            # metadata / query / crud / ddl
└── utils/               # identifiers, pagination, SQL classification
```

## License

Apache-2.0
