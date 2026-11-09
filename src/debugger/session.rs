use oracle::sql_type::OracleType;
use oracle::Connection;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};

use super::metadata::{self, Preview};
use super::{available, invalid, text, unavailable, Result};

struct Completion {
    execution: &'static str,
    outputs: Value,
    error: Option<String>,
    closed: bool,
}
struct Control {
    connection: Option<Connection>,
    receiver: mpsc::Receiver<Completion>,
    snapshot: Value,
    breakpoints: Vec<(i64, Value)>,
    stop_serial: u64,
    entered: bool,
    target_closed: bool,
}

pub struct Session {
    pub preview_id: String,
    pub preview: Preview,
    target: Arc<Connection>,
    cancel: Arc<AtomicBool>,
    activation: Mutex<Option<mpsc::Sender<bool>>>,
    control: Mutex<Control>,
    touched: Mutex<Instant>,
    finished: AtomicBool,
    released: AtomicBool,
}

pub fn connect(params: &crate::models::ConnectionParams) -> Result<Connection> {
    let mut connection = Connection::connect(
        params
            .username
            .as_deref()
            .ok_or_else(|| invalid("Missing Oracle username"))?,
        params.password.as_deref().unwrap_or(""),
        crate::client::connect_string(params)?,
    )?;
    connection.set_autocommit(false);
    connection.set_call_timeout(Some(Duration::from_secs(3)))?;
    Ok(connection)
}

impl Session {
    pub fn prepare(id: &str, preview_id: String, preview: Preview) -> Result<Arc<Self>> {
        let target = connect(&preview.params)?;
        let current = metadata::read(
            &target,
            preview.params.clone(),
            &json!({"target":preview.target,"arguments":preview.arguments}),
        )?;
        if current.source != preview.source
            || current.parameters != preview.parameters
            || current.source_id != preview.source_id
        {
            return Err(invalid(
                "Routine source/signature changed after preview; preview again before starting",
            ));
        }
        target.execute("ALTER SESSION SET NLS_NUMERIC_CHARACTERS='.,'", &[])?;
        target.execute(
            "BEGIN DBMS_SESSION.SET_IDENTIFIER(:id); END;",
            &[&format!("tabularis-debug:{id}")],
        )?;
        let initialize = target.execute("DECLARE n PLS_INTEGER; BEGIN n:=DBMS_DEBUG.SET_TIMEOUT(180); DBMS_DEBUG.SET_TIMEOUT_BEHAVIOUR(DBMS_DEBUG.ABORT_ON_TIMEOUT); :debug_id:=DBMS_DEBUG.INITIALIZE; END;", &[&OracleType::Varchar2(128)])?;
        let native_id: String = initialize.bind_value("debug_id")?;
        drop(initialize);
        let controller = connect(&preview.params)?;
        controller.execute(
            "BEGIN DBMS_SESSION.SET_IDENTIFIER(:id); END;",
            &[&format!("tabularis-debug:{id}:control")],
        )?;
        controller.execute("DECLARE n PLS_INTEGER; BEGIN n:=DBMS_DEBUG.SET_TIMEOUT(1); DBMS_DEBUG.ATTACH_SESSION(:id); END;", &[&native_id])?;
        target.execute("BEGIN DBMS_DEBUG.DEBUG_ON; END;", &[])?;
        // The target call can wait at a breakpoint; cancellation uses OCI break plus
        // DBMS_DEBUG.ABORT_EXECUTION from the separate controller connection.
        target.set_call_timeout(None)?;
        let target = Arc::new(target);
        let cancel = Arc::new(AtomicBool::new(false));
        let (completion_tx, completion_rx) = mpsc::channel();
        let (activate_tx, activate_rx) = mpsc::channel();
        let worker_target = target.clone();
        let worker_cancel = cancel.clone();
        let plan = preview.clone();
        std::thread::spawn(move || {
            let run = activate_rx.recv().unwrap_or(false) && !worker_cancel.load(Ordering::Acquire);
            let result = if run {
                execute_target(&worker_target, &plan)
            } else {
                Err(invalid("Debug execution cancelled before start"))
            };
            let (execution, outputs, error) = match result {
                Ok(outputs) => ("completed", available(outputs), None),
                Err(error) => (
                    if worker_cancel.load(Ordering::Acquire) {
                        "cancelled"
                    } else {
                        "failed"
                    },
                    unavailable("Execution did not produce OUT/return values"),
                    Some(error.message),
                ),
            };
            let _ = worker_target.set_call_timeout(Some(Duration::from_secs(2)));
            let rollback = worker_target.rollback();
            let close = worker_target.close();
            let error = error
                .or_else(|| {
                    rollback
                        .err()
                        .map(|e| format!("Final rollback failed: {e}"))
                })
                .or_else(|| {
                    close
                        .as_ref()
                        .err()
                        .map(|e| format!("Target release failed: {e}"))
                });
            let _ = completion_tx.send(Completion {
                execution,
                outputs,
                error,
                closed: close.is_ok(),
            });
        });
        let snapshot = json!({"session_id":id,"revision":0,"execution":"running","transaction":"unknown","resources":"held",
            "source":preview.source,"stop":unavailable("No native stop yet"),"stack":unavailable("Read stack at a stop"),
            "variables":unavailable("Read variables at a stop"),"outputs":unavailable("Execution has not completed"),
            "breakpoints":available(json!([])),"error":null});
        Ok(Arc::new(Self {
            preview_id,
            preview,
            target,
            cancel,
            activation: Mutex::new(Some(activate_tx)),
            control: Mutex::new(Control {
                connection: Some(controller),
                receiver: completion_rx,
                snapshot,
                breakpoints: vec![],
                stop_serial: 0,
                entered: false,
                target_closed: false,
            }),
            touched: Mutex::new(Instant::now()),
            finished: AtomicBool::new(false),
            released: AtomicBool::new(false),
        }))
    }

