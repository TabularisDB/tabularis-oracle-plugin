# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A [Tabularis](https://github.com/TabularisDB/tabularis) driver plugin, written in Rust, that lets Tabularis connect to Oracle Database. Tabularis launches the compiled binary as a subprocess and talks to it over stdio using JSON-RPC (one JSON object per line in, one JSON object per line out). The plugin is built on ODPI-C through the `oracle` crate: ODPI-C is compiled from bundled sources, and the Oracle Instant Client is loaded at runtime on first connect.

Full plugin contract (required RPC methods, manifest schema) lives in the upstream guide: `https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md`.

## Commands

```bash
just build           # EXPLAIN parser bundle + cargo build (debug)
just release         # EXPLAIN parser bundle + cargo build --release
just test            # cargo test (no database or Instant Client required)
just test-explain    # typecheck + test the TypeScript plan parser
just lint            # cargo clippy --all-targets -- -D warnings
just fmt             # cargo fmt --all
just repl            # cargo run --bin test_plugin — local JSON-RPC sandbox
just demo-db         # Oracle Free in Docker (system / oracle / FREEPDB1)
just live-test DIR   # tests/live_db.rs against demo-db, DIR = Instant Client
just dev-install     # build + copy binary/manifest/parser into the Tabularis plugins dir
just uninstall       # remove the installed plugin
```

## Architecture

```text
src/
  main.rs           # stdio JSON-RPC loop
  rpc.rs            # method routing + response helpers
  client.rs         # connect-string building, session cache, query/execute/drain
  settings.rs       # initialize-time settings (Instant Client location)
  handlers/         # metadata / query / explain / crud / ddl
  utils/            # identifiers, pagination, SQL classification, JSON columns
explain/            # TypeScript plan parser: plugin IIFE + @tabularis/explain-oracle
tests/live_db.rs    # end-to-end RPC test, skipped without ORACLE_TEST_HOST
```

Key invariants:

- JSON emitted by handlers must deserialize into the host's model structs; don't change field names or nullability casually.
- `.tabularium` `data_types` and `capabilities` describe what the handlers implement; keep them in sync.
- `explain_query` returns raw `{ engine: "oracle", format: "oracle-plan-json", payload, original_query }`. The payload shape is documented in `explain/README.md`; the Rust side only captures rows, and all tree building lives in `explain/src/plan.ts`. Keep the payload `version`, the manifest's `explain_parsers` entry and the parser in step.
- EXPLAIN ANALYZE executes the statement, so it is refused for anything but `SELECT`/`WITH`.
- `.tabularium`, `Cargo.toml` and `explain/package.json` share one version; the release workflow refuses a tag that disagrees with any of them.
