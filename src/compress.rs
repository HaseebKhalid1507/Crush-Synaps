//! The compression pipeline.
//!
//! `compress(tool_name, tool_input, output)` returns `Some(compressed)` when a
//! transform meaningfully shrinks the output, or `None` to pass through
//! unchanged. The `tool_name` (and `command`, sniffed from `tool_input`) enable
//! tool-aware transforms; the size floor and savings gate keep pointless
//! rewrites out of the conversation.
//!
//! Safety invariant: never panic, never return a string larger than the input.

use serde_json::Value;

/// Don't bother compressing anything smaller than this.
pub const MIN_RAW_BYTES: usize = 2048;

/// Only emit a Replace when we save at least this many bytes.
pub const MIN_BYTES_SAVED: usize = 256;

/// Run the pipeline. Returns the compressed string (with a one-line provenance
/// header) only when it is a genuine, meaningful win; otherwise `None`.
pub fn compress(tool_name: &str, tool_input: &Value, output: &str) -> Option<String> {
    if output.len() < MIN_RAW_BYTES {
        return None;
    }
    let command = tool_input
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let best = crate::transforms::run(tool_name, command, output)?;

    let saved = output.len().saturating_sub(best.len());
    if saved < MIN_BYTES_SAVED {
        return None;
    }
    Some(with_header(output.len(), best.len(), &best))
}

/// Prepend a single-line provenance header so the model (and humans reading the
/// transcript) know the output was rewritten and by how much.
fn with_header(before: usize, after: usize, body: &str) -> String {
    let pct = before.saturating_sub(after).saturating_mul(100) / before.max(1);
    format!("[crush: {before}→{after} bytes (-{pct}%)]\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn passes_through_output_below_size_floor() {
        let small = "x".repeat(MIN_RAW_BYTES - 1);
        assert!(compress("bash", &json!({"command": "echo"}), &small).is_none());
    }

    #[test]
    fn passes_through_when_no_transform_helps() {
        let mut big = String::new();
        for i in 0..300 {
            big.push_str(&format!("the quick brown fox {i} jumps over a lazy dog\n"));
        }
        assert!(compress("bash", &json!({"command": "echo"}), &big).is_none());
    }

    #[test]
    fn compresses_a_json_array_over_the_floor() {
        let mut arr = String::from("[");
        for i in 0..200 {
            if i > 0 {
                arr.push(',');
            }
            arr.push_str(&format!(
                r#"{{"name":"item{i}","value":{i},"active":true}}"#
            ));
        }
        arr.push(']');
        let out = compress("bash", &json!({"command": "echo"}), &arr).expect("should compress");
        assert!(out.starts_with("[crush:"));
        assert!(out.len() < arr.len());
    }
}