    pub fn activate(&self) {
        if let Some(sender) = self.activation.lock().unwrap().take() {
            let _ = sender.send(true);
        }
    }

    pub fn expired(&self) -> bool {
        !self.released.load(Ordering::Acquire)
            && self.touched.lock().unwrap().elapsed() > Duration::from_secs(120)
    }
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub fn old(&self) -> bool {
        self.released() && self.touched.lock().unwrap().elapsed() > Duration::from_secs(300)
    }

    pub fn released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    pub fn initial_stop(&self) -> Result<Value> {
        let mut control = self.control.lock().unwrap();
        self.reach_entry(&mut control)?;
        Ok(response(&mut control))
    }

    fn reach_entry(&self, control: &mut Control) -> Result<()> {
        for _ in 0..512 {
            self.finish(control, Duration::ZERO);
            if self.finished() {
                return Ok(());
            }
            if control.snapshot["execution"] == "paused"
                && control.snapshot["source"] == self.preview.source
            {
                control.entered = true;
                return Ok(());
            }
            let flags = if control.snapshot["execution"] == "paused" {
                Some("DBMS_DEBUG.BREAK_ANY_CALL + DBMS_DEBUG.BREAK_EXCEPTION")
            } else {
                None
            };
            event(control, flags)?;
            if control.snapshot["execution"] == "running" {
                self.finish(control, Duration::from_millis(100));
                return Ok(());
            }
        }
        Err(invalid(
            "Could not reach the selected routine entry; stop the session",
        ))
    }

