//! crush — native tool-output compressor for Synaps.
//!
//! Library surface: the transform pipeline, the wire-format markers, the
//! JSON-RPC protocol codec, and [`handle_hook`]. Exposed for the `crush` binary,
//! unit/integration tests, and the fuzz targets.
//!
//! Fail-safe by construction: any output below the size floor, any transform
//! that doesn't help, or any internal error all degrade to `continue` — the
//! original tool output is preserved. A compression layer must never break or
//! drop a tool's output.

pub mod compress;
pub mod markers;
pub mod protocol;
pub mod stats;
pub mod transforms;

use serde_json::Value;

use crate::stats::Stats;

/// Handle an `after_tool_call` hook event. Returns the JSON result body:
/// `{"action":"replace","output":...}` on a win, else `{"action":"continue"}`.
#[must_use]
pub fn handle_hook(params: Option<&Value>) -> Value {
    handle_hook_with_stats(params, &mut Stats::new())
}

/// Same as [`handle_hook`], but records every compression win to `stats`.
#[must_use]
pub fn handle_hook_with_stats(params: Option<&Value>, stats: &mut Stats) -> Value {
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
    let empty = Value::Null;
    let tool_input = params.get("tool_input").unwrap_or(&empty);
    let tool_name = params.get("tool_name").and_then(|t| t.as_str()).unwrap_or("");

    match compress::compress_labeled(tool_name, tool_input, output) {
        Some((compressed, label)) => {
            stats.record(label, output.len(), compressed.len());
            serde_json::json!({
                "action": "replace",
                "output": compressed,
            })
        }
        None => cont,
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
