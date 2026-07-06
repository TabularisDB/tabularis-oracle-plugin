//! JSON-RPC method handlers, split by concern.

pub mod crud;
pub mod ddl;
pub mod metadata;
pub mod query;

use serde_json::Value;

use crate::client::Client;
use crate::error::PluginError;
use crate::models::{inner_params, ConnectionParams};
use crate::rpc::{error_response, ok_response};
use crate::settings;

/// Handle the `initialize` lifecycle call: apply plugin settings (Oracle
/// client library location) before any connection is attempted.
pub fn initialize(id: Value, params: &Value) -> Value {
    if let Some(s) = params.get("settings") {
        settings::apply(s);
    }
    ok_response(id, Value::Null)
}

/// Open (or reuse) a connection from the `params.params` block every method carries.
pub fn connect(params: &Value) -> Result<Client, PluginError> {
    let cp = ConnectionParams::from_value(inner_params(params));
    Client::connect(&cp)
}

/// Turn a handler result into a JSON-RPC response.
pub fn respond(id: Value, result: Result<Value, PluginError>) -> Value {
    match result {
        Ok(value) => ok_response(id, value),
        Err(err) => error_response(id, err.code, &err.message),
    }
}

/// Read a required string parameter from the top-level params object.
pub fn req_str(params: &Value, key: &str) -> Result<String, PluginError> {
    opt_str(params, key)
        .ok_or_else(|| PluginError::invalid_params(format!("missing '{key}' parameter")))
}

/// Read an optional string parameter (absent, null and empty all count as missing).
pub fn opt_str(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Read a required string parameter that may arrive under any of several keys
/// (the host and the published guide disagree on some names).
pub fn req_str_any(params: &Value, keys: &[&str]) -> Result<String, PluginError> {
    keys.iter()
        .find_map(|k| opt_str(params, k))
        .ok_or_else(|| PluginError::invalid_params(format!("missing '{}' parameter", keys[0])))
}

/// The schema every metadata/CRUD method operates on: the explicit `schema`
/// request parameter, or the session's current schema (= the login user unless
/// ALTER SESSION changed it).
pub fn resolve_schema(client: &Client, params: &Value) -> Result<String, PluginError> {
    if let Some(schema) = opt_str(params, "schema") {
        return Ok(schema);
    }
    client
        .query_single(
            "SELECT SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') FROM DUAL",
            &[],
        )?
        .ok_or_else(|| PluginError::internal("could not determine current schema"))
}

/// A cell from a result row, defaulting to JSON null when out of range.
pub fn cell(row: &[Value], i: usize) -> Value {
    row.get(i).cloned().unwrap_or(Value::Null)
}

pub fn cell_str(row: &[Value], i: usize) -> Option<String> {
    row.get(i).and_then(Value::as_str).map(str::to_string)
}

pub fn cell_i64(row: &[Value], i: usize) -> i64 {
    row.get(i)
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
}