    pub fn command(&self, command: &Value) -> Result<Value> {
        *self.touched.lock().unwrap() = Instant::now();
        let action = text(command, "action")?;
        if action == "stop" {
            return self.stop();
        }
        let mut control = self.control.lock().unwrap();
        self.finish(&mut control, Duration::ZERO);
        if action == "status" {
            if !control.entered && !self.finished() {
                self.reach_entry(&mut control)?;
            } else if control.snapshot["execution"] == "running" {
                event(&mut control, None)?;
                self.finish(&mut control, Duration::ZERO);
            }
            return Ok(response(&mut control));
        }
        if control.snapshot["execution"] != "paused" || !control.entered {
            return Err(invalid(
                "Debugger operation requires a current native pause at the selected routine",
            ));
        }
        match action {
            "continue" | "step" => {
                let flags = if action == "continue" {
                    "DBMS_DEBUG.BREAK_EXCEPTION"
                } else {
                    match text(command, "mode")? {
                        "into" => "DBMS_DEBUG.BREAK_ANY_CALL + DBMS_DEBUG.BREAK_EXCEPTION",
                        "over" => "DBMS_DEBUG.BREAK_NEXT_LINE + DBMS_DEBUG.BREAK_EXCEPTION",
                        "out" => "DBMS_DEBUG.BREAK_ANY_RETURN + DBMS_DEBUG.BREAK_EXCEPTION",
                        _ => return Err(invalid("Unknown step mode")),
                    }
                };
                control.snapshot["execution"] = json!("running");
                stale(&mut control.snapshot["stack"]);
                stale(&mut control.snapshot["variables"]);
                event(&mut control, Some(flags))?;
                self.finish(&mut control, Duration::ZERO);
            }
            "set_breakpoints" => set_breakpoints(&mut control, &self.preview, command)?,
            "read_stack" => {
                if let Err(error) = read_stack(&mut control) {
                    control.snapshot["stack"] = unavailable(&error.message);
                }
            }
            "read_variables" => read_variables(&mut control, &self.preview, command)?,
            _ => return Err(invalid("Unsupported debugger operation")),
        }
        Ok(response(&mut control))
    }

    pub fn stop(&self) -> Result<Value> {
        self.cancel.store(true, Ordering::Release);
        if let Some(sender) = self.activation.lock().unwrap().take() {
            let _ = sender.send(false);
        }
        if !self.finished() {
            let _ = self.target.break_execution();
        }
        let mut control = self.control.lock().unwrap();
        self.finish(&mut control, Duration::ZERO);
        if !self.finished() {
            if control.connection.is_some() {
                let _ = event(&mut control, Some("DBMS_DEBUG.ABORT_EXECUTION"));
            }
            self.finish(&mut control, Duration::from_secs(2));
        }
        if !self.finished() {
            control.snapshot["execution"] = json!("unknown");
            control.snapshot["resources"] = json!("unknown");
            control.snapshot["error"] = json!(
                "Cancellation requested; target completion/resource release has not been confirmed"
            );
            stale(&mut control.snapshot["stack"]);
            stale(&mut control.snapshot["variables"]);
        } else if !self.released() {
            self.release_completed(&mut control);
        }
        Ok(response(&mut control))
    }

    fn finish(&self, control: &mut Control, wait: Duration) {
        if self.finished() {
            return;
        }
        let completion = if wait.is_zero() {
            control.receiver.try_recv().ok()
        } else {
            control.receiver.recv_timeout(wait).ok()
        };
        if let Some(completion) = completion {
            control.snapshot["execution"] = json!(completion.execution);
            control.target_closed = completion.closed;
            control.snapshot["transaction"] = json!("unknown");
            control.snapshot["error"] = json!(completion.error);
            control.snapshot["outputs"] = completion.outputs;
            control.snapshot["stop"] = unavailable("Execution ended");
            stale(&mut control.snapshot["stack"]);
            stale(&mut control.snapshot["variables"]);
            self.finished.store(true, Ordering::Release);
            self.release_completed(control);
        }
    }

    fn release_completed(&self, control: &mut Control) {
        if !control.target_closed {
            control.target_closed = self.target.close().is_ok();
        }
        if control
            .connection
            .as_ref()
            .is_some_and(|connection| connection.close().is_ok())
        {
            control.connection.take();
        }
        let released = control.target_closed && control.connection.is_none();
        control.snapshot["resources"] = json!(if released { "released" } else { "unknown" });
        self.released.store(released, Ordering::Release);
    }
}

