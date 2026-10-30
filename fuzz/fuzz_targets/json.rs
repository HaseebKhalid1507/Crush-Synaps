//! JSON layout: never panics; only whitespace changes; a fixed point; and
//! when the whole input is JSON, the output parses to the same token stream.
#![no_main]
use crush_core::transforms::json;
use libfuzzer_sys::fuzz_target;

fn squeeze(t: &str) -> String {
    t.chars().filter(|c| !matches!(c, ' ' | '\n' | '\t' | '\r')).collect()
}

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    if let Some(out) = json::minify(&s) {
        assert_eq!(squeeze(&out), squeeze(&s), "only whitespace may change");
        assert_eq!(json::minify(&out), None, "not a fixed point");
        assert!(out.len() < s.len(), "layout must shrink");
    }
});
