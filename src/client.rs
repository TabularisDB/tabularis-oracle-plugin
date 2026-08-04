//! Connection layer: build an Oracle connect string from the Tabularis
//! connection form, keep the one session per plugin process alive across RPC
//! calls, and expose a single `query`/`execute` surface for the handlers.
//!
//! Oracle sessions are expensive to open (typically 50–500 ms), and Tabularis
//! reuses one plugin process per connection session, so a single-slot cache
//! holds the last connection. Reuse is guarded by a cheap OCI `ping()`; a dead
//! session is transparently replaced.

use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use oracle::sql_type::{OracleType, ToSql};
use oracle::{Connection, Row};
use serde_json::{json, Value};

use crate::error::PluginError;
use crate::models::ConnectionParams;
use crate::utils::json_columns::{
    is_unsupported_json_error, json_safe_projection, parse_describe, DESCRIBE_COLUMNS_BLOCK,
};

/// A uniform result shape for the handlers.
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub affected: u64,
}

struct CachedConn {
    key: String,
    conn: Connection,
}

/// The plugin serves one Tabularis connection per process, so one slot is
/// enough. Guarded by a mutex only because statics require it.
static SLOT: Mutex<Option<CachedConn>> = Mutex::new(None);

pub struct Client {
    key: String,
    conn: Option<Connection>,
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if let Ok(mut slot) = SLOT.lock() {
                *slot = Some(CachedConn {
                    key: self.key.clone(),
                    conn,
                });
            }
        }
    }
}

impl Client {
    pub fn connect(params: &ConnectionParams) -> Result<Self, PluginError> {
        let username = params
            .username
            .as_deref()
            .ok_or_else(|| PluginError::invalid_params("missing username"))?;
        let password = params.password.as_deref().unwrap_or("");
        let target = connect_string(params)?;
        let key = format!("{username}\u{1}{target}");

        if let Ok(mut slot) = SLOT.lock() {
            if let Some(cached) = slot.take() {
                if cached.key == key && cached.conn.ping().is_ok() {
                    return Ok(Client {
                        key,
                        conn: Some(cached.conn),
                    });
                }
            }
        }

        let mut conn = Connection::connect(username, password, &target).map_err(|e| {
            let base: PluginError = e.into();
            PluginError {
                code: base.code,
                message: format!("cannot connect to '{target}': {}", base.message),
            }
        })?;
        conn.set_autocommit(true);
        Ok(Client {
            key,
            conn: Some(conn),
        })
    }

    fn conn(&self) -> &Connection {
        // `conn` is only None after Drop has run.
        self.conn.as_ref().expect("connection already released")
    }

    /// Run a row-returning statement (SELECT / WITH).
    ///
    /// Statements touching native JSON columns fail at define time inside
    /// rust-oracle ("unsupported Oracle type JSON"); those are transparently
    /// retried with the JSON columns serialized to CLOB text (see
    /// `utils::json_columns`). Any other failure — including a failure of the
    /// recovery itself — surfaces the original error.
    pub fn query(&self, sql: &str, args: &[Value]) -> Result<QueryResult, PluginError> {
        match self.query_inner(sql, args) {
            Err(err) if is_unsupported_json_error(&err.message) => match self.json_safe_rewrite(sql) {
                Ok(Some(rewritten)) => match self.query_inner(&rewritten, args) {
                    Ok(result) => Ok(result),
                    Err(_) => Err(err),
                },
                _ => Err(err),
            },
            other => other,
        }
    }

    /// Describe `sql` server-side and, when it selects JSON columns, return
    /// the equivalent query with those columns wrapped in `JSON_SERIALIZE`.
    fn json_safe_rewrite(&self, sql: &str) -> Result<Option<String>, PluginError> {
        let mut stmt = self.conn().statement(DESCRIBE_COLUMNS_BLOCK).build()?;
        stmt.execute(&[&sql.to_string(), &OracleType::Varchar2(32767)])?;
        let buf: String = stmt.bind_value(2)?;
        Ok(json_safe_projection(&parse_describe(&buf))
            .map(|projection| format!("SELECT {projection} FROM ({sql})")))
    }