fn execute_target(connection: &Connection, preview: &Preview) -> Result<Value> {
    let metadata::Invocation {
        sql,
        inputs,
        outputs,
    } = preview.invocation()?;
    let mut statement = connection.statement(&sql).build()?;
    for (name, value) in &inputs {
        statement.bind(name.as_str(), value)?;
    }
    for (name, _) in &outputs {
        statement.bind(name.as_str(), &OracleType::Varchar2(32767))?;
    }
    statement.execute(&[])?;
    let mut values = vec![];
    for (name, parameter) in outputs {
        let value: Option<String> = statement.bind_value(name.as_str())?;
        values.push(json!({"name":parameter.name,"data_type":parameter.data_type,"value":available(json!(value))}));
    }
    Ok(json!(values))
}

fn response(control: &mut Control) -> Value {
    let revision = control.snapshot["revision"].as_u64().unwrap_or(0) + 1;
    control.snapshot["revision"] = json!(revision);
    json!({"kind":"session","version":1,"session":control.snapshot})
}

fn stale(value: &mut Value) {
    if value.get("value").is_some() {
        value["state"] = json!("stale");
        value["reason"] = json!("Values belong to the previous stop");
    }
}

fn event(control: &mut Control, flags: Option<&str>) -> Result<()> {
    let connection = control
        .connection
        .as_ref()
        .ok_or_else(|| invalid("Native debugger resources have been released"))?;
    let info = "DBMS_DEBUG.INFO_GETSTACKDEPTH + DBMS_DEBUG.INFO_GETLINEINFO";
    let call = flags
        .map(|f| format!("DBMS_DEBUG.CONTINUE(r, {f}, {info})"))
        .unwrap_or_else(|| format!("DBMS_DEBUG.SYNCHRONIZE(r, {info})"));
    let sql = format!("DECLARE r DBMS_DEBUG.RUNTIME_INFO; n PLS_INTEGER; BEGIN n:={call}; :code:=n; :terminated:=NVL(r.terminated,0); :line:=NVL(r.line#,0); :owner:=r.program.owner; :name:=r.program.name; :timed_out:=CASE WHEN n=DBMS_DEBUG.ERROR_TIMEOUT THEN 1 ELSE 0 END; END;");
    let statement = connection.execute_named(
        &sql,
        &[
            ("code", &OracleType::Number(10, 0)),
            ("terminated", &OracleType::Number(10, 0)),
            ("line", &OracleType::Number(10, 0)),
            ("owner", &OracleType::Varchar2(128)),
            ("name", &OracleType::Varchar2(128)),
            ("timed_out", &OracleType::Number(10, 0)),
        ],
    )?;
    let code: i64 = statement.bind_value("code")?;
    if code != 0 {
        if statement.bind_value::<_, i64>("timed_out")? == 1 {
            control.snapshot["execution"] = json!("running");
            control.snapshot["stop"] = unavailable("Native target has not signalled another stop");
            return Ok(());
        }
        return Err(invalid(format!(
            "Native debugger event failed with status {code}"
        )));
    }
    if statement.bind_value::<_, i64>("terminated")? != 0 {
        control.snapshot["execution"] = json!("running");
        control.snapshot["stop"] = unavailable("Waiting for final execution/cleanup result");
        return Ok(());
    }
    let owner: Option<String> = statement.bind_value("owner")?;
    let name: Option<String> = statement.bind_value("name")?;
    let line: i64 = statement.bind_value("line")?;
    control.snapshot["execution"] = json!("paused");
    control.stop_serial += 1;
    control.snapshot["stack"] = unavailable("Read stack at this stop");
    control.snapshot["variables"] = unavailable("Read variables at this stop");
    let source = match (owner.as_deref(), name.as_deref()) {
        (Some(owner), Some(name)) => {
            load_source(connection, owner, name).unwrap_or_else(|e| unavailable(&e.message))
        }
        _ => unavailable("Anonymous wrapper; stepping to routine entry"),
    };
    control.snapshot["source"] = source;
    control.snapshot["stop"] =
        if let Some(source_id) = control.snapshot["source"]["value"]["source_id"].as_str() {
            available(json!({"source_id":source_id,"line":line.max(1)}))
        } else {
            unavailable("Current source location is not available")
        };
    Ok(())
}

