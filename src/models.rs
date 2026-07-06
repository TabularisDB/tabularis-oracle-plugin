//! Shared request shapes.
//!
//! `ConnectionParams` mirrors the values the user typed into the Tabularis
//! connection form. For Oracle: `host`/`port` point at the listener,
//! `database` holds the service name (or a full EZConnect string / connect
//! descriptor / TNS alias), and `username`/`password` are the credentials.

use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct ConnectionParams {
    pub driver: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub ssl_mode: Option<String>,
}

impl ConnectionParams {
    pub fn from_value(value: &Value) -> Self {
        let obj = value.as_object();
        let get_str = |k: &str| {
            obj.and_then(|o| o.get(k))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let port = obj
            .and_then(|o| o.get("port"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .and_then(|p| u16::try_from(p).ok());

        // The host serialises `database` as an untagged enum: either a plain
        // string or an array of names (multi-select). Oracle serves one
        // service per connection, so take the first entry.
        let database = obj
            .and_then(|o| o.get("database"))
            .and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Array(arr) => arr.iter().find_map(|e| e.as_str().map(str::to_string)),
                _ => None,
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Self {
            driver: get_str("driver"),
            host: get_str("host"),
            port,
            database,
            username: get_str("username"),
            password: get_str("password"),
            ssl_mode: get_str("ssl_mode"),
        }
    }
}

/// Extract the nested `params` object every RPC method receives. Tabularis
/// wraps the connection params in `params.params`.
pub fn inner_params(value: &Value) -> &Value {
    value.get("params").unwrap_or(&Value::Null)
}
