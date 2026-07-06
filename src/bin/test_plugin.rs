//! Local REPL that drives the real JSON-RPC dispatch over stdio.
//!
//! Usage: `just repl` (or `cargo run --bin test_plugin`).
//!
//! Two input modes:
//!   * A full JSON-RPC object (e.g. `{"method":"get_tables","params":{...}}`)
//!     is forwarded verbatim to the dispatcher.
//!   * A bare method name (e.g. `get_schemas`) is wrapped in a stub request,
//!     handy for quickly hitting static methods.
//!
//! Type `exit` / `quit` or press Ctrl-D to leave.

use std::io::{self, BufRead, Write};

use oracle_plugin::rpc::handle_line;
use serde_json::json;

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    println!("test_plugin — enter a JSON-RPC request or a bare method name. `exit` to quit.");

    let mut next_id: u64 = 1;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let cmd = line.trim();
        if cmd.is_empty() {
            continue;
        }
        if cmd == "exit" || cmd == "quit" {
            break;
        }

        let request_line = if cmd.starts_with('{') {
            cmd.to_string()
        } else {
            let request = json!({
                "jsonrpc": "2.0",
                "method": cmd,
                "params": { "params": {}, "schema": null, "query": "" },
                "id": next_id,
            });
            next_id += 1;
            request.to_string()
        };

        let response = handle_line(&request_line);
        let pretty =
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| response.to_string());
        writeln!(out, "{pretty}").ok();
        out.flush().ok();
    }
}
