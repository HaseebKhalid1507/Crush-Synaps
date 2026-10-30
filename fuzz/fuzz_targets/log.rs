//! LOG test/build collapse: never panics; every output line is either a note
//! or an input line, in input order; the notes' counts never exceed what was
//! removed; trailing newline kept; a second pass changes nothing. test_group
//! (test_pass v2) rebuilds its input byte for byte.
#![no_main]
use crush_core::transforms::log;
use libfuzzer_sys::fuzz_target;

const R: &str = "/spill/x.txt";

fn note_count(l: &str) -> Option<usize> {
    let rest = l.trim_start().strip_prefix("[crush: ")?;
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == ',').collect();
    let k = n.replace(',', "").parse().ok()?;
    let after = &rest[n.len()..];
    (after.starts_with(" passing test") || after.starts_with(" build-progress line")).then_some(k)
}

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    if s.contains("[crush:") {
        return; // compress() never hands marker-like input to a transform
    }
    for f in [log::collapse_tests as fn(&str, &str) -> Option<String>, log::collapse_build] {
        let Some(out) = f(&s, R) else { continue };
        assert_eq!(out.ends_with('\n'), s.ends_with('\n'));
        let raw: Vec<&str> = s.split('\n').collect();
        let mut it = raw.iter();
        let mut kept = 0usize;
        let mut counted = 0usize;
        for l in out.split('\n') {
            if let Some(k) = note_count(l) {
                counted += k;
            } else if l.starts_with("[crush: the collapsed lines are in the raw output: ") {
            } else {
                assert!(it.any(|r| *r == l), "line not from the input, or out of order: {l:?}");
                kept += 1;
            }
        }
        assert!(counted <= raw.len() - kept, "notes count more lines than were removed");
        assert_eq!(f(&out, R), None, "not idempotent");
    }
    // test_pass v2: lossless, a fixed point
    if let Some(g) = log::group_tests(&s) {
        assert_eq!(log::expand_test_groups(&g), s, "test_group must rebuild its input");
        assert_eq!(log::group_tests(&g), None, "test_group not idempotent");
    }
});
