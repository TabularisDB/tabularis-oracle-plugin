//! Real Oracle evidence; CI supplies a disposable Oracle Free service and SYSTEM.
//! All grants/DDL below are explicit test fixture setup, never debugger behaviour.
use oracle::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Plugin {
    child: Child,
    input: Option<ChildStdin>,
    responses: mpsc::Receiver<Value>,
    pending: HashMap<u64, Value>,
    next: u64,
    params: Value,
}

impl Plugin {
    fn new(params: Value) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_oracle-plugin"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str(&line) {
                    if sender.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let input = child.stdin.take();
        let mut plugin = Self {
            child,
            input,
            responses,
            pending: HashMap::new(),
            next: 0,
            params,
        };
        let settings = std::env::var("ORACLE_CLIENT_LIB_DIR")
            .map(|dir| json!({"client_lib_dir":dir}))
            .unwrap_or(json!({}));
        let id = plugin.send("initialize", json!({"settings":settings}));
        assert!(plugin.reply(id).get("error").is_none());
        plugin
    }
    fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next += 1;
        let request = json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params});
        writeln!(self.input.as_mut().unwrap(), "{request}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        self.next
    }
    fn reply(&mut self, id: u64) -> Value {
        if let Some(value) = self.pending.remove(&id) {
            return value;
        }
        loop {
            let response = self
                .responses
                .recv_timeout(Duration::from_secs(15))
                .expect("native RPC must remain bounded/responsive");
            let response_id = response["id"].as_u64().expect("response identity");
            if response_id == id {
                return response;
            }
            self.pending.insert(response_id, response);
        }
    }
    fn debug_raw(&mut self, command: Value) -> Value {
        let id = self.send(
            "routine_debug",
            json!({"params":self.params,"version":1,"command":command}),
        );
        self.reply(id)
    }
    fn debug(&mut self, command: Value) -> Value {
        let response = self.debug_raw(command);
        assert!(
            response.get("error").is_none(),
            "native debugger error: {response}"
        );
        response["result"].clone()
    }
    fn start(&mut self, name: &str, id: &str, arguments: Value) -> Value {
        let target = json!({"database":self.params["database"],"schema":self.params["username"],"name":name,"routine_type":if name=="DBG_FUNC" {"FUNCTION"} else {"PROCEDURE"},"identity_args":""});
        let preview = self.debug(json!({"action":"preview","target":target,"arguments":arguments}));
        assert_eq!(preview["kind"], "preview");
        assert_eq!(preview["source"]["state"], "available");
        let mut result = self
            .debug(json!({"action":"start","preview_id":preview["preview_id"],"session_id":id}));
        for _ in 0..20 {
            if result["session"]["execution"] != "running" {
                break;
            }
            result = self.debug(json!({"action":"status","session_id":id}));
        }
        assert_eq!(
            result["session"]["execution"], "paused",
            "start must stop at the selected entry: {result}"
        );
        assert_eq!(result["session"]["source"], preview["source"]);
        let duplicate = self
            .debug(json!({"action":"start","preview_id":preview["preview_id"],"session_id":id}));
        assert_eq!(
            duplicate["session"]["execution"], "paused",
            "duplicate start must never resume/rerun"
        );
        result
    }
    fn complete(&mut self, id: &str) -> Value {
        let mut result = self.debug(json!({"action":"status","session_id":id}));
        for _ in 0..40 {
            let state = result["session"]["execution"].as_str().unwrap();
            if matches!(state, "completed" | "failed" | "cancelled")
                && result["session"]["resources"] == "released"
            {
                return result;
            }
            let action = if state == "paused" {
                "continue"
            } else {
                "status"
            };
            result = self.debug(json!({"action":action,"session_id":id}));
        }
        panic!("execution/cleanup did not settle: {result}");
    }
    fn eof(&mut self) {
        self.input.take();
        let end = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < end,
                "EOF must stop/release native sessions and exit"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
impl Drop for Plugin {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    admin: Connection,
    connection: Connection,
    user: String,
    params: Value,
}
impl Fixture {
    fn new() -> Option<Self> {
        let host = std::env::var("ORACLE_TEST_HOST").ok()?;
        if let Ok(dir) = std::env::var("ORACLE_CLIENT_LIB_DIR") {
            oracle_plugin::settings::apply(&json!({"client_lib_dir":dir}));
        }
        let env =
            |key: &str, fallback: &str| std::env::var(key).unwrap_or_else(|_| fallback.into());
        let service = env("ORACLE_TEST_SERVICE", "FREEPDB1");
        let port = env("ORACLE_TEST_PORT", "1521");
        let connect = format!("{host}:{port}/{service}");
        let admin = Connection::connect(
            env("ORACLE_TEST_USER", "system"),
            env("ORACLE_TEST_PASSWORD", "oracle"),
            &connect,
        )
        .unwrap();
        let version: String = admin
            .query_row_as("SELECT banner FROM v$version WHERE rownum=1", &[])
            .unwrap();
        eprintln!("Native debugger integration against {version}");
        eprintln!(
            "Oracle Instant Client {}",
            oracle::Version::client().unwrap()
        );
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let user = format!("TDBG_{}_{}", std::process::id(), suffix);
        admin.execute(&format!("CREATE USER {user} IDENTIFIED BY TestOnly123 DEFAULT TABLESPACE USERS QUOTA 1M ON USERS"),&[]).unwrap();
        admin.execute(&format!("GRANT CREATE SESSION, CREATE PROCEDURE, CREATE TABLE, DEBUG CONNECT SESSION TO {user}"),&[]).unwrap();
        let connection = Connection::connect(&user, "TestOnly123", &connect).unwrap();
        connection
            .execute("CREATE TABLE DEBUG_EFFECTS(n NUMBER)", &[])
            .unwrap();
        connection.execute("CREATE OR REPLACE PROCEDURE DBG_SAMPLE(P_IN IN NUMBER,P_OUT OUT NUMBER) AS\nL_VALUE NUMBER;\nBEGIN\nL_VALUE := P_IN + 1;\nINSERT INTO DEBUG_EFFECTS VALUES(L_VALUE);\nP_OUT := L_VALUE;\nEND;",&[]).unwrap();
        connection.execute("CREATE OR REPLACE PROCEDURE DBG_FAIL AS\nBEGIN\nINSERT INTO DEBUG_EFFECTS VALUES(999);\nRAISE_APPLICATION_ERROR(-20001,'expected debug exception');\nEND;",&[]).unwrap();
        connection.execute("CREATE OR REPLACE FUNCTION DBG_FUNC(P_IN IN NUMBER) RETURN NUMBER AS\nBEGIN\nRETURN P_IN + 2;\nEND;",&[]).unwrap();
        connection
            .execute(
                "CREATE OR REPLACE PROCEDURE DBG_SPIN AS\nBEGIN\nLOOP NULL; END LOOP;\nEND;",
                &[],
            )
            .unwrap();
        connection
            .execute(
                "CREATE OR REPLACE PROCEDURE DBG_NODEBUG AS BEGIN NULL; END;",
                &[],
            )
            .unwrap();
        for (name, kind) in [
            ("DBG_SAMPLE", "PROCEDURE"),
            ("DBG_FAIL", "PROCEDURE"),
            ("DBG_FUNC", "FUNCTION"),
            ("DBG_SPIN", "PROCEDURE"),
        ] {
            connection
                .execute(&format!("ALTER {kind} {name} COMPILE DEBUG"), &[])
                .unwrap();
        }
        let params = json!({"host":host,"port":port.parse::<u16>().unwrap(),"database":service,"username":user,"password":"TestOnly123"});
        Some(Self {
            admin,
            connection,
            user,
            params,
        })
    }
    fn no_sessions(&self, id: &str) {
        let pattern = format!("tabularis-debug:{id}%");
        for _ in 0..30 {
            let count: i64 = self
                .admin
                .query_row_as(
                    "SELECT COUNT(*) FROM v$session WHERE client_identifier LIKE :1",
                    &[&pattern],
                )
                .unwrap();
            let owned: i64 = self
                .admin
                .query_row_as(
                    "SELECT COUNT(*) FROM v$session WHERE username=:1",
                    &[&self.user],
                )
                .unwrap();
            if count == 0 && owned == 1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("native target/controller resources leaked for {id}");
    }
    fn unchanged(&self) {
        let count: i64 = self
            .connection
            .query_row_as("SELECT COUNT(*) FROM DEBUG_EFFECTS", &[])
            .unwrap();
        assert_eq!(count,0,"ordinary uncommitted target effects must be rolled back, never committed by debugger cleanup");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.connection.close();
        let _ = self
            .admin
            .execute(&format!("DROP USER {} CASCADE", self.user), &[]);
    }
}

#[test]
fn real_debugger_breakpoints_values_exceptions_and_cleanup() {
    let Some(fixture) = Fixture::new() else {
        eprintln!("SKIPPED native Oracle evidence: ORACLE_TEST_HOST is unset");
        return;
    };
    let mut plugin = Plugin::new(fixture.params.clone());
    let id = "normal";
    let start = plugin.start("DBG_SAMPLE", id, json!([{"position":1,"value":"41"}]));
    assert!(
        start["session"]["stop"]["value"]["line"]
            .as_u64()
            .is_some_and(|line| (1..=4).contains(&line)),
        "initial stop must precede the routine's INSERT: {start}"
    );
    let source = start["session"]["source"]["value"]["source_id"].clone();
    let bp=plugin.debug(json!({"action":"set_breakpoints","session_id":id,"breakpoints":[{"source_id":source,"line":6}]}));
    assert_eq!(bp["session"]["breakpoints"]["state"], "available");
    assert_eq!(
        bp["session"]["breakpoints"]["value"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let stopped = plugin.debug(json!({"action":"continue","session_id":id}));
    assert_eq!(stopped["session"]["stop"]["value"]["line"], 6);
    let stack = plugin.debug(json!({"action":"read_stack","session_id":id}));
    let frames = stack["session"]["stack"]["value"].as_array().unwrap();
    let frame = frames.last().expect("current native frame")["frame_id"].clone();
    let variables = plugin.debug(
        json!({"action":"read_variables","session_id":id,"frame_id":frame,"names":["L_VALUE"]}),
    );
    assert_eq!(
        variables["session"]["variables"]["value"][0]["value"]["value"]
            .as_str()
            .unwrap()
            .trim(),
        "42"
    );
    let partial=plugin.debug(json!({"action":"read_variables","session_id":id,"frame_id":frame,"names":["MISSING_VALUE"]}));
    assert_eq!(partial["session"]["variables"]["state"], "partial");
    assert!(plugin
        .debug_raw(
            json!({"action":"read_variables","session_id":id,"frame_id":frame,"names":["F()"]})
        )
        .get("error")
        .is_some());
    plugin.debug(json!({"action":"step","session_id":id,"mode":"over"}));
    let completed = plugin.complete(id);
    assert_eq!(completed["session"]["execution"], "completed");
    assert_eq!(completed["session"]["transaction"], "unknown");
    assert_eq!(
        completed["session"]["outputs"]["value"][0]["value"]["value"],
        "42"
    );
    fixture.no_sessions(id);
    fixture.unchanged();
    let repeated = plugin.debug(json!({"action":"stop","session_id":id}));
    assert_eq!(repeated["session"]["resources"], "released");

    plugin.start("DBG_FUNC", "function", json!([{"position":1,"value":"40"}]));
    plugin.debug(json!({"action":"step","session_id":"function","mode":"into"}));
    plugin.debug(json!({"action":"step","session_id":"function","mode":"out"}));
    let result = plugin.complete("function");
    assert_eq!(result["session"]["outputs"]["value"][0]["name"], "RETURN");
    assert_eq!(
        result["session"]["outputs"]["value"][0]["value"]["value"],
        "42"
    );
    fixture.no_sessions("function");

    plugin.start("DBG_FAIL", "exception", json!([]));
    let failure = plugin.complete("exception");
    assert_eq!(failure["session"]["execution"], "failed");
    assert!(failure["session"]["error"]
        .as_str()
        .unwrap()
        .contains("20001"));
    fixture.no_sessions("exception");
    fixture.unchanged();

    plugin.start("DBG_SAMPLE", "stop", json!([{"position":1,"value":"41"}]));
    let stopped = plugin.debug(json!({"action":"stop","session_id":"stop"}));
    assert_eq!(stopped["session"]["execution"], "cancelled");
    assert_eq!(stopped["session"]["resources"], "released");
    fixture.no_sessions("stop");
    fixture.unchanged();

    plugin.start("DBG_SPIN", "cancel", json!([]));
    let run=plugin.send("routine_debug",json!({"params":plugin.params,"version":1,"command":{"action":"continue","session_id":"cancel"}}));
    let stop=plugin.send("routine_debug",json!({"params":plugin.params,"version":1,"command":{"action":"stop","session_id":"cancel"}}));
    let cancelled = plugin.reply(stop);
    assert_eq!(
        cancelled["result"]["session"]["resources"], "released",
        "stop must interrupt an in-flight continue: {cancelled}"
    );
    let _ = plugin.reply(run);
    fixture.no_sessions("cancel");

    let preview=plugin.debug_raw(json!({"action":"preview","target":{"database":fixture.params["database"],"schema":fixture.user,"name":"DBG_NODEBUG","routine_type":"PROCEDURE","identity_args":""},"arguments":[]}));
    assert!(preview["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no debug information"));
    let debug: String = fixture
        .connection
        .query_row_as(
            "SELECT plsql_debug FROM user_plsql_object_settings WHERE name='DBG_NODEBUG'",
            &[],
        )
        .unwrap();
    assert_eq!(debug, "FALSE", "preview must not recompile routines");

    fixture
        .connection
        .execute(
            "CREATE OR REPLACE PROCEDURE DBG_UNSUPPORTED(P_DATE IN DATE) AS BEGIN NULL; END;",
            &[],
        )
        .unwrap();
    fixture
        .connection
        .execute("ALTER PROCEDURE DBG_UNSUPPORTED COMPILE DEBUG", &[])
        .unwrap();
    let unsupported=plugin.debug_raw(json!({"action":"preview","target":{"database":fixture.params["database"],"schema":fixture.user,"name":"DBG_UNSUPPORTED","routine_type":"PROCEDURE","identity_args":""},"arguments":[{"position":1,"value":"2026-01-01"}]}));
    assert!(unsupported["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Unsupported debugger parameter type"));

    let preview=plugin.debug(json!({"action":"preview","target":{"database":fixture.params["database"],"schema":fixture.user,"name":"DBG_SAMPLE","routine_type":"PROCEDURE","identity_args":""},"arguments":[{"position":1,"value":"41"}]}));
    fixture.connection.execute("CREATE OR REPLACE PROCEDURE DBG_SAMPLE(P_IN IN NUMBER,P_OUT OUT NUMBER) AS\nL_VALUE NUMBER;\nBEGIN\nL_VALUE := P_IN + 10;\nINSERT INTO DEBUG_EFFECTS VALUES(L_VALUE);\nP_OUT := L_VALUE;\nEND;",&[]).unwrap();
    fixture
        .connection
        .execute("ALTER PROCEDURE DBG_SAMPLE COMPILE DEBUG", &[])
        .unwrap();
    let changed = plugin.debug_raw(
        json!({"action":"start","preview_id":preview["preview_id"],"session_id":"changed"}),
    );
    assert!(changed["error"]["message"]
        .as_str()
        .unwrap()
        .contains("changed after preview"));
    fixture.no_sessions("changed");
    fixture.unchanged();

    fixture
        .admin
        .execute(
            &format!("REVOKE DEBUG CONNECT SESSION FROM {}", fixture.user),
            &[],
        )
        .unwrap();
    let preview=plugin.debug(json!({"action":"preview","target":{"database":fixture.params["database"],"schema":fixture.user,"name":"DBG_SAMPLE","routine_type":"PROCEDURE","identity_args":""},"arguments":[{"position":1,"value":"41"}]}));
    let denied = plugin.debug_raw(
        json!({"action":"start","preview_id":preview["preview_id"],"session_id":"denied"}),
    );
    assert!(
        denied.get("error").is_some(),
        "missing privileges must never be granted by the debugger"
    );
    let grants:i64=fixture.admin.query_row_as("SELECT COUNT(*) FROM dba_sys_privs WHERE grantee=:1 AND privilege='DEBUG CONNECT SESSION'",&[&fixture.user]).unwrap();
    assert_eq!(grants, 0);
    fixture.no_sessions("denied");
    fixture.unchanged();
    fixture
        .admin
        .execute(
            &format!("GRANT DEBUG CONNECT SESSION TO {}", fixture.user),
            &[],
        )
        .unwrap();

    plugin.start("DBG_SAMPLE", "eof", json!([{"position":1,"value":"41"}]));
    plugin.eof();
    fixture.no_sessions("eof");
    fixture.unchanged();
    eprintln!("PASS: routine, exception, scalar OUT/return, all step modes, native breakpoints/variables, stop, concurrent cancellation, EOF, rollback and target/controller cleanup");
}
