//! `explain_query`: capture Oracle's execution plan and hand it to the host
//! as raw output for the plugin-owned TypeScript parser (`explain/`).
//!
//! The host recognizes `{ engine, format, payload, original_query }` as raw
//! EXPLAIN output and dispatches `payload` to the parser registered for
//! `format`, loaded from `explain/dist/index.iife.js`. This module only
//! captures plan rows; all tree building and metric mapping lives in
//! TypeScript so the desktop and the standalone visualizer share one parser.
//!
//! - Estimated plans use `EXPLAIN PLAN SET STATEMENT_ID ... FOR` and read the
//!   rows back from `PLAN_TABLE` (a session-private temporary table on 12c+).
//! - `analyze` executes the statement with `STATISTICS_LEVEL = ALL`, then reads
//!   the cursor's per-operation runtime statistics from
//!   `V$SQL_PLAN_STATISTICS_ALL`. It is limited to queries, so a plan request
//!   never modifies data.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Map, Value};

use crate::client::{Client, QueryResult};
use crate::error::PluginError;
use crate::handlers::{connect, req_str, respond};
use crate::utils::sql::{prepare_statement, returns_rows};

/// Wire-format tag shared with `explain/src/parser.ts`.
pub const FORMAT: &str = "oracle-plan-json";
/// Bumped only on an incompatible payload change.
const PAYLOAD_VERSION: u64 = 1;

/// Plan columns common to `PLAN_TABLE` and `V$SQL_PLAN_STATISTICS_ALL`.
const PLAN_COLUMNS: &str = "id, parent_id, depth, position, operation, options, \
     object_owner, object_name, object_alias, object_type, optimizer, cost, cardinality, \
     bytes, cpu_cost, io_cost, time, partition_start, partition_stop, access_predicates, \
     filter_predicates, projection, qblock_name";

static STATEMENT_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn explain_query(id: Value, params: &Value) -> Value {
    respond(id, explain_query_impl(params))
}

fn explain_query_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let query = req_str(params, "query")?;
    let analyze = params
        .get("analyze")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let statement = prepare_statement(&query);

    let payload = if analyze {
        analyzed_plan(&client, statement)?
    } else {
        estimated_plan(&client, statement)?
    };

    Ok(json!({
        "engine": "oracle",
        "format": FORMAT,
        "payload": payload.to_string(),
        "original_query": query,
    }))
}

