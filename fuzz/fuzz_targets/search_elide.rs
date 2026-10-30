//! SEARCH line elision: never panics; every result is exactly what its
//! markers say (verify); the grep-pattern reader survives any command line.
#![no_main]
use crush_core::transforms::search_elide;
use libfuzzer_sys::fuzz_target;

const CMDS: &[&str] = &[
    "grep -rn ab .",
    "grep -rni 'ab' src | head",
    "rg -n -S ab",
    "grep -n -E 'a+b|cd' f",
    "grep -rn \"ab\\|cd\" .",
    "git grep -n ab -- .",
    "grep -rnF 'a.b' .",
];

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(&data[1..]);
    // the first byte picks a command; odd bytes also try the input's first line as one
    let cmd = CMDS[data[0] as usize % CMDS.len()];
    if let Some(out) = search_elide::elide(&text, Some(cmd), "/r/raw.txt") {
        assert!(search_elide::verify(&text, &out), "elide does not verify");
    }
    if data[0] % 2 == 1 {
        let own = text.lines().next().unwrap_or("");
        let _ = search_elide::patterns(Some(own));
        if let Some(out) = search_elide::elide(&text, Some(own), "/r/raw.txt") {
            assert!(search_elide::verify(&text, &out), "elide does not verify (own command)");
        }
    }
});
