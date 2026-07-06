//! Schema discovery and view management on top of Oracle's `ALL_*` catalog
//! views, so the plugin sees exactly what the connected user is allowed to see.
//!
//! Result shapes match the host's serde models in
//! `tabularisdb/src-tauri/src/models.rs` (`TableColumn`, `ForeignKey`,
//! `Index`, `TableSchema`, ...) — the published plugin guide documents an
//! older protocol with different field names.
//!
//! Batch methods (snapshot, columns/FKs batch) hit the catalog once per schema
//! instead of once per table — Oracle round-trips are too expensive for a
//! query-per-table loop on real schemas.

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Value};

use crate::client::Client;
use crate::error::PluginError;
use crate::handlers::{
    cell_i64, cell_str, connect, req_str, req_str_any, resolve_schema, respond,
};
use crate::utils::identifiers::qualify;

/// Values grouped per table name, as produced by the per-schema catalog queries.
type GroupedRows = BTreeMap<String, Vec<Value>>;

// ---------------------------------------------------------------------------
// Reusable extractors
// ---------------------------------------------------------------------------

fn list_table_names(client: &Client, owner: &str) -> Result<Vec<String>, PluginError> {
    let r = client.query(
        "SELECT table_name FROM all_tables \
         WHERE owner = :1 AND nested = 'NO' AND secondary = 'N' \
           AND table_name NOT LIKE 'BIN$%' \
         ORDER BY table_name",
        &[json!(owner)],
    )?;
    Ok(r.rows.iter().filter_map(|row| cell_str(row, 0)).collect())
}

/// Primary-key columns as (table, column) pairs — one catalog query for a
/// whole schema, optionally narrowed to one table.
fn pk_columns(
    client: &Client,
    owner: &str,
    table: Option<&str>,
) -> Result<HashSet<(String, String)>, PluginError> {
    let base = "SELECT k.table_name, cc.column_name \
                FROM all_constraints k \
                JOIN all_cons_columns cc \
                  ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name \
                WHERE k.owner = :1 AND k.constraint_type = 'P'";
    let r = match table {
        Some(t) => client.query(
            &format!("{base} AND k.table_name = :2"),
            &[json!(owner), json!(t)],
        )?,
        None => client.query(base, &[json!(owner)])?,
    };
    Ok(r.rows
        .iter()
        .filter_map(|row| Some((cell_str(row, 0)?, cell_str(row, 1)?)))
        .collect())
}

/// Columns for one table or a whole schema, grouped by table name. The JSON
/// shape matches the host's `TableColumn` struct.
///
/// `identity_column` only exists on Oracle 12c+; on older servers the query
/// is retried without it (every column then reports non-identity).
fn columns_grouped(
    client: &Client,
    owner: &str,
    table: Option<&str>,
) -> Result<GroupedRows, PluginError> {
    let query_for = |with_identity: bool| {
        let identity_expr = if with_identity {
            "c.identity_column"
        } else {
            "'NO'"
        };
        let table_filter = if table.is_some() {
            " AND c.table_name = :2"
        } else {
            ""
        };
        format!(
            "SELECT c.table_name, c.column_name, c.data_type, c.char_length, c.data_length, \
                    c.data_precision, c.data_scale, c.nullable, c.data_default, {identity_expr} \
             FROM all_tab_columns c \
             WHERE c.owner = :1{table_filter} \
             ORDER BY c.table_name, c.column_id"
        )
    };
    let args: Vec<Value> = match table {
        Some(t) => vec![json!(owner), json!(t)],
        None => vec![json!(owner)],
    };
    let result = client
        .query(&query_for(true), &args)
        .or_else(|_| client.query(&query_for(false), &args))?;

    let pks = pk_columns(client, owner, table)?;

    let mut grouped: GroupedRows = BTreeMap::new();
    for row in &result.rows {
        let table_name = cell_str(row, 0).unwrap_or_default();
        let name = cell_str(row, 1).unwrap_or_default();
        let base_type = cell_str(row, 2).unwrap_or_default();
        let char_length = cell_i64(row, 3);
        let data_type = format_data_type(
            &base_type,
            char_length,
            cell_i64(row, 4),
            row.get(5).cloned().unwrap_or(Value::Null),
            row.get(6).cloned().unwrap_or(Value::Null),
        );
        let nullable = cell_str(row, 7).as_deref() == Some("Y");
        let default_value = cell_str(row, 8)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(Value::String)
            .unwrap_or(Value::Null);
        let identity = cell_str(row, 9).as_deref() == Some("YES");
        let is_pk = pks.contains(&(table_name.clone(), name.clone()));

        let char_max = if is_char_type(&base_type) && char_length > 0 {
            json!(char_length)
        } else {
            Value::Null
        };

        grouped.entry(table_name).or_default().push(json!({
            "name": name,
            "data_type": data_type,
            "is_pk": is_pk,
            "is_nullable": nullable,
            "is_auto_increment": identity,
            "default_value": default_value,
            "character_maximum_length": char_max,
        }));
    }
    Ok(grouped)
}