fn estimated_plan(client: &Client, statement: &str) -> Result<Value, PluginError> {
    let statement_id = format!(
        "TABULARIS_{}_{}",
        std::process::id(),
        STATEMENT_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    // The statement id is plugin-generated (digits and underscores only), so
    // quoting it into the EXPLAIN PLAN clause cannot inject SQL.
    client.execute(
        &format!("EXPLAIN PLAN SET STATEMENT_ID = '{statement_id}' FOR {statement}"),
        &[],
    )?;

    let captured = (|| {
        let rows = client.query(
            &format!(
                "SELECT {PLAN_COLUMNS} FROM plan_table \
                 WHERE statement_id = :1 \
                   AND plan_id = (SELECT MAX(plan_id) FROM plan_table WHERE statement_id = :2) \
                 ORDER BY id"
            ),
            &[json!(statement_id), json!(statement_id)],
        )?;
        let text = client.query(
            "SELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY('PLAN_TABLE', :1, 'TYPICAL'))",
            &[json!(statement_id)],
        )?;
        Ok::<_, PluginError>((rows, text))
    })();

    // PLAN_TABLE rows are session-private but would otherwise accumulate for
    // the lifetime of the cached session.
    let _ = client.execute(
        "DELETE FROM plan_table WHERE statement_id = :1",
        &[json!(statement_id)],
    );

    let (rows, text) = captured?;
    Ok(payload(&rows, &text, false))
}

fn analyzed_plan(client: &Client, statement: &str) -> Result<Value, PluginError> {
    if !returns_rows(statement) {
        return Err(PluginError::invalid_params(
            "EXPLAIN ANALYZE executes the statement, so the Oracle driver only allows it for SELECT/WITH queries",
        ));
    }

    client.execute("ALTER SESSION SET STATISTICS_LEVEL = ALL", &[])?;
    let executed = client.drain(statement);
    // Read the cursor id before running anything else: PREV_SQL_ID is the
    // statement executed just before this lookup.
    let cursor = executed.and_then(|_| {
        client
            .query(
                "SELECT prev_sql_id, prev_child_number FROM v$session \
                 WHERE sid = SYS_CONTEXT('USERENV', 'SID')",
                &[],
            )
            .map_err(missing_privilege_hint)
    });
    let _ = client.execute("ALTER SESSION SET STATISTICS_LEVEL = TYPICAL", &[]);

    let cursor = cursor?;
    let (sql_id, child) = match cursor.rows.first().map(Vec::as_slice) {
        Some([Value::String(sql_id), child, ..]) => (sql_id.clone(), child.clone()),
        _ => {
            return Err(PluginError::internal(
                "could not identify the executed cursor in V$SESSION",
            ))
        }
    };

    let rows = client
        .query(
            &format!(
                "SELECT {PLAN_COLUMNS}, last_starts AS starts, last_output_rows AS actual_rows, \
                        last_elapsed_time AS elapsed_us, last_cr_buffer_gets AS cr_buffer_gets, \
                        last_cu_buffer_gets AS cu_buffer_gets, last_disk_reads AS disk_reads \
                 FROM v$sql_plan_statistics_all \
                 WHERE sql_id = :1 AND child_number = :2 \
                 ORDER BY id"
            ),
            &[json!(sql_id), child.clone()],
        )
        .map_err(missing_privilege_hint)?;
    let text = client.query(
        "SELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY_CURSOR(:1, :2, 'ALLSTATS LAST +COST +BYTES'))",
        &[json!(sql_id), child],
    )?;
    Ok(payload(&rows, &text, true))
}

/// Runtime plan statistics live in dynamic performance views that ordinary
/// application accounts often cannot read.
fn missing_privilege_hint(err: PluginError) -> PluginError {
    if err.message.contains("ORA-00942") {
        PluginError {
            code: err.code,
            message: format!(
                "{}. EXPLAIN ANALYZE needs SELECT on V$SESSION and V$SQL_PLAN_STATISTICS_ALL \
                 (for example through SELECT_CATALOG_ROLE)",
                err.message
            ),
        }
    } else {
        err
    }
}

/// Build the `oracle-plan-json` payload: plan rows keyed by lowercase column
/// name, plus the DBMS_XPLAN text rendering for the raw view.
fn payload(rows: &QueryResult, text: &QueryResult, statistics: bool) -> Value {
    let columns: Vec<String> = rows.columns.iter().map(|c| c.to_lowercase()).collect();
    let plan: Vec<Value> = rows
        .rows
        .iter()
        .map(|row| {
            let object: Map<String, Value> = columns.iter().cloned().zip(row.clone()).collect();
            Value::Object(object)
        })
        .collect();
    let text = text
        .rows
        .iter()
        .filter_map(|row| row.first().and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");

    json!({
        "version": PAYLOAD_VERSION,
        "statistics": statistics,
        "plan": plan,
        "text": text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_keys_rows_by_lowercase_column() {
        let rows = QueryResult {
            columns: vec!["ID".into(), "OPERATION".into()],
            rows: vec![vec![json!(0), json!("SELECT STATEMENT")]],
            affected: 0,
        };
        let text = QueryResult {
            columns: vec!["PLAN_TABLE_OUTPUT".into()],
            rows: vec![vec![json!("line 1")], vec![json!("line 2")]],
            affected: 0,
        };
        assert_eq!(
            payload(&rows, &text, false),
            json!({
                "version": 1,
                "statistics": false,
                "plan": [{ "id": 0, "operation": "SELECT STATEMENT" }],
                "text": "line 1\nline 2",
            })
        );
    }

    #[test]
    fn privilege_hint_only_for_missing_views() {
        let hinted = missing_privilege_hint(PluginError::internal(
            "ORA-00942: table or view does not exist",
        ));
        assert!(hinted.message.contains("SELECT_CATALOG_ROLE"));
        let other = missing_privilege_hint(PluginError::internal("ORA-01017"));
        assert_eq!(other.message, "ORA-01017");
    }
}