fn load_source(connection: &Connection, owner: &str, name: &str) -> Result<Value> {
    use std::hash::{Hash, Hasher};
    let row = connection.query_row("SELECT object_id, TO_CHAR(last_ddl_time,'YYYYMMDDHH24MISS'), object_type FROM all_objects \
        WHERE owner=:1 AND object_name=:2 AND object_type IN ('PROCEDURE','FUNCTION','PACKAGE BODY','TYPE BODY','TRIGGER')", &[&owner,&name])?;
    let id: i64 = row.get(0)?;
    let modified: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let mut source = String::new();
    for row in connection.query(
        "SELECT text FROM all_source WHERE owner=:1 AND name=:2 AND type=:3 ORDER BY line",
        &[&owner, &name, &kind.as_str()],
    )? {
        source.push_str(&row?.get::<_, String>(0)?);
        if source.len() > 1_048_576 {
            return Err(invalid("Source exceeds debugger size limit"));
        }
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hash);
    Ok(available(
        json!({"source_id":format!("{id}:{modified}:{:x}",hash.finish()),"text":source}),
    ))
}

fn set_breakpoints(control: &mut Control, preview: &Preview, command: &Value) -> Result<()> {
    let requested = command["breakpoints"]
        .as_array()
        .ok_or_else(|| invalid("Missing breakpoint list"))?;
    if requested.len() > 128 {
        return Err(invalid("Too many breakpoints"));
    }
    let lines = preview.source["value"]["text"]
        .as_str()
        .map(|s| s.lines().count())
        .unwrap_or(0);
    for bp in requested {
        if bp["source_id"] != preview.source_id
            || !bp["line"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= lines as u64)
        {
            return Err(invalid(
                "Breakpoints must refer to an executable line of the previewed standalone routine",
            ));
        }
    }
    let connection = control
        .connection
        .as_ref()
        .ok_or_else(|| invalid("Debugger resources released"))?;
    let mut failures = vec![];
    let mut retained = vec![];
    for (id, bp) in control.breakpoints.drain(..) {
        let result = connection.execute(
            "BEGIN :code:=DBMS_DEBUG.DELETE_BREAKPOINT(:id); END;",
            &[&OracleType::Number(10, 0), &id],
        )?;
        if result.bind_value::<_, i64>("code")? != 0 {
            failures.push(format!("Could not remove breakpoint {id}"));
            retained.push((id, bp));
        }
    }
    control.breakpoints = retained;
    for bp in requested {
        let line = bp["line"].as_i64().unwrap();
        let result = connection.execute_named("DECLARE p DBMS_DEBUG.PROGRAM_INFO; BEGIN p.owner:=:owner; p.name:=:name; p.namespace:=DBMS_DEBUG.NAMESPACE_PKGSPEC_OR_TOPLEVEL; :code:=DBMS_DEBUG.SET_BREAKPOINT(p,:line,:bp); END;",
            &[("owner", &text(&preview.target,"schema")?), ("name", &text(&preview.target,"name")?), ("line", &line),
                ("code", &OracleType::Number(10,0)), ("bp", &OracleType::Number(10,0))])?;
        let code: i64 = result.bind_value("code")?;
        if code == 0 {
            control
                .breakpoints
                .push((result.bind_value("bp")?, bp.clone()));
        } else {
            failures.push(format!("Line {line}: native breakpoint status {code}"));
        }
    }
    let confirmed: Vec<_> = control
        .breakpoints
        .iter()
        .map(|(_, bp)| bp.clone())
        .collect();
    control.snapshot["breakpoints"] = if failures.is_empty() {
        available(json!(confirmed))
    } else {
        json!({"state":"partial","value":confirmed,"reason":failures.join("; ")})
    };
    Ok(())
}

