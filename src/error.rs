//! Plugin-local error type, kept deliberately small (no `anyhow`/`thiserror`).
//!
//! Every error carries a JSON-RPC error code so handlers can bubble failures
//! straight out to the host with `?`.

use std::fmt;

#[derive(Debug)]
pub struct PluginError {
    pub code: i64,
    pub message: String,
}

impl PluginError {
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
        }
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
        }
    }

    /// An operation the underlying database genuinely cannot perform. Surfaced
    /// as "method not found" so the host treats it as unsupported rather than a
    /// transient failure.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: msg.into(),
        }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for PluginError {}

impl From<oracle::Error> for PluginError {
    fn from(err: oracle::Error) -> Self {
        let text = err.to_string();
        // DPI-1047: ODPI-C could not load the Oracle Client library. Turn the
        // cryptic loader message into an actionable hint.
        if text.contains("DPI-1047") {
            return PluginError::internal(
                "Oracle Client library not found (DPI-1047). Install the Oracle \
                 Instant Client and either add it to the system PATH / \
                 LD_LIBRARY_PATH or set its directory in the plugin settings \
                 ('Oracle Client library directory').",
            );
        }
        PluginError::internal(format!("oracle error: {text}"))
    }
}