fn is_char_type(base: &str) -> bool {
    matches!(base, "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR")
}

/// Render an Oracle data type with its length/precision, e.g. `VARCHAR2(255)`,
/// `NUMBER(10,2)`. Types like `DATE` or `TIMESTAMP(6)` come out of the catalog
/// already complete and pass through.
fn format_data_type(
    base: &str,
    char_length: i64,
    data_length: i64,
    precision: Value,
    scale: Value,
) -> String {
    let precision = precision
        .as_i64()
        .or_else(|| precision.as_str().and_then(|s| s.parse().ok()));
    let scale = scale
        .as_i64()
        .or_else(|| scale.as_str().and_then(|s| s.parse().ok()));

    match base {
        "NUMBER" => match (precision, scale) {
            (Some(p), Some(s)) if s != 0 => format!("NUMBER({p},{s})"),
            (Some(p), _) => format!("NUMBER({p})"),
            (None, _) => "NUMBER".to_string(),
        },
        "FLOAT" => match precision {
            Some(p) => format!("FLOAT({p})"),
            None => "FLOAT".to_string(),
        },
        "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" if char_length > 0 => {
            format!("{base}({char_length})")
        }
        "RAW" if data_length > 0 => format!("RAW({data_length})"),
        _ => base.to_string(),
    }
}

/// Foreign keys grouped by table name, shaped like the host's `ForeignKey`.
/// Oracle has no ON UPDATE action, so `on_update` is always null.
fn foreign_keys_grouped(
    client: &Client,
    owner: &str,
    table: Option<&str>,
) -> Result<GroupedRows, PluginError> {
    let base = "SELECT k.table_name, k.constraint_name, cc.column_name, \
                       rk.table_name, rcc.column_name, k.delete_rule \
                FROM all_constraints k \
                JOIN all_cons_columns cc \
                  ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name \
                JOIN all_constraints rk \
                  ON rk.owner = k.r_owner AND rk.constraint_name = k.r_constraint_name \
                JOIN all_cons_columns rcc \
                  ON rcc.owner = rk.owner AND rcc.constraint_name = rk.constraint_name \
                 AND rcc.position = cc.position \
                WHERE k.owner = :1 AND k.constraint_type = 'R'";
    let r = match table {
        Some(t) => client.query(
            &format!("{base} AND k.table_name = :2 ORDER BY k.constraint_name, cc.position"),
            &[json!(owner), json!(t)],
        )?,
        None => client.query(
            &format!("{base} ORDER BY k.table_name, k.constraint_name, cc.position"),
            &[json!(owner)],
        )?,
    };

    let mut grouped: GroupedRows = BTreeMap::new();
    for row in &r.rows {
        let table_name = cell_str(row, 0).unwrap_or_default();
        grouped.entry(table_name).or_default().push(json!({
            "name": cell_str(row, 1).unwrap_or_default(),
            "column_name": cell_str(row, 2).unwrap_or_default(),
            "ref_table": cell_str(row, 3).unwrap_or_default(),
            "ref_column": cell_str(row, 4).unwrap_or_default(),
            "on_delete": row.get(5).cloned().unwrap_or(Value::Null),
            "on_update": Value::Null,
        }));
    }
    Ok(grouped)
}

/// Indexes as the host expects them: one flat entry per (index, column) with
/// its 1-based position, matching the `Index` struct.
fn indexes_for(client: &Client, owner: &str, table: &str) -> Result<Vec<Value>, PluginError> {
    let r = client.query(
        "SELECT i.index_name, i.uniqueness, ic.column_name, ic.column_position, \
                CASE WHEN pc.constraint_name IS NOT NULL THEN 1 ELSE 0 END \
         FROM all_indexes i \
         JOIN all_ind_columns ic \
           ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
         LEFT JOIN all_constraints pc \
           ON pc.owner = i.table_owner AND pc.table_name = i.table_name \
          AND pc.constraint_type = 'P' AND pc.index_name = i.index_name \
         WHERE i.table_owner = :1 AND i.table_name = :2 \
         ORDER BY i.index_name, ic.column_position",
        &[json!(owner), json!(table)],
    )?;

    Ok(r.rows
        .iter()
        .filter_map(|row| {
            let name = cell_str(row, 0)?;
            Some(json!({
                "name": name,
                "column_name": cell_str(row, 2).unwrap_or_default(),
                "is_unique": cell_str(row, 1).as_deref() == Some("UNIQUE"),
                "is_primary": cell_i64(row, 4) == 1,
                "seq_in_index": cell_i64(row, 3),
            }))
        })
        .collect())
}