fn read_stack(control: &mut Control) -> Result<()> {
    let connection = control
        .connection
        .as_ref()
        .ok_or_else(|| invalid("Debugger resources released"))?;
    let result = connection.execute("DECLARE t DBMS_DEBUG.BACKTRACE_TABLE; a JSON_ARRAY_T:=JSON_ARRAY_T(); o JSON_OBJECT_T; i BINARY_INTEGER; BEGIN DBMS_DEBUG.PRINT_BACKTRACE(t); i:=t.FIRST; WHILE i IS NOT NULL LOOP o:=JSON_OBJECT_T(); o.put('frame',i); o.put('name',t(i).owner||'.'||t(i).name); o.put('line',t(i).line#); a.append(o); i:=t.NEXT(i); END LOOP; :value:=a.to_string; END;", &[&OracleType::Varchar2(32767)])?;
    let frames: Value = serde_json::from_str(&result.bind_value::<_, String>("value")?)
        .map_err(|e| invalid(e.to_string()))?;
    let frames: Vec<_> = frames.as_array().ok_or_else(|| invalid("Invalid native stack"))?.iter().map(|frame| json!({
        "frame_id":format!("{}:{}",control.stop_serial,frame["frame"]),"name":frame["name"],
        "location":unavailable("Select the current source stop; other frame source locations are not loaded")
    })).collect();
    control.snapshot["stack"] = available(json!(frames));
    Ok(())
}

pub fn variable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_$#".contains(c))
}

fn read_variables(control: &mut Control, preview: &Preview, command: &Value) -> Result<()> {
    let frame = text(command, "frame_id")?;
    let (serial, frame) = frame
        .split_once(':')
        .ok_or_else(|| invalid("Invalid frame identity"))?;
    if serial.parse::<u64>().ok() != Some(control.stop_serial) {
        return Err(invalid("Frame belongs to a stale stop"));
    }
    let frame = frame
        .parse::<i64>()
        .map_err(|_| invalid("Invalid native frame"))?;
    if frame < 0 {
        return Err(invalid("Invalid native frame"));
    }
    let requested = command["names"]
        .as_array()
        .ok_or_else(|| invalid("Missing variable names"))?;
    let names: Vec<String> = if requested.is_empty() {
        preview
            .parameters
            .iter()
            .filter(|p| p.position > 0)
            .map(|p| p.name.clone())
            .collect()
    } else {
        requested
            .iter()
            .map(|name| {
                name.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| invalid("Variable name must be text"))
            })
            .collect::<Result<_>>()?
    };
    if names.len() > 128 || names.iter().any(|name| !variable_name(name)) {
        return Err(invalid(
            "Variable inspection only accepts scalar identifiers, never expressions or calls",
        ));
    }
    let connection = control
        .connection
        .as_ref()
        .ok_or_else(|| invalid("Debugger resources released"))?;
    let mut values = vec![];
    let mut partial = false;
    for name in names {
        let result = connection.execute_named("DECLARE n PLS_INTEGER; BEGIN n:=DBMS_DEBUG.GET_VALUE(:name,:frame,:value); :code:=n; :is_null:=CASE WHEN n=DBMS_DEBUG.ERROR_NULLVALUE THEN 1 ELSE 0 END; END;",
            &[("name", &name.to_uppercase()), ("frame", &frame), ("value", &OracleType::Varchar2(32767)), ("code", &OracleType::Number(10,0)), ("is_null", &OracleType::Number(10,0))])?;
        let code: i64 = result.bind_value("code")?;
        let value = if result.bind_value::<_, i64>("is_null")? == 1 {
            available(Value::Null)
        } else if code == 0 {
            available(json!(result.bind_value::<_, Option<String>>("value")?))
        } else {
            partial = true;
            unavailable(&format!("Native variable status {code}; missing debug information, unsupported type or variable"))
        };
        let data_type = preview
            .parameters
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(&name))
            .map(|p| p.data_type.as_str())
            .unwrap_or("PL/SQL scalar");
        values.push(json!({"name":name,"data_type":data_type,"value":value}));
    }
    control.snapshot["variables"] = if partial {
        json!({"state":"partial","value":values,"reason":"Some variables are unavailable"})
    } else {
        available(json!(values))
    };
    Ok(())
}
