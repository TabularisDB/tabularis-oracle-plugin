//! Stdio JSON-RPC protocol checks that do not need a live Oracle instance.
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn host_cancel_notification_does_not_emit_a_response() {
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_oracle-plugin"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut input = process.0.stdin.take().unwrap();
    let output = process.0.stdout.take().unwrap();
    let (responses, received) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            if responses.send(line.unwrap()).is_err() {
                break;
            }
        }
    });

    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})
    )
    .unwrap();
    input.flush().unwrap();
    let first: Value = serde_json::from_str(
        &received
            .recv_timeout(Duration::from_secs(5))
            .expect("initialize must respond"),
    )
    .unwrap();
    assert_eq!(first, json!({"jsonrpc":"2.0","id":1,"result":null}));

    // Exact notification written by the host's cancel_notification_line.
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"cancel","params":{"id":123}})
    )
    .unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":124}})
    )
    .unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}})
    )
    .unwrap();
    drop(input);

    let second: Value =
        serde_json::from_str(&received.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(second, json!({"jsonrpc":"2.0","id":2,"result":null}));
    process.0.wait().unwrap();
    reader.join().unwrap();
    assert!(
        received.try_recv().is_err(),
        "cancel notifications must not emit responses"
    );
}
