# `@tabularis/explain-oracle`

Oracle Database execution-plan parser for Tabularis Visual EXPLAIN. This
package is maintained and versioned with the
[Tabularis Oracle plugin](https://github.com/TabularisDB/tabularis-oracle-plugin).

## Install

```bash
pnpm add @tabularis/explain @tabularis/explain-oracle
```

`@tabularis/explain` 0.2.0 or newer is required because that release introduced
the parser registry.

## Register on import

Import the package for its registration side effect before parsing Oracle raw
output through `@tabularis/explain`:

```ts
import "@tabularis/explain-oracle";
import { parseRawExplain } from "@tabularis/explain";

const plan = parseRawExplain({
  engine: "oracle",
  format: "oracle-plan-json",
  payload,
  original_query: "SELECT * FROM shop.orders",
});
```

The package declares `dist/index.js` as side-effectful so bundlers retain the
registration import.

## Parse directly

```ts
import { parseOraclePlan } from "@tabularis/explain-oracle";

const plan = parseOraclePlan(payload);
```

Direct parsing leaves `original_query` empty. The registry's raw-output path
adds the original query supplied by the caller.

## Payload format: `oracle-plan-json`

The plugin captures plan rows server-side and ships them as a JSON document:

```jsonc
{
  "version": 1,
  "statistics": false,        // true for EXPLAIN ANALYZE
  "plan": [                   // one object per plan operation
    { "id": 0, "parent_id": null, "position": 1, "operation": "SELECT STATEMENT", "cost": 9, ... }
  ],
  "text": "Plan hash value: ..." // DBMS_XPLAN rendering for the raw view
}
```

Plan rows use the lowercase column names of `PLAN_TABLE` (estimated plans) or
`V$SQL_PLAN_STATISTICS_ALL` (analyzed plans). Analyzed rows add the last
execution's `starts`, `actual_rows`, `elapsed_us`, `cr_buffer_gets`,
`cu_buffer_gets` and `disk_reads`.

## Mapping

| Oracle | Shared plan model |
| --- | --- |
| `OPERATION` + `OPTIONS` | `node_type` (for example `TABLE ACCESS FULL`) |
| `OBJECT_NAME` | `relation` |
| `COST` / `CARDINALITY` | `total_cost` / `plan_rows` |
| `ACCESS_PREDICATES` | `index_condition` on `INDEX` operations, `hash_condition` on hash and merge joins, otherwise `extra.access_predicates` |
| `FILTER_PREDICATES` | `filter` |
| `NESTED LOOPS` / `* JOIN` + `OPTIONS` | `join_type` (`INNER` when no option is set) |
| `LAST_STARTS` | `actual_loops` |
| `LAST_OUTPUT_ROWS`, `LAST_ELAPSED_TIME` | `actual_rows`, `actual_time_ms`, divided by starts |
| `LAST_CR_BUFFER_GETS` + `LAST_CU_BUFFER_GETS` / `LAST_DISK_READS` | `buffers_hit` / `buffers_read` |

Oracle reports A-Rows and A-Time as totals across all starts of an operation,
while the shared model is per loop (like PostgreSQL). The parser divides by
`starts`, and keeps the totals in `extra.actual_rows_total` and
`extra.actual_time_ms_total`. The statement's execution time is the root's
elapsed time, falling back to its first child. Oracle has no separate planning
time, so `planning_time_ms` is `null`.

## Plugin bundle

The build also produces `dist/index.iife.js`. Tabularis loads that file from
the installed Oracle plugin, evaluates it with the `__TABULARIS_EXPLAIN__`
host API, and reads the parser descriptor from `__tabularis_explain_parser__`.
The IIFE does not register itself; the desktop loader validates it against the
plugin manifest and performs registration.

## Fixtures

The fixtures under `tests/fixtures/` are `explain_query` payloads captured from
Oracle Database 23ai Free: estimated and analyzed hash joins, an index unique
scan, analyzed nested loops over an index range scan, a native JSON column
query, and a sorted view. Update a golden plan under `tests/fixtures/expected/`
only with a separately reviewed parser-semantic change.