// ---------------------------------------------------------------------------
// RPC handlers
// ---------------------------------------------------------------------------

pub fn get_databases(id: Value, params: &Value) -> Value {
    // One Oracle connection sees one database (CDB/PDB); report its name.
    respond(id, {
        connect(params).and_then(|c| {
            let name = c
                .query_single("SELECT SYS_CONTEXT('USERENV', 'DB_NAME') FROM DUAL", &[])?
                .unwrap_or_else(|| "ORACLE".to_string());
            Ok(json!([name]))
        })
    })
}

pub fn get_schemas(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let r = c.query("SELECT username FROM all_users ORDER BY username", &[])?;
            let schemas: Vec<Value> = r
                .rows
                .iter()
                .filter_map(|row| cell_str(row, 0))
                .map(Value::String)
                .collect();
            Ok(json!(schemas))
        })
    })
}

pub fn get_tables(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let tables: Vec<Value> = list_table_names(&c, &owner)?
                .into_iter()
                .map(|name| json!({ "name": name }))
                .collect();
            Ok(json!(tables))
        })
    })
}

pub fn get_columns(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let table = req_str(params, "table")?;
            let mut grouped = columns_grouped(&c, &owner, Some(&table))?;
            Ok(json!(grouped.remove(&table).unwrap_or_default()))
        })
    })
}

pub fn get_foreign_keys(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let table = req_str(params, "table")?;
            let mut grouped = foreign_keys_grouped(&c, &owner, Some(&table))?;
            Ok(json!(grouped.remove(&table).unwrap_or_default()))
        })
    })
}

pub fn get_indexes(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let table = req_str(params, "table")?;
            Ok(json!(indexes_for(&c, &owner, &table)?))
        })
    })
}

pub fn get_views(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let r = c.query(
                "SELECT view_name FROM all_views WHERE owner = :1 ORDER BY view_name",
                &[json!(owner)],
            )?;
            let views: Vec<Value> = r
                .rows
                .iter()
                .filter_map(|row| cell_str(row, 0))
                .map(|name| json!({ "name": name, "definition": Value::Null }))
                .collect();
            Ok(json!(views))
        })
    })
}

pub fn get_view_definition(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let view = req_str_any(params, &["view_name", "view"])?;
            // ALL_VIEWS.TEXT is a LONG column; the client fetches it as text.
            let def = c
                .query_single(
                    "SELECT text FROM all_views WHERE owner = :1 AND view_name = :2",
                    &[json!(owner), json!(view)],
                )?
                .unwrap_or_default();
            Ok(Value::String(def.trim().to_string()))
        })
    })
}

pub fn get_view_columns(id: Value, params: &Value) -> Value {
    // ALL_TAB_COLUMNS covers views as well as tables.
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let view = req_str_any(params, &["view_name", "view"])?;
            let mut grouped = columns_grouped(&c, &owner, Some(&view))?;
            Ok(json!(grouped.remove(&view).unwrap_or_default()))
        })
    })
}

pub fn get_routines(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let r = c.query(
                "SELECT object_name, object_type FROM all_objects \
                 WHERE owner = :1 AND object_type IN ('PROCEDURE', 'FUNCTION') \
                 ORDER BY object_name",
                &[json!(owner)],
            )?;
            let routines: Vec<Value> = r
                .rows
                .iter()
                .filter_map(|row| {
                    let name = cell_str(row, 0)?;
                    let kind = cell_str(row, 1)?;
                    Some(json!({ "name": name, "routine_type": kind, "definition": Value::Null }))
                })
                .collect();
            Ok(json!(routines))
        })
    })
}

pub fn get_routine_parameters(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let routine = req_str_any(params, &["routine_name", "routine"])?;
            let r = c.query(
                "SELECT argument_name, data_type, in_out, position FROM all_arguments \
                 WHERE owner = :1 AND object_name = :2 \
                   AND package_name IS NULL AND argument_name IS NOT NULL \
                 ORDER BY sequence",
                &[json!(owner), json!(routine)],
            )?;
            let parameters: Vec<Value> = r
                .rows
                .iter()
                .map(|row| {
                    // Oracle reports IN / OUT / IN/OUT; the host expects INOUT.
                    let mode = match cell_str(row, 2).unwrap_or_default().as_str() {
                        "IN/OUT" => "INOUT".to_string(),
                        other => other.to_string(),
                    };
                    json!({
                        "name": cell_str(row, 0).unwrap_or_default(),
                        "data_type": cell_str(row, 1).unwrap_or_default(),
                        "mode": mode,
                        "ordinal_position": cell_i64(row, 3),
                    })
                })
                .collect();
            Ok(json!(parameters))
        })
    })
}

