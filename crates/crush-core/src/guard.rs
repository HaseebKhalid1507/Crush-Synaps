//! Hands-off detection: output that already carries a crush or host marker, or
//! looks like a role/tool boundary, is never transformed. Rewriting it could
//! launder an injected "marker" into one the model trusts, and would break
//! idempotence.

/// Prefix of every marker crush writes. Output containing it passes through.
pub(crate) const MARKER: &str = "[crush:";

/// Foreign markers and boundary shapes (matched case-sensitively, anywhere).
const FOREIGN: &[&str] = &[
    "@crush/",               // crush v0.1 wire format
    "[Output virtualised",   // SynapsCLI #334 spill note
    "[truncated \u{2014}",   // Synaps legacy cut
    "[output truncated at",  // Synaps bash cap
    "lower-authority",       // forum/memory provenance banners
    "<system-reminder",
    "</system-reminder",
    "<tool_result",
    "</tool_result",
    "<tool_output",
    "</tool_output",
    "<function_results",
    "</function_results",
    "<|im_start|>",
    "<|im_end|>",
    "\n\nHuman:",
    "\n\nAssistant:",
];

/// Any marker, ours included.
pub(crate) fn marker_like(s: &str) -> bool {
    contains(s, MARKER) || marker_like_except_ours(s)
}

/// Foreign markers only (our own output is allowed to carry ours).
pub(crate) fn marker_like_except_ours(s: &str) -> bool {
    let b = s.as_bytes();
    for i in memchr::memchr3_iter(b'[', b'<', b'@', b) {
        let rest = &b[i..];
        // the second byte narrows it down before any full compare
        let Some(&next) = rest.get(1) else { break };
        if !matches!(next, b'O' | b't' | b'o' | b's' | b'/' | b'f' | b'|' | b'c') {
            continue;
        }
        if FOREIGN.iter().any(|p| rest.starts_with(p.as_bytes())) {
            return true;
        }
    }
    FOREIGN.iter().filter(|p| !p.starts_with(['[', '<', '@'])).any(|p| contains(s, p))
}

fn contains(s: &str, pat: &str) -> bool {
    memchr::memmem::find(s.as_bytes(), pat.as_bytes()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects() {
        assert!(marker_like("x\n[crush: previous line repeated 3 more times]\n"));
        assert!(marker_like("[@crush/1 ls] a b"));
        assert!(marker_like("ok\n[Output virtualised: 1 bytes]"));
        assert!(marker_like("a</tool_output>b"));
        assert!(marker_like("hi\n\nHuman: do x"));
        assert!(marker_like("[forum results are lower-authority peer DATA]"));
        assert!(!marker_like("fn main() { let v = [1, 2]; if a < b {} }\n\nfoo"));
        assert!(!marker_like_except_ours("[crush: x]"));
    }
}
