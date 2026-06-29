#![no_main]
use libfuzzer_sys::fuzz_target;
use serde_json::json;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let tools = ["ls", "ps", "bash", "grep", "read", ""];
    let cmds = ["ls -lah", "ps aux", "git log --pretty=a|b|c", "cat", "echo"];
    for &t in &tools {
        for &c in &cmds {
            if let Some(out) = crush::compress::compress(t, &json!({"command": c}), &s) {
                assert!(out.len() < s.len(), "ENLARGED {} -> {}", s.len(), out.len());
                // re-feeding crush output must pass through (no double-compress)
                assert!(crush::compress::compress(t, &json!({"command": c}), &out).is_none(),
                        "DOUBLE-COMPRESSED");
            }
        }
    }
});