pub fn get_routine_definition(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let routine = req_str_any(params, &["routine_name", "routine"])?;
            let r = c.query(
                "SELECT text FROM all_source \
                 WHERE owner = :1 AND name = :2 \
                   AND type IN ('PROCEDURE', 'FUNCTION') \
                 ORDER BY line",
                &[json!(owner), json!(routine)],
            )?;
            let definition: String = r.rows.iter().filter_map(|row| cell_str(row, 0)).collect();
            Ok(Value::String(definition))
        })
    })
}

pub fn create_view(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let name = req_str_any(params, &["view_name", "name"])?;
            let definition = req_str(params, "definition")?;
            c.execute(
                &format!(
                    "CREATE VIEW {} AS {}",
                    qualify(Some(&owner), &name),
                    definition
                ),
                &[],
            )?;
            Ok(Value::Null)
        })
    })
}

pub fn alter_view(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let name = req_str_any(params, &["view_name", "name"])?;
            let definition = req_str(params, "definition")?;
            c.execute(
                &format!(
                    "CREATE OR REPLACE VIEW {} AS {}",
                    qualify(Some(&owner), &name),
                    definition
                ),
                &[],
            )?;
            Ok(Value::Null)
        })
    })
}

pub fn drop_view(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|c| {
            let owner = resolve_schema(&c, params)?;
            let name = req_str_any(params, &["view_name", "name"])?;
            c.execute(&format!("DROP VIEW {}", qualify(Some(&owner), &name)), &[])?;
            Ok(Value::Null)
        })
    })
}

pub fn get_schema_snapshot(id: Value, params: &Value) -> Value {
    respond(id, get_schema_snapshot_impl(params))
}

/// The host deserialises this as `Vec<TableSchema { name, columns, foreign_keys }>`.
fn get_schema_snapshot_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let owner = resolve_schema(&client, params)?;

    let names = list_table_names(&client, &owner)?;
    let mut all_columns = columns_grouped(&client, &owner, None)?;
    let mut all_fks = foreign_keys_grouped(&client, &owner, None)?;

    let tables: Vec<Value> = names
        .into_iter()
        .map(|name| {
            let columns = all_columns.remove(&name).unwrap_or_default();
            let foreign_keys = all_fks.remove(&name).unwrap_or_default();
            json!({ "name": name, "columns": columns, "foreign_keys": foreign_keys })
        })
        .collect();

    Ok(json!(tables))
}

pub fn get_all_columns_batch(id: Value, params: &Value) -> Value {
    respond(id, batch_impl(params, columns_grouped))
}

pub fn get_all_foreign_keys_batch(id: Value, params: &Value) -> Value {
    respond(id, batch_impl(params, foreign_keys_grouped))
}

fn batch_impl(
    params: &Value,
    extract: fn(&Client, &str, Option<&str>) -> Result<GroupedRows, PluginError>,
) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let owner = resolve_schema(&client, params)?;

    let requested: Vec<String> = params
        .get("tables")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let tables = if requested.is_empty() {
        list_table_names(&client, &owner)?
    } else {
        requested
    };

    let mut grouped = extract(&client, &owner, None)?;
    let mut out = serde_json::Map::new();
    for table in tables {
        let entries = grouped.remove(&table).unwrap_or_default();
        out.insert(table, json!(entries));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_data_types() {
        assert_eq!(
            format_data_type("NUMBER", 0, 22, json!(10), json!(2)),
            "NUMBER(10,2)"
        );
        assert_eq!(
            format_data_type("NUMBER", 0, 22, json!(10), json!(0)),
            "NUMBER(10)"
        );
        assert_eq!(
            format_data_type("NUMBER", 0, 22, Value::Null, Value::Null),
            "NUMBER"
        );
        assert_eq!(
            format_data_type("VARCHAR2", 255, 255, Value::Null, Value::Null),
            "VARCHAR2(255)"
        );
        assert_eq!(
            format_data_type("RAW", 0, 2000, Value::Null, Value::Null),
            "RAW(2000)"
        );
        assert_eq!(
            format_data_type("TIMESTAMP(6)", 0, 11, Value::Null, json!(6)),
            "TIMESTAMP(6)"
        );
        assert_eq!(
            format_data_type("DATE", 0, 7, Value::Null, Value::Null),
            "DATE"
        );
        assert_eq!(
            format_data_type("FLOAT", 0, 22, json!(126), Value::Null),
            "FLOAT(126)"
        );
    }

    #[test]
    fn char_types_detected() {
        assert!(is_char_type("VARCHAR2"));
        assert!(is_char_type("NCHAR"));
        assert!(!is_char_type("NUMBER"));
        assert!(!is_char_type("CLOB"));
    }
}
