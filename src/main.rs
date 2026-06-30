//! crush binary — the Synaps process-extension entry point. All logic lives in
//! the `crush` library; this is the JSON-RPC stdin/stdout driver + the
//! `--unfold` decoder.

use crush::{handle_hook_with_stats, protocol, stats::Stats, transforms};
use std::io::{self, BufReader, Write};

const PROTOCOL_VERSION: u64 = 1;

fn log(msg: &str) {
    let _ = writeln!(io::stderr(), "[crush] {msg}");
}

fn main() {
    // `crush --unfold`: read a columnar-folded block on stdin, reconstruct the
    // original rows on stdout. The executable proof of the lossless contract.
    if std::env::args().any(|a| a == "--unfold") {
        let mut input = String::new();
        if io::Read::read_to_string(&mut io::stdin(), &mut input).is_err() {
            std::process::exit(2);
        }
        match transforms::unfold_columnar(&input) {
            Some(s) => {
                let _ = io::stdout().write_all(s.as_bytes());
            }
            None => {
                let _ = writeln!(io::stderr(), "[crush] not a columnar-folded block");
                std::process::exit(1);
            }
        }
        return;
    }

    log("started");
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut stats = Stats::new();

    loop {
        let msg = match protocol::read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => break, // clean EOF
            Err(e) => {
                // A malformed/oversized frame corrupts at most one message.
                // read_message is stateless between calls, so resync on the next
                // frame rather than killing a long-lived extension.
                log(&format!("frame error: {e} — resyncing"));
                continue;
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
                // Panic firewall: a bug in any transform must degrade to
                // pass-through, never crash the long-lived extension. (Release
                // profile is panic=unwind precisely so this catches.)
                let params = msg.get("params").cloned();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle_hook_with_stats(params.as_ref(), &mut stats)
                }))
                .unwrap_or_else(|_| {
                    log("transform panicked — passing through");
                    serde_json::json!({ "action": "continue" })
                });
                respond(&mut writer, &id, result);
            }
            "command.invoke" => {
                // Slash command dispatched by Synaps. We stream the report
                // back as a `command.output` text notification, then `done`,
                // then return the JSON-RPC response value (an empty object —
                // the runtime carries no contract for the response body once
                // events have been streamed).
                let params = msg.get("params").cloned().unwrap_or(serde_json::Value::Null);
                let response = handle_command_invoke(&mut writer, &params, &stats);
                respond(&mut writer, &id, response);
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

/// Render `/crush`-style commands. Streams `command.output` notifications
/// (text + done) so Synaps renders the report into chat, then returns the
/// final JSON-RPC response object.
fn handle_command_invoke<W: Write>(
    writer: &mut W,
    params: &serde_json::Value,
    stats: &Stats,
) -> serde_json::Value {
    let command = params.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let request_id = params
        .get("request_id")
        .and_then(|r| r.as_str())
        .unwrap_or("");

    let body = match command {
        // The only command this extension registers. We also accept the
        // bare slash name for robustness.
        "crush" | "/crush" | "" => stats.render(),
        other => format!("crush: unknown command '{other}'"),
    };

    emit_system(writer, request_id, &body);
    emit_done(writer, request_id);

    serde_json::json!({ "ok": true })
}

fn emit_system<W: Write>(writer: &mut W, request_id: &str, content: &str) {
    let params = serde_json::json!({
        "request_id": request_id,
        "event": { "kind": "system", "content": content },
    });
    if let Err(e) = protocol::write_notification(writer, "command.output", params) {
        log(&format!("notification write error: {e}"));
    }
}

fn emit_done<W: Write>(writer: &mut W, request_id: &str) {
    let params = serde_json::json!({
        "request_id": request_id,
        "event": { "kind": "done" },
    });
    if let Err(e) = protocol::write_notification(writer, "command.output", params) {
        log(&format!("notification write error: {e}"));
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
    use crush::protocol::read_message;
    use std::io::Cursor;

    fn drain_notifications(buf: &[u8]) -> Vec<serde_json::Value> {
        let mut cursor = Cursor::new(buf.to_vec());
        let mut out = Vec::new();
        while let Ok(Some(v)) = read_message(&mut cursor) {
            out.push(v);
        }
        out
    }

    #[test]
    fn command_invoke_crush_streams_report_and_done() {
        let mut stats = Stats::new();
        stats.record("ls", 2000, 1000);
        let mut buf: Vec<u8> = Vec::new();
        let params = serde_json::json!({
            "command": "crush",
            "args": [],
            "request_id": "req-42",
        });
        let resp = handle_command_invoke(&mut buf, &params, &stats);
        assert_eq!(resp["ok"], true);

        let frames = drain_notifications(&buf);
        assert_eq!(frames.len(), 2, "expected system + done, got: {frames:?}");
        assert_eq!(frames[0]["method"], "command.output");
        assert_eq!(frames[0]["params"]["request_id"], "req-42");
        assert_eq!(frames[0]["params"]["event"]["kind"], "system");
        let text = frames[0]["params"]["event"]["content"].as_str().unwrap();
        assert!(text.contains("crush"), "report should mention crush: {text}");
        assert!(text.contains("ls"), "report should list the ls bucket: {text}");

        assert_eq!(frames[1]["params"]["event"]["kind"], "done");
    }

    #[test]
    fn command_invoke_with_no_stats_reports_empty() {
        let stats = Stats::new();
        let mut buf: Vec<u8> = Vec::new();
        let params = serde_json::json!({
            "command": "crush",
            "args": [],
            "request_id": "r1",
        });
        let _ = handle_command_invoke(&mut buf, &params, &stats);
        let frames = drain_notifications(&buf);
        let text = frames[0]["params"]["event"]["content"].as_str().unwrap();
        assert!(text.contains("no compressions yet"), "got: {text}");
    }
}
