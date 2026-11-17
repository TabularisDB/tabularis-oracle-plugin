//! Entry point: read JSON-RPC lines from stdin, dispatch, write responses.
//!
//! One JSON-RPC request per line in, one response per line out. stdin is read
//! on the main thread so `cancel` and `routine_debug` (including Stop) are
//! dispatched immediately; ordinary requests are queued to one sequential
//! worker, which preserves their order but cannot block Stop when a query
//! waits on a row lock held by a paused debug target. Everything
//! that can fail is turned into a JSON-RPC error response rather than a panic,
//! so a single bad request never takes the plugin down.

// GUI subsystem on Windows: the host spawns us with piped stdio (which still
// works), and without this a console window pops up next to the app.
#![cfg_attr(windows, windows_subsystem = "windows")]

use oracle_plugin::{debugger, rpc};
use serde_json::Value;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
static OUTPUT_FAILED: AtomicBool = AtomicBool::new(false);

fn write_response(out: &Arc<Mutex<io::Stdout>>, response: Value) -> bool {
    let success = {
        let mut out = out.lock().unwrap();
        writeln!(out, "{response}")
            .and_then(|_| out.flush())
            .is_ok()
    };
    if !success {
        OUTPUT_FAILED.store(true, Ordering::Release);
        debugger::shutdown();
    }
    success
}

fn main() {
    let stdin = io::stdin();
    let out = Arc::new(Mutex::new(io::stdout()));
    let (queue, ordinary) = mpsc::channel::<String>();
    let worker = {
        let out = out.clone();
        std::thread::spawn(move || {
            for line in ordinary {
                if OUTPUT_FAILED.load(Ordering::Acquire)
                    || !write_response(&out, rpc::handle_line(&line))
                {
                    break;
                }
            }
        })
    };

    for line in stdin.lock().lines() {
        if OUTPUT_FAILED.load(Ordering::Acquire) {
            break;
        }
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request = serde_json::from_str::<Value>(trimmed).ok();
        let method = request.as_ref().and_then(|v| v["method"].as_str());
        // Host timeout/abort sends a JSON-RPC notification `cancel` (no top-level
        // id). `$/cancelRequest` is accepted as an alias. Do not reply.
        if matches!(method, Some("cancel") | Some("$/cancelRequest")) {
            debugger::cancel_request(&request.unwrap()["params"]["id"]);
        } else if method == Some("routine_debug") {
            let request = request.unwrap();
            let id = request["id"].clone();
            match debugger::begin_request(&id, &request["params"]) {
                Ok(ticket) => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        let _ticket = ticket;
                        write_response(&out, rpc::handle_line(&line));
                    });
                }
                Err(error) => {
                    if !write_response(&out, rpc::error_response(id, error.code, &error.message)) {
                        break;
                    }
                }
            }
        } else if queue.send(trimmed.to_string()).is_err() {
            break;
        }
    }
    // Release debug targets first so a queued query blocked on their locks can
    // finish, then drain the ordinary queue.
    drop(queue);
    debugger::shutdown();
    let _ = worker.join();
}
