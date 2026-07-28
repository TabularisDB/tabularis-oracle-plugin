//! Row-level create/update/delete, using bound parameters throughout so values
//! are never interpolated into SQL. Table names are schema-qualified because
//! one Oracle session sees every schema it has rights on.

use serde_json::{json, Value};

use crate::error::PluginError;
use crate::handlers::{connect, opt_str, req_str, respond};
use crate::utils::identifiers::{qualify, quote};

/// Read a required value parameter of any JSON type.
fn req_value(params: &Value, key: &str) -> Result<Value, PluginError> {
    params
        .get(key)
        .cloned()
        .ok_or_else(|| PluginError::invalid_params(format!("missing '{key}' parameter")))
}

/// The primary-key filter for update/delete. The host sends `pk_map`
/// (column -> value, supporting composite keys); the published guide
/// documents `pk_col`/`pk_val`, so accept that too.
fn pk_pairs(params: &Value) -> Result<Vec<(String, Value)>, PluginError> {
    if let Some(map) = params.get("pk_map").and_then(Value::as_object) {
        if map.is_empty() {
            return Err(PluginError::invalid_params("'pk_map' object is empty"));
        }
        return Ok(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    }
    let pk_col = req_str(params, "pk_col")?;
    let pk_val = req_value(params, "pk_val")?;
    Ok(vec![(pk_col, pk_val)])
}

/// Build `WHERE` conditions for the key columns, binding values starting at
/// placeholder `:{first_bind}`. NULL keys become `IS NULL` (no bind).
fn pk_where(pairs: &[(String, Value)], first_bind: usize, args: &mut Vec<Value>) -> String {
    let mut bind = first_bind;
    let conds: Vec<String> = pairs
        .iter()
        .map(|(col, val)| {
            if val.is_null() {
                format!("{} IS NULL", quote(col))
            } else {
                args.push(val.clone());
                let c = format!("{} = :{}", quote(col), bind);
                bind += 1;
                c
            }
        })
        .collect();
    conds.join(" AND ")
}

pub fn insert_record(id: Value, params: &Value) -> Value {
    respond(id, insert_impl(params))
}

fn insert_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let schema = opt_str(params, "schema");
    let table = req_str(params, "table")?;
    let data = params
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| PluginError::invalid_params("missing 'data' object"))?;

    if data.is_empty() {
        return Err(PluginError::invalid_params("'data' object is empty"));
    }

    let mut columns = Vec::with_capacity(data.len());
    let mut placeholders = Vec::with_capacity(data.len());
    let mut args = Vec::with_capacity(data.len());
    for (i, (key, value)) in data.iter().enumerate() {
        columns.push(quote(key));
        placeholders.push(format!(":{}", i + 1));
        args.push(value.clone());
    }

    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        qualify(schema.as_deref(), &table),
        columns.join(", "),
        placeholders.join(", "),
    );
    // The host expects the affected-row count (u64), not null.
    let affected = client.execute(&sql, &args)?;
    Ok(json!(affected))
}

pub fn update_record(id: Value, params: &Value) -> Value {
    respond(id, update_impl(params))
}

fn update_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let schema = opt_str(params, "schema");
    let table = req_str(params, "table")?;
    let col_name = req_str(params, "col_name")?;
    let new_val = req_value(params, "new_val")?;
    let pk = pk_pairs(params)?;

    let mut args = vec![new_val];
    let conditions = pk_where(&pk, 2, &mut args);
    let sql = format!(
        "UPDATE {} SET {} = :1 WHERE {}",
        qualify(schema.as_deref(), &table),
        quote(&col_name),
        conditions,
    );
    let affected = client.execute(&sql, &args)?;
    Ok(json!(affected))
}

pub fn delete_record(id: Value, params: &Value) -> Value {
    respond(id, delete_impl(params))
}

fn delete_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let schema = opt_str(params, "schema");
    let table = req_str(params, "table")?;
    let pk = pk_pairs(params)?;

    let mut args = Vec::with_capacity(pk.len());
    let conditions = pk_where(&pk, 1, &mut args);
    let sql = format!(
        "DELETE FROM {} WHERE {}",
        qualify(schema.as_deref(), &table),
        conditions,
    );
    let affected = client.execute(&sql, &args)?;
    Ok(json!(affected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pk_map_takes_precedence_and_supports_composite_keys() {
        let params = json!({ "pk_map": { "ID": 42, "REGION": "EU" } });
        let pairs = pk_pairs(&params).unwrap();
        assert_eq!(pairs.len(), 2);

        let mut args = Vec::new();
        let where_clause = pk_where(&pairs, 2, &mut args);
        assert_eq!(where_clause, "\"ID\" = :2 AND \"REGION\" = :3");
        assert_eq!(args, vec![json!(42), json!("EU")]);
    }

    #[test]
    fn legacy_pk_col_pk_val_still_accepted() {
        let params = json!({ "pk_col": "ID", "pk_val": 7 });
        let pairs = pk_pairs(&params).unwrap();
        assert_eq!(pairs, vec![("ID".to_string(), json!(7))]);
    }

    #[test]
    fn null_key_becomes_is_null_without_bind() {
        let pairs = vec![("A".to_string(), Value::Null), ("B".to_string(), json!(1))];
        let mut args = Vec::new();
        let where_clause = pk_where(&pairs, 1, &mut args);
        assert_eq!(where_clause, "\"A\" IS NULL AND \"B\" = :1");
        assert_eq!(args, vec![json!(1)]);
    }

    #[test]
    fn missing_both_forms_is_invalid_params() {
        assert!(pk_pairs(&json!({})).is_err());
        assert!(pk_pairs(&json!({ "pk_map": {} })).is_err());
    }
}
