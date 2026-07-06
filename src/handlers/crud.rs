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
    let pk_col = req_str(params, "pk_col")?;
    let col_name = req_str(params, "col_name")?;
    let pk_val = req_value(params, "pk_val")?;
    let new_val = req_value(params, "new_val")?;

    let sql = format!(
        "UPDATE {} SET {} = :1 WHERE {} = :2",
        qualify(schema.as_deref(), &table),
        quote(&col_name),
        quote(&pk_col),
    );
    let affected = client.execute(&sql, &[new_val, pk_val])?;
    Ok(json!(affected))
}

pub fn delete_record(id: Value, params: &Value) -> Value {
    respond(id, delete_impl(params))
}

fn delete_impl(params: &Value) -> Result<Value, PluginError> {
    let client = connect(params)?;
    let schema = opt_str(params, "schema");
    let table = req_str(params, "table")?;
    let pk_col = req_str(params, "pk_col")?;
    let pk_val = req_value(params, "pk_val")?;

    let sql = format!(
        "DELETE FROM {} WHERE {} = :1",
        qualify(schema.as_deref(), &table),
        quote(&pk_col)
    );
    let affected = client.execute(&sql, &[pk_val])?;
    Ok(json!(affected))
}
