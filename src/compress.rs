//! The compression pipeline.
//!
//! `compress(tool_name, tool_input, output)` returns `Some(compressed)` when a
//! transform meaningfully shrinks the output, or `None` to pass through
//! unchanged. The `tool_name` (and `command`, sniffed from `tool_input`) enable
//! tool-aware transforms; the size floor and savings gate keep pointless
//! rewrites out of the conversation.
//!
//! Safety invariants (enforced, not aspirational):
//! - Never return a string larger than the input — the savings gate accounts for
//!   the provenance header, so "never enlarge" holds by construction.
//! - Never double-compress: if the input already contains crush's own markers we
//!   bail to pass-through (prevents marker-injection corruption + idempotency
//!   breakage).
//! - Never panic — the caller ([`crate::handle_hook`]) wraps this in
//!   `catch_unwind` as a final firewall.

use serde_json::Value;

/// Don't bother compressing anything smaller than this.
pub const MIN_RAW_BYTES: usize = 2048;

/// Only emit a Replace when the FINAL output (header included) is at least this
/// many bytes smaller than the input.
pub const MIN_BYTES_SAVED: usize = 256;

/// Run the pipeline. Returns the compressed string (with a one-line provenance
/// header) only when it is a genuine, meaningful win; otherwise `None`.
#[must_use]
pub fn compress(tool_name: &str, tool_input: &Value, output: &str) -> Option<String> {
    if output.len() < MIN_RAW_BYTES {
        return None;
    }
    // Never double-compress: if the output already carries crush markers, it was
    // produced by crush (or contains text that would poison the wire format).
    // Bail to pass-through rather than risk corrupting the format.
    if looks_crushed(output) {
        return None;
    }
    let command = tool_input
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let body = crate::transforms::run(tool_name, command, output)?;
    let final_out = with_header(output.len(), body.len(), &body);

    // Net win after header overhead; also guarantees final_out < output (no enlarge).
    if final_out.len() + MIN_BYTES_SAVED > output.len() {
        return None;
    }
    Some(final_out)
}

/// True if the input already contains crush's versioned markers — i.e. it is (or
/// embeds) crush output. Cheap single scan.
#[must_use]
pub fn looks_crushed(s: &str) -> bool {
    s.contains(crate::markers::NS)
}

/// Prepend a single-line provenance header so the model (and humans reading the
/// transcript) know the output was rewritten and by how much.
fn with_header(before: usize, after: usize, body: &str) -> String {
    // before >= MIN_RAW_BYTES at call site, so the division is always defined.
    let pct = (before - after) * 100 / before;
    format!(
        "[{} {before}\u{2192}{after} bytes (-{pct}%)]\n{body}",
        crate::markers::VERSION
    )
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
        assert!(out.starts_with("[@crush/1"));
        assert!(out.len() < arr.len());
    }

    #[test]
    fn refuses_to_double_compress_its_own_output() {
        // Build a large array, compress once, then feed the result back in.
        let mut arr = String::from("[");
        for i in 0..200 {
            if i > 0 {
                arr.push(',');
            }
            arr.push_str(&format!(r#"{{"name":"item{i}","value":{i}}}"#));
        }
        arr.push(']');
        let once = compress("bash", &json!({"command": "echo"}), &arr).expect("first pass");
        assert!(compress("bash", &json!({"command": "echo"}), &once).is_none());
    }

    #[test]
    fn refuses_input_containing_crush_markers() {
        let mut s = String::from("@crush/1.cols a b c\n");
        s.push_str(&"some line of data here\n".repeat(200));
        assert!(compress("ls", &json!({}), &s).is_none());
    }
}