    fn query_inner(&self, sql: &str, args: &[Value]) -> Result<QueryResult, PluginError> {
        let binds = to_binds(args);
        let bind_refs: Vec<&dyn ToSql> = binds.iter().map(|b| b.as_ref()).collect();

        let mut stmt = self.conn().statement(sql).build()?;
        let rows = stmt.query(&bind_refs)?;

        let column_types: Vec<(String, OracleType)> = rows
            .column_info()
            .iter()
            .map(|c| (c.name().to_string(), c.oracle_type().clone()))
            .collect();
        let columns: Vec<String> = column_types.iter().map(|(n, _)| n.clone()).collect();

        let mut out_rows = Vec::new();
        for row_result in rows {
            let row = row_result?;
            let mut cells = Vec::with_capacity(column_types.len());
            for (i, (_, oracle_type)) in column_types.iter().enumerate() {
                cells.push(fetch_cell(&row, i, oracle_type));
            }
            out_rows.push(cells);
        }

        Ok(QueryResult {
            columns,
            rows: out_rows,
            affected: 0,
        })
    }

    /// Run a non-row statement (DML/DDL/PLSQL) and return the affected-row count.
    pub fn execute(&self, sql: &str, args: &[Value]) -> Result<u64, PluginError> {
        let binds = to_binds(args);
        let bind_refs: Vec<&dyn ToSql> = binds.iter().map(|b| b.as_ref()).collect();

        let mut stmt = self.conn().statement(sql).build()?;
        stmt.execute(&bind_refs)?;
        Ok(stmt.row_count()?)
    }

    /// Cheap connectivity check used by `test_connection` and `ping`.
    pub fn health_check(&self) -> Result<(), PluginError> {
        self.conn().ping()?;
        Ok(())
    }

    /// First cell of the first row, as a string.
    pub fn query_single(&self, sql: &str, args: &[Value]) -> Result<Option<String>, PluginError> {
        let r = self.query(sql, args)?;
        Ok(r.rows
            .first()
            .and_then(|row| row.first())
            .and_then(Value::as_str)
            .map(str::to_string))
    }
}

// ---------------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------------

fn to_binds(args: &[Value]) -> Vec<Box<dyn ToSql>> {
    args.iter().map(json_to_bind).collect()
}

fn json_to_bind(value: &Value) -> Box<dyn ToSql> {
    match value {
        Value::Null => Box::new(None::<String>),
        // Oracle SQL has no portable boolean; bind as 0/1 like NUMBER(1) columns expect.
        Value::Bool(b) => Box::new(if *b { 1i64 } else { 0i64 }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Box::new(i)
            } else if let Some(f) = n.as_f64() {
                Box::new(f)
            } else {
                Box::new(n.to_string())
            }
        }
        Value::String(s) => Box::new(s.clone()),
        // Arrays/objects are bound as their JSON text representation.
        other => Box::new(other.to_string()),
    }
}

fn fetch_cell(row: &Row, i: usize, oracle_type: &OracleType) -> Value {
    match oracle_type {
        OracleType::Number(_, _)
        | OracleType::Float(_)
        | OracleType::BinaryFloat
        | OracleType::BinaryDouble
        | OracleType::Int64
        | OracleType::UInt64 => match row.get::<usize, Option<String>>(i) {
            // Fetch as text (NLS-independent) and re-parse to keep precision.
            Ok(Some(s)) => parse_number(&s),
            _ => Value::Null,
        },
        OracleType::Raw(_) | OracleType::LongRaw | OracleType::BLOB => {
            match row.get::<usize, Option<Vec<u8>>>(i) {
                Ok(Some(bytes)) => Value::String(STANDARD.encode(bytes)),
                _ => Value::Null,
            }
        }
        // Everything else (strings, dates, timestamps, intervals, CLOB, LONG,
        // ROWID, JSON, ...) renders as text. Types with no text conversion
        // (object types, BFILE, nested cursors) degrade to NULL instead of
        // failing the whole row.
        _ => match row.get::<usize, Option<String>>(i) {
            Ok(v) => v.map(Value::String).unwrap_or(Value::Null),
            Err(_) => Value::Null,
        },
    }
}

