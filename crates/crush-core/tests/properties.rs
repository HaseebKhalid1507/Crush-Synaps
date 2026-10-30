//! Property tests for the gate's invariants (proptest).
//!
//! Every generated result must be: deterministic, idempotent, never larger in
//! estimated tokens (both families), byte-identical for CONTENT, and for SEARCH
//! rebuild every original line byte for byte. Repeat collapse must expand back.

use crush_core::{classify, compress, estimate_tokens, transforms::search, Class, Config, Input, Tokenizer};
use proptest::prelude::*;

const RAW_REF: &str = "/home/u/.synaps-cli/spill/s/toolu_1.txt";

const COMMANDS: &[&str] = &[
    "grep -rn foo src",
    "rg -n foo",
    "grep -rn -C2 foo .",
    "git grep -n foo origin/main",
    "grep -n foo a.rs",
    "grep -rl foo",
    "grep -c foo -r .",
    "cat src/main.rs",
    "sed -n 1,80p src/lib.rs",
    "git diff HEAD~1",
    "head -50 README.md",
    "cargo test 2>&1 | tail -40",
    "npm install",
    "ls -la",
    "git log --oneline",
    "curl -s https://x",
    "python3 - <<'EOF'\nprint(1)\nEOF",
    "for f in *.log; do tail -n 5 $f; done",
    "make 2>&1",
];
const TOOLS: &[&str] = &["bash", "read", "grep", "ls", "forum_read", "web-tools_fetch", "unknown_tool"];

fn cfgs() -> Vec<Config> {
    let mut all_on = Config { min_bytes: 0, min_gain_tokens: 0, min_gain_ratio: 0.0, ..Config::default() };
    all_on.transforms.search_group = true;
    all_on.classes.prose = true;
    vec![Config::default(), all_on]
}

/// Text with the shapes crush cares about mixed in at random.
fn shaped_text() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        3 => "[a-zA-Z0-9 _./:;(){}=<>\"'-]{0,60}",
        2 => ("[a-z]{1,8}(/[a-z]{1,8}){0,3}\\.(rs|py|md)", 1u32..400, "[ -~]{0,50}").prop_map(|(p, n, t)| format!("{p}:{n}:{t}")),
        1 => ("[a-z]{1,8}/[a-z]{1,8}\\.rs", 1u32..400, "[ -~]{0,30}").prop_map(|(p, n, t)| format!("{p}-{n}-{t}")),
        1 => Just("--".to_string()),
        1 => ("[0-9]{1,3}%", "[0-9]{1,3}%").prop_map(|(a, b)| format!("{a}\r{b}")),
        1 => "[A-Za-z0-9+/]{250,400}",
        1 => "[0-9a-f]{120,200}",
        1 => Just("[crush: fake marker]".to_string()),
        1 => Just(String::new()),
    ];
    let line = prop_oneof![
        5 => piece.clone(),
        1 => (piece, 2usize..60).prop_map(|(l, n)| vec![l; n].join("\n")),
    ];
    (prop::collection::vec(line, 0..80), any::<bool>()).prop_map(|(ls, nl)| {
        let mut s = ls.join("\n");
        if nl {
            s.push('\n');
        }
        s
    })
}

/// Realistic grep output: files, hits, context, separators, noise.
fn grep_output() -> impl Strategy<Value = String> {
    let path = "(\\./|/home/u/)?[a-z]{1,6}(-[0-9]{1,2}-[a-z]{1,3})?(/[a-z_]{1,10}){0,4}\\.(rs|py|ts|md)";
    let text = "[ -~]{0,80}";
    let file = (path, prop::collection::vec((1u32..5000, text, 0u8..4), 1..12)).prop_map(|(p, hits)| {
        let mut out = Vec::new();
        for (n, t, k) in hits {
            match k {
                0 => out.push(format!("{p}-{}-ctx {t}", n.saturating_sub(1))),
                1 => out.push("--".to_string()),
                _ => {}
            }
            out.push(format!("{p}:{n}:{t}"));
        }
        out.join("\n")
    });
    let noise = prop_oneof![
        Just("Binary file target/x.bin matches".to_string()),
        Just("grep: src/y: binary file matches".to_string()),
        Just("---".to_string()),
        "[a-z/]{3,20}\\.rs",
    ];
    let chunk = prop_oneof![6 => file, 1 => noise];
    prop::collection::vec(chunk, 1..20).prop_map(|c| c.join("\n") + "\n")
}

