//! Connection checks and arbitrary query execution.
//!
//! `execute_query` results match the host's `QueryResult` model:
//! `{ columns, rows, affected_rows, truncated, pagination }`. Pagination
//! follows the native drivers' pattern: fetch `limit + 1` rows, report
//! `has_more` and truncate, leave `total_rows` unset (no COUNT round-trip).

use serde_json::{json, Value};

use crate::error::PluginError;
use crate::handlers::{connect, req_str, respond};
use crate::utils::pagination::offset_for;
use crate::utils::sql::{is_wrappable, prepare_statement, returns_rows};

pub fn test_connection(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|client| {
            client.health_check()?;
            Ok(json!({ "success": true }))
        })
    })
}

pub fn ping(id: Value, params: &Value) -> Value {
    // Lightweight liveness probe: a failed OCI ping tells the host the
    // connection is dead so it can disconnect.
    respond(id, {
        connect(params).and_then(|client| {
            client.health_check()?;
            Ok(Value::Null)
        })
    })
}

pub fn execute_query(id: Value, params: &Value) -> Value {
    respond(id, execute_query_impl(params))
}

fn execute_query_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let query = req_str(params, "query")?;
    // The host sends `limit`; the published guide called it `page_size`.
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .or_else(|| params.get("page_size").and_then(Value::as_u64))
        .filter(|l| *l > 0);
    let page = params.get("page").and_then(Value::as_u64).unwrap_or(1);

    let prepared = prepare_statement(&query);

    if returns_rows(prepared) {
        match (limit, is_wrappable(prepared)) {
            (Some(size), true) => {
                let offset = offset_for(page, size);
                // OFFSET/FETCH needs Oracle 12c+. Fetch one row beyond the
                // page to learn whether more pages exist. Note: Oracle table
                // aliases take no AS keyword.
                let probe = size + 1;
                let paged = format!(
                    "SELECT * FROM ({prepared}) OFFSET {offset} ROWS FETCH NEXT {probe} ROWS ONLY"
                );
                let mut result = client.query(&paged, &[])?;
                let has_more = result.rows.len() as u64 > size;
                if has_more {
                    result.rows.truncate(size as usize);
                }
                Ok(json!({
                    "columns": result.columns,
                    "rows": result.rows,
                    "affected_rows": 0,
                    "truncated": has_more,
                    "pagination": {
                        "page": page,
                        "page_size": size,
                        "total_rows": Value::Null,
                        "has_more": has_more,
                    },
                }))
            }
            _ => {
                let result = client.query(prepared, &[])?;
                Ok(json!({
                    "columns": result.columns,
                    "rows": result.rows,
                    "affected_rows": 0,
                    "truncated": false,
                    "pagination": Value::Null,
                }))
            }
        }
    } else {
        // DML/DDL/PLSQL: no result set, report the affected-row count.
        let affected = client.execute(prepared, &[])?;
        Ok(json!({
            "columns": [],
            "rows": [],
            "affected_rows": affected,
            "truncated": false,
            "pagination": Value::Null,
        }))
    }
}
