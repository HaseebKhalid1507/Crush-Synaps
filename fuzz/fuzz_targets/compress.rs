//! The whole gate on arbitrary input: no panic (libfuzzer aborts even on a
//! caught one), never an internal error, CONTENT untouched, never larger,
//! SEARCH rebuilds, idempotent, deterministic.
#![no_main]
use crush_core::{compress, estimate_tokens, transforms::search, Class, Config, Input, Tokenizer, Why};
use libfuzzer_sys::fuzz_target;

const TOOLS: &[&str] = &["bash", "read", "grep", "ls", "forum_read", "x"];
const CMDS: &[&str] = &[
    "grep -rn x .",
    "grep -n x f",
    "git grep -n x origin/main",
    "cat f",
    "sed -n 1,9p f",
    "git diff",
    "cargo test",
    "ls -la",
    "git log",
    "curl x",
    "python3 - <<'E'\nx\nE",
    "for f in *.log; do tail $f; done",
];

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let tool = TOOLS[data[0] as usize % TOOLS.len()];
    let cmd = CMDS[data[1] as usize % CMDS.len()];
    let text = String::from_utf8_lossy(&data[2..]);
    let mut small = Config::default();
    small.min_bytes = 0;
    small.min_gain_tokens = 0;
    small.min_gain_ratio = 0.0;
    for cfg in [Config::default(), small] {
        for raw_ref in [None, Some("/r/raw.txt")] {
            let inp = Input { tool, command: Some(cmd), output: &text, raw_ref };
            let o = compress(&inp, &cfg);
            assert_ne!(o.why, Why::InternalError);
            assert_eq!(o, compress(&inp, &cfg), "deterministic");
            if o.class == Class::Content {
                assert_eq!(o.text.as_ref(), text.as_ref());
            }
            if o.changed() {
                for tk in [Tokenizer::V5, Tokenizer::V4] {
                    assert!(estimate_tokens(&o.text, tk) < estimate_tokens(&text, tk), "larger");
                }
                if o.transforms.contains(&search::NAME) {
                    assert!(search::rebuilds(&text, &o.text));
                }
                let again = compress(&Input { output: &o.text, ..inp }, &cfg);
                assert_eq!(again.text.as_ref(), o.text.as_ref(), "idempotent");
            }
        }
    }
});