fn check(tool: &str, cmd: Option<&str>, text: &str) -> Result<(), TestCaseError> {
    for cfg in cfgs() {
        for raw_ref in [None, Some(RAW_REF)] {
            let inp = Input { tool, command: cmd, output: text, raw_ref };
            let a = compress(&inp, &cfg);
            let b = compress(&inp, &cfg);
            prop_assert_eq!(&a, &b, "deterministic");
            let class = classify(tool, cmd, text).class;
            prop_assert_eq!(a.class, class);
            if class == Class::Content {
                prop_assert!(!a.changed(), "CONTENT must pass through borrowed");
                prop_assert_eq!(a.text.as_ref(), text);
            }
            if raw_ref.is_none() {
                prop_assert!(a.lossless, "lossy without a raw_ref");
            }
            if a.changed() {
                for tk in [Tokenizer::V5, Tokenizer::V4] {
                    prop_assert!(estimate_tokens(&a.text, tk) < estimate_tokens(text, tk), "never larger ({:?})", tk);
                }
                if a.transforms.contains(&search::NAME) {
                    prop_assert!(search::rebuilds(text, &a.text), "SEARCH lines must rebuild");
                }
                let again = compress(&Input { output: &a.text, ..inp }, &cfg);
                prop_assert_eq!(again.text.as_ref(), a.text.as_ref(), "idempotent");
            }
        }
    }
    Ok(())
}

/// Undo repeat notes: `L\n[crush: previous line repeated K more times; …]` -> L × (K+1).
fn expand_repeats(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for l in s.split('\n') {
        if let Some(rest) = l.strip_prefix("[crush: previous line repeated ") {
            let k: usize = rest.split(' ').next().unwrap().replace(',', "").parse().unwrap();
            let prev = out.last().unwrap().clone();
            for _ in 0..k {
                out.push(prev.clone());
            }
        } else {
            out.push(l.to_string());
        }
    }
    out.join("\n")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1500, ..ProptestConfig::default() })]

    #[test]
    fn invariants_on_shaped_text(text in shaped_text(), ci in 0..COMMANDS.len(), ti in 0..TOOLS.len()) {
        check(TOOLS[ti], Some(COMMANDS[ci]), &text)?;
        check(TOOLS[ti], None, &text)?;
    }

    #[test]
    fn invariants_on_arbitrary_strings(text in any::<String>(), ci in 0..COMMANDS.len()) {
        check("bash", Some(COMMANDS[ci]), &text)?;
    }

    #[test]
    fn grep_output_groups_and_rebuilds(text in grep_output()) {
        check("bash", Some("grep -rn foo ."), &text)?;
        if let Some(g) = search::group(&text) {
            prop_assert!(search::rebuilds(&text, &g));
            prop_assert_eq!(search::group(&g), None, "grouped output must not group again");
        }
    }

    #[test]
    fn content_is_byte_identical(text in shaped_text(), ci in 7usize..11) {
        let mut cfg = Config::default();
        cfg.classes.content = true; // even when switched on: no CONTENT transform exists
        for raw_ref in [None, Some(RAW_REF)] {
            let o = compress(&Input { tool: "bash", command: Some(COMMANDS[ci]), output: &text, raw_ref }, &cfg);
            prop_assert_eq!(o.class, Class::Content);
            prop_assert_eq!(o.text.as_ref(), text.as_str());
            let o = compress(&Input { tool: "read", command: None, output: &text, raw_ref }, &cfg);
            prop_assert_eq!(o.text.as_ref(), text.as_str());
        }
    }

    #[test]
    fn repeats_are_lossless(lines in prop::collection::vec(("[a-z ]{20,70}", 1usize..80), 1..12)) {
        let text: String = lines.iter().map(|(l, n)| format!("{}\n", vec![l.as_str(); *n].join("\n"))).collect();
        if let Some(c) = crush_core::transforms::repeats::collapse(&text) {
            prop_assert_eq!(expand_repeats(&c), text);
        }
    }
}

#[test]
fn worst_case_inputs_stay_fast() {
    // 256 KiB shapes that used to be slow or pathological
    let cfg = Config::default();
    let minified = format!("{{\"a\":[{}]}}", (0..30000).map(|i| format!("{i},")).collect::<String>());
    let hits: String = (0..4000).map(|i| format!("crates/x/src/m{}.rs:{i}:    let v = f({i});\n", i / 40)).collect();
    let cr: String = (0..20000).map(|i| format!("{i}%\r")).collect::<String>() + "\n";
    let one_line = "x".repeat(256 * 1024);
    for (cmd, text) in [("curl x", minified), ("grep -rn v crates", hits), ("pip install x", cr), ("echo", one_line)] {
        let t = std::time::Instant::now();
        let o = compress(&Input { tool: "bash", command: Some(cmd), output: &text, raw_ref: Some(RAW_REF) }, &cfg);
        let dt = t.elapsed();
        // debug build: generous bound; the release p99 is measured by the eval
        assert!(dt.as_millis() < 500, "{cmd}: {dt:?}");
        let _ = o;
    }
}
