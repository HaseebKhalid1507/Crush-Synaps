//! crush — native tool-output compressor for Synaps.
//!
//! A process extension that subscribes to `after_tool_call` and rewrites large
//! tool output through a transform pipeline before it enters conversation
//! history. Speaks the Content-Length-framed JSON-RPC v1 protocol over
//! stdin/stdout.
//!
//! Fail-safe by construction: any output below the size floor, any transform
//! that doesn't help, or any internal error all degrade to `continue` — the
//! original tool output is preserved. A compression layer must never break or
//! drop a tool's output.

mod compress;
mod protocol;
mod transforms;

use std::io::{self, BufReader, Write};

const PROTOCOL_VERSION: u64 = 1;

fn log(msg: &str) {
    let _ = writeln!(io::stderr(), "[crush] {msg}");
}

fn main() {
    log("started");
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        let msg = match protocol::read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => break, // clean EOF
            Err(e) => {
                log(&format!("frame error: {e}"));
                break;
            }
        };

        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

        match method {
            "initialize" => {
                let result = serde_json::json!({
                    "protocol_version": PROTOCOL_VERSION,
                    "capabilities": {},
                });
                respond(&mut writer, &id, result);
            }
            "hook.handle" => {
                let result = handle_hook(msg.get("params"));
                respond(&mut writer, &id, result);
            }
            "shutdown" => {
                respond(&mut writer, &id, serde_json::json!({}));
                break;
            }
            // info.get and any future method: respond benign so the engine
            // never marks us degraded over an unimplemented optional call.
            _ => respond(&mut writer, &id, serde_json::json!({})),
        }
    }
    log("stopped");
}

/// Handle an `after_tool_call` hook event. Returns the JSON result body:
/// `{"action":"replace","output":...}` on a win, else `{"action":"continue"}`.
fn handle_hook(params: Option<&serde_json::Value>) -> serde_json::Value {
    let cont = serde_json::json!({ "action": "continue" });
    let Some(params) = params else { return cont };

    let kind = params.get("kind").and_then(|k| k.as_str()).unwrap_or("");
    if kind != "after_tool_call" {
        return cont;
    }

    let output = match params.get("tool_output").and_then(|o| o.as_str()) {
        Some(o) => o,
        None => return cont,
    };
    let empty = serde_json::Value::Null;
    let tool_input = params.get("tool_input").unwrap_or(&empty);
    let tool_name = params
        .get("tool_name")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    match compress::compress(tool_name, tool_input, output) {
        Some(compressed) => serde_json::json!({
            "action": "replace",
            "output": compressed,
        }),
        None => cont,
    }
}

fn respond<W: Write>(writer: &mut W, id: &serde_json::Value, result: serde_json::Value) {
    if let Err(e) = protocol::write_response(writer, id, result) {
        log(&format!("write error: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_after_tool_call_events_pass_through() {
        let params = serde_json::json!({ "kind": "before_message", "message": "hi" });
        assert_eq!(handle_hook(Some(&params))["action"], "continue");
    }

    #[test]
    fn missing_output_passes_through() {
        let params = serde_json::json!({ "kind": "after_tool_call" });
        assert_eq!(handle_hook(Some(&params))["action"], "continue");
    }

    #[test]
    fn incompressible_output_passes_through() {
        let mut big = String::new();
        for i in 0..300 {
            big.push_str(&format!("the quick brown fox {i} jumps over a lazy dog\n"));
        }
        let params = serde_json::json!({
            "kind": "after_tool_call",
            "tool_input": { "command": "echo hi" },
            "tool_output": big,
        });
        assert_eq!(handle_hook(Some(&params))["action"], "continue");
    }
}
