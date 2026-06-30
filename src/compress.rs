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
    compress_labeled(tool_name, tool_input, output).map(|(s, _)| s)
}

/// Like [`compress`], but also returns the bucket label (transform/tool)
/// for stats. The label is `"ls"`/`"ps"`/`"git_log"` for tool-aware folds,
/// `"tabular"` for JSON-array fold, `"text"` for the cleanup chain.
#[must_use]
pub fn compress_labeled(
    tool_name: &str,
    tool_input: &Value,
    output: &str,
) -> Option<(String, &'static str)> {
    if output.len() < MIN_RAW_BYTES {
        return None;
    }
    if looks_crushed(output) {
        return None;
    }
    let command = tool_input
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let (body, label) = crate::transforms::run_labeled(tool_name, command, output)?;
    let final_out = with_header(output.len(), body.len(), &body);

    if final_out.len() + MIN_BYTES_SAVED > output.len() {
        return None;
    }
    Some((final_out, label))
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

    #[test]
    fn fuzz_compress_invariants() {
        // never panics · output (if any) is strictly smaller · deterministic.
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                x
            }
            fn below(&mut self, n: usize) -> usize {
                (self.next() % n as u64) as usize
            }
        }
        let tools = ["ls", "ps", "bash", "grep", "read", ""];
        let cmds = [
            "ls -lah /usr/bin",
            "ps aux",
            "git log --pretty=a|b|c",
            "cat x",
            "echo",
        ];
        let pool = b"abcXYZ 0129 \t\n:/|=.-_[]{}\",";
        let mut rng = Rng(0x0bad_f00d_1234_5678);
        for _ in 0..8000 {
            let len = rng.below(6000);
            let mut out = String::with_capacity(len);
            for _ in 0..len {
                out.push(pool[rng.below(pool.len())] as char);
            }
            let tool = tools[rng.below(tools.len())];
            let input = json!({ "command": cmds[rng.below(cmds.len())] });

            let a = compress(tool, &input, &out);
            // never enlarges
            if let Some(ref s) = a {
                assert!(s.len() < out.len(), "enlarged {} -> {}", out.len(), s.len());
            }
            // deterministic (cache-safety invariant): same input → same output
            let b = compress(tool, &input, &out);
            assert_eq!(a, b, "non-deterministic output");
        }
    }
}
