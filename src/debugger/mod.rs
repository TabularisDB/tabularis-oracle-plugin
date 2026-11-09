//! Isolated native debugging; no automatic grants, compilation or helper DDL.
mod metadata;
mod session;
#[cfg(test)]
mod tests;
use crate::{
    error::PluginError,
    models::{inner_params, ConnectionParams},
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};
type Result<T> = std::result::Result<T, PluginError>;
fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::invalid_params(message)
}
fn unavailable(reason: &str) -> Value {
    json!({"state":"unavailable","reason":reason})
}
fn available(value: Value) -> Value {
    json!({"state":"available","value":value})
}
fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 1024)
        .ok_or_else(|| invalid(format!("Missing or invalid debugger field '{name}'")))
}
struct Request {
    cancelled: Arc<AtomicBool>,
    session: Option<String>,
}
#[derive(Default)]
struct Registry {
    previews: HashMap<String, (Instant, metadata::Preview)>,
    sessions: HashMap<String, Arc<session::Session>>,
    started: HashSet<String>,
    requests: HashMap<String, Request>,
}
static CLOSING: AtomicBool = AtomicBool::new(false);
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
pub struct RequestTicket(String);
impl Drop for RequestTicket {
    fn drop(&mut self) {
        registry().lock().unwrap().requests.remove(&self.0);
    }
}
pub fn begin_request(id: &Value, params: &Value) -> Result<RequestTicket> {
    let mut registry = registry().lock().unwrap();
    let id = id.to_string();
    if registry.requests.len() >= 32 || registry.requests.contains_key(&id) {
        return Err(invalid(
            "Too many pending debugger requests or duplicate request ID",
        ));
    }
    registry.requests.insert(
        id.clone(),
        Request {
            cancelled: Arc::new(AtomicBool::new(false)),
            session: params["command"]["session_id"].as_str().map(str::to_string),
        },
    );
    Ok(RequestTicket(id))
}
pub fn cancel_request(id: &Value) {
    let session = {
        let registry = registry().lock().unwrap();
        registry.requests.get(&id.to_string()).and_then(|request| {
            request.cancelled.store(true, Ordering::Release);
            request
                .session
                .as_ref()
                .and_then(|id| registry.sessions.get(id).cloned())
        })
    };
    if let Some(session) = session {
        std::thread::spawn(move || {
            let _ = session.stop();
        });
    }
}
fn check_keys(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("Debugger command must be an object"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(
            "Unknown debugger field; raw SQL/expressions are not supported",
        ));
    }
    Ok(())
}
fn validate(params: &Value) -> Result<(&Value, &str)> {
    if params["version"].as_u64() != Some(1) {
        return Err(invalid("Unsupported routine debugger protocol version"));
    }
    let command = params
        .get("command")
        .ok_or_else(|| invalid("Missing debugger command"))?;
    let action = text(command, "action")?;
    let fields: &[&str] = match action {
        "preview" => &["action", "target", "arguments"],
        "start" => &["action", "preview_id", "session_id"],
        "stop" | "continue" | "status" | "read_stack" => &["action", "session_id"],
        "step" => &["action", "session_id", "mode"],
        "set_breakpoints" => &["action", "session_id", "breakpoints"],
        "read_variables" => &["action", "session_id", "frame_id", "names"],
        _ => return Err(invalid("Unknown debugger action")),
    };
    check_keys(command, fields)?;
    if action == "preview" {
        check_keys(
            &command["target"],
            &[
                "database",
                "schema",
                "name",
                "routine_type",
                "identity_args",
            ],
        )?;
        if let Some(arguments) = command["arguments"].as_array() {
            for argument in arguments {
                check_keys(argument, &["position", "value"])?;
            }
        }
    } else {
        let id = text(command, "session_id")?;
        if id.len() > 128
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        {
            return Err(invalid("Invalid debug session identity"));
        }
    }
    Ok((command, action))
}
pub fn handle(id: Value, params: &Value) -> Value {
    crate::handlers::respond(id.clone(), execute(&id, params))
}
fn execute(id: &Value, params: &Value) -> Result<Value> {
    let (command, action) = validate(params)?;
    let cancelled = registry()
        .lock()
        .unwrap()
        .requests
        .get(&id.to_string())
        .map(|r| r.cancelled.clone())
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    if cancelled.load(Ordering::Acquire) || CLOSING.load(Ordering::Acquire) {
        return Err(invalid("Debugger request cancelled"));
    }
    if action == "preview" {
        let connection_params = ConnectionParams::from_value(inner_params(params));
        let connection = session::connect(&connection_params)?;
        let preview = metadata::read(&connection, connection_params, command)?;
        connection.close()?;
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let preview_id = format!(
            "{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let response = preview.response(&preview_id);
        let mut registry = registry().lock().unwrap();
        registry
            .previews
            .retain(|_, (time, _)| time.elapsed() < Duration::from_secs(300));
        if registry.previews.len() >= 64 {
            return Err(invalid(
                "Too many debugger previews; wait for unused previews to expire",
            ));
        }
        registry
            .previews
            .insert(preview_id, (Instant::now(), preview));
        return Ok(response);
    }
    let session_id = text(command, "session_id")?;
    if action == "start" {
        let preview_id = text(command, "preview_id")?;
        let preview = {
            let mut registry = registry().lock().unwrap();
            if let Some(existing) = registry.sessions.get(session_id).cloned() {
                if existing.preview_id != preview_id {
                    return Err(invalid(
                        "Session identity was already used for another preview",
                    ));
                }
                drop(registry);
                return existing.command(&json!({"action":"status"}));
            }
            if registry.started.len() >= 1024 || registry.started.contains(session_id) {
                return Err(invalid(
                    "Session identity was already started; execution is never replayed",
                ));
            }
            registry.sessions.retain(|_, s| !s.old());
            if registry.sessions.values().filter(|s| !s.released()).count() >= 16 {
                return Err(invalid("Too many active Oracle debugger sessions"));
            }
            let (created, preview) = registry
                .previews
                .get(preview_id)
                .ok_or_else(|| invalid("Debugger preview expired or not found"))?;
            if created.elapsed() >= Duration::from_secs(300) {
                return Err(invalid("Debugger preview expired"));
            }
            let preview = preview.clone();
            // Reserve before connection setup: failed starts are never replayed.
            registry.started.insert(session_id.to_string());
            preview
        };
        let session = session::Session::prepare(session_id, preview_id.to_string(), preview)?;
        {
            let mut registry = registry().lock().unwrap();
            if cancelled.load(Ordering::Acquire) || CLOSING.load(Ordering::Acquire) {
                return Err(invalid("Debugger start cancelled before execution"));
            }
            registry
                .sessions
                .insert(session_id.to_string(), session.clone());
            session.activate();
        }
        start_lease_worker();
        return session.initial_stop();
    }
    let session = {
        let mut registry = registry().lock().unwrap();
        if let Some(session) = registry.sessions.get(session_id).cloned() {
            session
        } else if action == "stop" && !registry.started.contains(session_id) {
            // A never-started preview uses its native preview identity for stop.
            registry.previews.remove(session_id);
            let unread = unavailable("Execution was never started");
            return Ok(json!({"kind":"session","version":1,"session":{
                "session_id":session_id,"revision":0,"execution":"cancelled","transaction":"unknown","resources":"released",
                "source":unread,"stop":unread,"stack":unread,"variables":unread,"outputs":unread,"breakpoints":unread,"error":null
            }}));
        } else {
            return Err(invalid("Native debug session not found"));
        }
    };
    session.command(command)
}
fn start_lease_worker() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(|| {
            while !CLOSING.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_secs(1));
                let expired: Vec<_> = registry()
                    .lock()
                    .unwrap()
                    .sessions
                    .values()
                    .filter(|s| s.expired())
                    .cloned()
                    .collect();
                for session in expired {
                    let _ = session.stop();
                }
            }
        });
    });
}
pub fn shutdown() {
    CLOSING.store(true, Ordering::Release);
    let sessions: Vec<_> = registry()
        .lock()
        .unwrap()
        .sessions
        .values()
        .cloned()
        .collect();
    for session in sessions {
        let _ = session.stop();
    }
}
