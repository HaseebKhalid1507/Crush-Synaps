#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    for &t in &["ls", "ps", "bash", "grep", ""] {
        for &c in &["ls -lah", "ps aux", "git log --pretty=a|b", "x"] {
            if let Some(out) = crush::transforms::run(t, c, &s) {
                assert!(out.len() < s.len(), "ENLARGED {} -> {}", s.len(), out.len());
            }
        }
    }
});