/// Parse Oracle's text rendering of a number into a JSON number, keeping
/// integers exact. Integers too large for i64 stay strings rather than losing
/// precision through f64.
fn parse_number(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        return json!(i);
    }
    let is_plain_integer = {
        let digits = s.strip_prefix('-').unwrap_or(s);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    };
    if is_plain_integer {
        return Value::String(s.to_string());
    }
    match s.parse::<f64>() {
        Ok(f) if f.is_finite() => json!(f),
        _ => Value::String(s.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Connect-string building (pure logic, unit-tested)
// ---------------------------------------------------------------------------

/// Build the ODPI-C connect string from the connection form.
///
/// - `database` containing `(` is a full connect descriptor → used as-is.
/// - `database` starting with `//` or a `tcp(s)://` scheme is EZConnect → as-is.
/// - `host` present → EZConnect `[tcps://]host[:port]/service` where the
///   service name comes from `database`.
/// - Otherwise `database` alone is treated as a TNS alias / EZConnect string.
pub fn connect_string(params: &ConnectionParams) -> Result<String, PluginError> {
    let database = params.database.as_deref().unwrap_or("").trim();

    if database.contains('(') {
        return Ok(database.to_string());
    }
    let lower = database.to_ascii_lowercase();
    if database.starts_with("//") || lower.starts_with("tcp://") || lower.starts_with("tcps://") {
        return Ok(database.to_string());
    }

    if let Some(host) = params.host.as_deref().map(str::trim).filter(|h| !h.is_empty()) {
        if database.is_empty() {
            return Err(PluginError::invalid_params(
                "missing service name: put the Oracle service name (e.g. FREEPDB1 or ORCLPDB1) in the Database field",
            ));
        }
        let use_tls = matches!(
            params.ssl_mode.as_deref(),
            Some("require") | Some("verify-ca") | Some("verify-full") | Some("verify_identity")
        );
        let prefix = if use_tls { "tcps://" } else { "//" };
        let port = params.port.unwrap_or(1521);
        return Ok(format!("{prefix}{host}:{port}/{database}"));
    }

    if !database.is_empty() {
        // A bare name with no host: let the client resolve it (TNS alias,
        // LDAP, or a full EZConnect string typed straight into the field).
        return Ok(database.to_string());
    }

    Err(PluginError::invalid_params(
        "no connection target: provide host + service name, or a full EZConnect string / connect descriptor / TNS alias in the Database field",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(
        host: Option<&str>,
        port: Option<u16>,
        database: Option<&str>,
        ssl_mode: Option<&str>,
    ) -> ConnectionParams {
        ConnectionParams {
            host: host.map(String::from),
            port,
            database: database.map(String::from),
            ssl_mode: ssl_mode.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn host_and_service_build_ezconnect() {
        let p = params(Some("db.example.com"), Some(1521), Some("FREEPDB1"), None);
        assert_eq!(connect_string(&p).unwrap(), "//db.example.com:1521/FREEPDB1");
    }

    #[test]
    fn default_port_is_1521() {
        let p = params(Some("localhost"), None, Some("XEPDB1"), None);
        assert_eq!(connect_string(&p).unwrap(), "//localhost:1521/XEPDB1");
    }

    #[test]
    fn ssl_mode_require_uses_tcps() {
        let p = params(Some("adb.eu-1.oraclecloud.com"), Some(1522), Some("mydb_high"), Some("require"));
        assert_eq!(
            connect_string(&p).unwrap(),
            "tcps://adb.eu-1.oraclecloud.com:1522/mydb_high",
        );
    }

    #[test]
    fn full_descriptor_passes_through() {
        let desc = "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=h)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=s)))";
        let p = params(Some("ignored"), None, Some(desc), None);
        assert_eq!(connect_string(&p).unwrap(), desc);
    }

    #[test]
    fn ezconnect_in_database_field_passes_through() {
        let p = params(None, None, Some("//h:1521/svc"), None);
        assert_eq!(connect_string(&p).unwrap(), "//h:1521/svc");
    }

    #[test]
    fn bare_database_is_tns_alias() {
        let p = params(None, None, Some("MYALIAS"), None);
        assert_eq!(connect_string(&p).unwrap(), "MYALIAS");
    }

    #[test]
    fn host_without_service_is_an_error() {
        let p = params(Some("localhost"), None, None, None);
        assert!(connect_string(&p).is_err());
    }

    #[test]
    fn empty_params_are_an_error() {
        let p = params(None, None, None, None);
        assert!(connect_string(&p).is_err());
    }

    #[test]
    fn parses_numbers_precisely() {
        assert_eq!(parse_number("42"), json!(42));
        assert_eq!(parse_number("-7"), json!(-7));
        assert_eq!(parse_number("3.25"), json!(3.25));
        // 39-digit NUMBER survives as a string instead of degrading to f64.
        let big = "123456789012345678901234567890123456789";
        assert_eq!(parse_number(big), Value::String(big.to_string()));
        assert_eq!(parse_number("1E10"), json!(1e10));
    }
}
