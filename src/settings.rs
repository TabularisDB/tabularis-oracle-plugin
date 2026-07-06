//! Plugin settings received via the `initialize` RPC call.
//!
//! The only setting today is `client_lib_dir`: the directory holding the
//! Oracle Instant Client libraries. ODPI-C must know about it *before* the
//! first connection attempt, so `initialize` applies it immediately through
//! `oracle::InitParams`. Applying twice is harmless (the second call fails
//! silently once the client library is already loaded).

use serde_json::Value;

/// Apply the settings object from the `initialize` request. Errors are logged
/// to stderr rather than surfaced: the host ignores `initialize` failures, and
/// a bad optional setting should not block a plugin that may still work via
/// PATH / LD_LIBRARY_PATH.
pub fn apply(settings: &Value) {
    let dir = settings
        .get("client_lib_dir")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(dir) = dir {
        let mut params = oracle::InitParams::new();
        let applied = params
            .oracle_client_lib_dir(dir)
            .map_err(|e| e.to_string())
            .and_then(|p| p.init().map(|_| ()).map_err(|e| e.to_string()));
        if let Err(err) = applied {
            eprintln!("oracle-plugin: could not apply client_lib_dir '{dir}': {err}");
        }
    }
}
