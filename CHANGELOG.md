# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-10-23

### Fixed

- Decimal `NUMBER` values with more than 15 significant digits (for example
  `NUMBER(38,10)`) were rounded through `f64`; they are now returned as exact
  strings, like integers wider than `i64` already were.

### Changed

- The manifest declares the stable `id` `oracle` and the display name
  "Oracle".
- The plugin icon and README banner use the Oracle Database logo; the
  manifest icon is now `oracle-icon.png`.
- Bumped `base64` to 0.23 and `serde_json` to 1.0.154.
- EXPLAIN parser tooling: `tsup` 9, `vitest` 5 and `@types/node` 26.6.5. The
  test typecheck now lists `node` types explicitly, since `vitest` 5 no
  longer pulls them in. TypeScript stays on 5.9 because TypeScript 7 ships no
  compiler API for `tsup`'s declaration build; Dependabot ignores its major
  updates until that changes.

### Security

- The RustSec audit ignores RUSTSEC-2024-0436 (`paste` unmaintained, no CVE).
  `paste` is a compile-time proc-macro reached only through the `oracle`
  crate, so none of its code ships in the binary; upstream tracks the
  replacement in kubo/rust-oracle#97.

## [0.1.0] - 2026-09-23

First public release.

### Added

- Oracle Database driver (12c+, Free/XE, Autonomous Database, Amazon RDS)
  built on ODPI-C, loading the Oracle Instant Client at runtime; the client
  location can be set from the plugin settings.
- EZConnect, `tcps://`, full connect descriptors and TNS aliases, with a
  single cached session guarded by an OCI ping.
- Multi-schema browsing through the `ALL_*` catalog views: tables, columns
  (primary keys, identity columns), indexes, foreign keys, views and stored
  procedures/functions, including table and column comments.
- Query execution with `OFFSET/FETCH` pagination, PL/SQL blocks, exact
  `NUMBER` handling, `RAW`/`BLOB` as base64 and native `JSON` columns through
  a transparent `JSON_SERIALIZE` retry.
- Row insert/update/delete with bound values and composite primary keys.
- Table, column, index, foreign-key and view DDL.
- Visual EXPLAIN: `explain_query` returns raw `oracle-plan-json` captured from
  `PLAN_TABLE`, or from `V$SQL_PLAN_STATISTICS_ALL` for EXPLAIN ANALYZE
  (queries only), parsed by the plugin-owned TypeScript parser shipped as
  `explain/dist/index.iife.js` and published as `@tabularis/explain-oracle`.
- Registry-grade `.tabularium` manifest (icon, screenshots, `sql_dialect`
  `oracle`, `explain_parsers`) with a `0.25.0` runtime floor, the first
  Tabularis release exposing table and column comments.
- CI for formatting, Clippy, unit tests, a live Oracle Free integration test,
  the EXPLAIN parser package, manifest validation, Markdown lint, Conventional
  Commit PR titles, version suggestions, dependency updates and RustSec
  advisories, plus a five-platform release workflow that checks tag/version
  agreement and publishes the npm parser from the same tag.

[Unreleased]: https://github.com/TabularisDB/tabularis-oracle-plugin/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/TabularisDB/tabularis-oracle-plugin/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/TabularisDB/tabularis-oracle-plugin/releases/tag/v0.1.0
