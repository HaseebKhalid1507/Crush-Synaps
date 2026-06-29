//! crush binary — the Synaps process-extension entry point. All logic lives in
//! the `crush` library; this is the JSON-RPC stdin/stdout driver + the
//! `--unfold` decoder.

use crush::{handle_hook, protocol, transforms};
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
                    handle_hook(params.as_ref())
                }))
                .unwrap_or_else(|_| {
                    log("transform panicked — passing through");
                    serde_json::json!({ "action": "continue" })
                });
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

fn respond<W: Write>(writer: &mut W, id: &serde_json::Value, result: serde_json::Value) {
    if let Err(e) = protocol::write_response(writer, id, result) {
        log(&format!("write error: {e}"));
    }
}
