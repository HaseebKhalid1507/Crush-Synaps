//! The compression pipeline.
//!
//! `compress(tool_input, output)` returns `Some(compressed)` when a transform
//! meaningfully shrinks the output, or `None` to pass the output through
//! unchanged. Slice 1 has no transforms — it is a pure pass-through that only
//! enforces the size floor. Later slices register transforms here.
//!
//! Safety invariant: this function must never panic and must never return a
//! string that is *larger* than the input. A compression layer that breaks or
//! bloats a tool's output is worse than no compression at all.

use serde_json::Value;

/// Don't bother compressing anything smaller than this — the framing overhead
/// and header cost outweigh any win.
pub const MIN_RAW_BYTES: usize = 2048;

/// Only emit a Replace when we save at least this many bytes. Keeps tiny,
/// pointless rewrites out of the conversation.
pub const MIN_BYTES_SAVED: usize = 256;

/// Run the pipeline. Returns the compressed string (with a one-line provenance
/// header) only when it is a genuine, meaningful win; otherwise `None`.
pub fn compress(_tool_input: &Value, output: &str) -> Option<String> {
    if output.len() < MIN_RAW_BYTES {
        return None;
    }

    // Slice 1: no transforms registered yet — always pass through.
    let best = run_transforms(output)?;

    let saved = output.len().saturating_sub(best.len());
    if saved < MIN_BYTES_SAVED {
        return None;
    }
    Some(with_header(output.len(), best.len(), &best))
}

/// Apply the transform suite and return the smallest result that is strictly
/// smaller than the input.
fn run_transforms(output: &str) -> Option<String> {
    crate::transforms::run(output)
}

/// Prepend a single-line provenance header so the model (and humans reading
/// the transcript) know the output was rewritten and by how much.
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
        assert!(compress(&json!({}), &small).is_none());
    }

    #[test]
    fn passes_through_when_no_transform_helps() {
        // Slice 1: nothing registered, so even large output passes through.
        let big = "x".repeat(100_000);
        assert!(compress(&json!({}), &big).is_none());
    }
}
