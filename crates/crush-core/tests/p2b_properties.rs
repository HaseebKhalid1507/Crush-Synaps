//! Property tests for the Phase 2B transforms (proptest): LOG test/build
//! collapse and STRUCTURED JSON layout.
//!
//! - LOG: every line that isn't a collapsible passing-test / build-progress
//!   line survives verbatim and in order (failures, panics, summaries,
//!   diagnostics, code frames, interleaved stdout); the notes' counts add up
//!   to exactly the lines removed; the gate's invariants hold through
//!   `compress` (deterministic, idempotent, never larger, lossy only with a
//!   raw_ref).
//! - JSON: the layout parses back to the same value with every string and
//!   number byte-identical (compared on the token stream), is a fixed point,
//!   and never touches compact or invalid JSON.

use crush_core::transforms::{json, log};
use crush_core::{compress, estimate_tokens, Class, Config, Input, Tokenizer};
use proptest::prelude::*;

const RAW_REF: &str = "/home/u/.synaps-cli/spill/s/toolu_1.txt";

/// Every transform on (including the ones that ship disabled), no margins.
fn all_on() -> Config {
    let mut c = Config {
        min_bytes: 0,
        min_gain_tokens: 0,
        min_gain_ratio: 0.0,
        min_gain_tokens_lossy: 0,
        min_gain_ratio_lossy: 0.0,
        ..Config::default()
    };
    c.transforms.test_pass = true;
    c.transforms.json_minify = true;
    c.transforms.search_group = true;
    c
}

/// Shipped margins, every transform on.
fn candidates() -> Config {
    let mut c = Config::default();
    c.transforms.test_pass = true;
    c.transforms.json_minify = true;
    c
}

/// Lines a test/build log is made of. `true` = collapsible.
fn log_line() -> impl Strategy<Value = (String, bool)> {
    let ident = "[a-z][a-z0-9_]{0,10}";
    prop_oneof![
        6 => (prop::collection::vec(ident, 1..4), ident).prop_map(|(m, t)| (format!("test {}::{t} ... ok", m.join("::")), true)),
        1 => (ident, prop_oneof![Just("FAILED"), Just("ignored"), Just("")]).prop_map(|(t, s)| (format!("test m::{t} ... {s}"), false)),
        1 => (1u32..300).prop_map(|n| (format!("running {n} tests"), false)),
        1 => (0u32..90, 0u32..3).prop_map(|(p, f)| (format!("test result: {}. {p} passed; {f} failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s", if f > 0 { "FAILED" } else { "ok" }), false)),
        1 => ident.prop_map(|t| (format!("thread 'm::{t}' panicked at src/lib.rs:12:5:"), false)),
        1 => "[ -~]{0,40}".prop_map(|t| (format!("seeding {t}"), false)),
        3 => (prop_oneof![Just("Compiling"), Just("Checking"), Just("Downloaded"), Just("Fresh")], ident)
            .prop_map(|(v, c)| (format!("{:>12} {c} v0.1.0", v), true)),
        1 => ident.prop_map(|c| (format!("warning: unused variable: `{c}`"), false)),
        1 => (1u32..500).prop_map(|n| (format!(" --> src/main.rs:{n}:9"), false)),
        1 => Just(("  |         ^^^^ help: prefix it with an underscore".to_string(), false)),
        2 => "[A-Za-z ]{3,30}".prop_map(|t| (format!("      ✓ {t} (3 ms)"), t.trim().len() >= 2 && t.trim_end() == t)),
        1 => "[A-Za-z ]{3,30}".prop_map(|t| (format!("      ✕ {t}"), false)),
        2 => ident.prop_map(|t| (format!("--- PASS: Test{t} (0.00s)"), true)),
        1 => ident.prop_map(|t| (format!("--- FAIL: Test{t} (0.00s)"), false)),
        2 => (ident, ident).prop_map(|(f, t)| (format!("tests/{f}.py::test_{t} PASSED  [ 10%]"), true)),
        1 => (ident, ident).prop_map(|(f, t)| (format!("tests/{f}.py::test_{t} FAILED  [ 20%]"), false)),
        1 => ident.prop_map(|p| (format!("Collecting {p}"), true)),
        1 => ident.prop_map(|p| (format!("  Downloading {p}-1.0-py3-none-any.whl (12 kB)"), true)),
        1 => Just((String::new(), false)),
        1 => "[ -~]{0,60}".prop_map(|t| (t, false)),
    ]
}

fn note_count(l: &str) -> Option<usize> {
    let t = l.trim_start();
    let rest = t.strip_prefix("[crush: ")?;
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == ',').collect();
    let k: usize = n.replace(',', "").parse().ok()?;
    let after = &rest[n.len()..];
    (after.starts_with(" passing test") || after.starts_with(" build-progress line")).then_some(k)
}

/// `needle` lines appear in `hay`, in order (a subsequence).
fn subsequence(needle: &[&str], hay: &[&str]) -> bool {
    let mut it = hay.iter();
    needle.iter().all(|n| it.any(|h| h == n))
}

/// JSON tokens (strings and numbers verbatim, structure, literals).
fn tokens(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut v = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' | b'\n' | b'\t' | b'\r' => i += 1,
            b'"' => {
                let st = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                v.push(String::from_utf8_lossy(&b[st..i]).into_owned());
            }
            c if c.is_ascii_alphanumeric() || c == b'-' || c == b'.' || c == b'+' => {
                let st = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'-' | b'.' | b'+')) {
                    i += 1;
                }
                v.push(s[st..i].to_string());
            }
            _ => {
                v.push((b[i] as char).to_string());
                i += 1;
            }
        }
    }
    v
}

fn json_value() -> impl Strategy<Value = serde_json::Value> {
    use serde_json::Value;
    let leaf = prop_oneof![
        Just(Value::Null),
        Just(Value::String(String::new())),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::from(n)),
        (-1e6f64..1e6).prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        "[ -~é\"\\\\\\n\\t]{0,20}".prop_map(Value::String),
    ];
    leaf.prop_recursive(5, 64, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(serde_json::Value::Array),
            prop::collection::vec(("[a-z_]{1,8}", inner), 0..8)
                .prop_map(|kv| serde_json::Value::Object(kv.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn log_collapse_keeps_every_other_line(lines in prop::collection::vec(log_line(), 0..120), nl in any::<bool>()) {
        let mut text = lines.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join("\n");
        if nl { text.push('\n'); }
        let raw: Vec<&str> = text.split('\n').collect();
        for (name, f) in [("tests", log::collapse_tests as fn(&str, &str) -> Option<String>), ("build", log::collapse_build)] {
            let Some(out) = f(&text, RAW_REF) else { continue };
            prop_assert_eq!(out.ends_with('\n'), text.ends_with('\n'), "{} trailing newline", name);
            let got: Vec<&str> = out.split('\n').collect();
            // every line the notes don't account for is kept, in order
            let kept: Vec<&str> = got.iter().copied().filter(|l| note_count(l).is_none() && !l.starts_with("[crush: the collapsed lines")).collect();
            prop_assert!(subsequence(&kept, &raw), "{}: output lines must come from the input, in order", name);
            let removed = raw.len() - kept.len();
            let counted: usize = got.iter().filter_map(|l| note_count(l)).sum();
            // removed = counted (+ go bookkeeping lines, which are removed uncounted)
            let bookkeeping = raw.iter().filter(|l| l.trim_start().starts_with("=== ")).count();
            prop_assert!(counted <= removed && removed <= counted + bookkeeping, "{}: notes count {} but {} lines went", name, counted, removed);
            // failures, panics, summaries, diagnostics and frames all survive
            let must: Vec<&str> = raw.iter().copied().filter(|l| l.contains("FAILED") || l.contains("FAIL:") || l.contains("panicked")
                || l.starts_with("test result:") || l.starts_with("warning:") || l.contains("-->") || l.contains("  |") || l.contains('✕')).collect();
            prop_assert!(subsequence(&must, &got), "{}: a failure / summary / diagnostic line went missing", name);
            prop_assert_eq!(f(&out, RAW_REF), None, "{} idempotent", name);
        }
        // and through the whole gate
        for raw_ref in [None, Some(RAW_REF)] {
            for cfg in [Config::default(), candidates(), all_on()] {
                let inp = Input { tool: "bash", command: Some("cargo test 2>&1"), output: &text, raw_ref };
                let a = compress(&inp, &cfg);
                prop_assert_eq!(&a, &compress(&inp, &cfg), "deterministic");
                prop_assert_eq!(a.class, Class::Log);
                if raw_ref.is_none() { prop_assert!(a.lossless); }
                if a.changed() {
                    for tk in [Tokenizer::V5, Tokenizer::V4] {
                        prop_assert!(estimate_tokens(&a.text, tk) < estimate_tokens(&text, tk));
                    }
                    let again = compress(&Input { output: &a.text, ..inp }, &cfg);
                    prop_assert_eq!(again.text.as_ref(), a.text.as_ref(), "idempotent");
                }
            }
        }
    }

    #[test]
    fn json_layout_is_lossless(v in json_value(), stream in 1usize..4) {
        let pretty: Vec<String> = (0..stream).map(|_| serde_json::to_string_pretty(&v).unwrap()).collect();
        let text = pretty.join("\n") + "\n";
        match json::minify(&text) {
            Some(out) => {
                prop_assert_eq!(tokens(&out), tokens(&text), "same tokens, byte for byte");
                // same values as parsing the original text (not `v`: serde_json's
                // own float printing doesn't round-trip every f64)
                let parse = |t: &str| serde_json::Deserializer::from_str(t).into_iter().map(|d| d.unwrap()).collect::<Vec<serde_json::Value>>();
                let docs = parse(&out);
                prop_assert_eq!(docs.len(), stream);
                prop_assert_eq!(docs, parse(&text));
                prop_assert_eq!(json::minify(&out), None, "fixed point");
                // one top-level element per line: the line count says the length
                if let serde_json::Value::Array(a) = &v {
                    if stream == 1 && !a.is_empty() {
                        prop_assert_eq!(out.trim_end().lines().count(), a.len() + 2);
                    }
                }
            }
            None => prop_assert!(!text.trim().contains('\n') || pretty.iter().all(|p| !p.contains('\n')), "pretty JSON left alone"),
        }
        prop_assert_eq!(json::minify(&(serde_json::to_string(&v).unwrap() + "\n")), None, "compact JSON untouched");
        // through the gate: STRUCTURED, lossless, no raw_ref needed
        let o = compress(&Input { tool: "bash", command: Some("jq . "), output: &text, raw_ref: None }, &all_on());
        prop_assert!(o.lossless);
        if o.changed() {
            prop_assert_eq!(tokens(&o.text), tokens(&text));
            let cfg = all_on();
            let again = compress(&Input { tool: "bash", command: Some("jq . "), output: &o.text, raw_ref: None }, &cfg);
            prop_assert_eq!(again.text.as_ref(), o.text.as_ref());
        }
    }

    #[test]
    fn json_never_touches_broken_input(text in "[\\[\\]{}\":, \\n0-9a-z.-]{0,200}") {
        if let Some(out) = json::minify(&text) {
            // only whitespace ever changes (inside valid column-0 blocks)
            let squeeze = |t: &str| t.chars().filter(|c| !matches!(c, ' ' | '\n' | '\t' | '\r')).collect::<String>();
            prop_assert_eq!(squeeze(&out), squeeze(&text));
            prop_assert_eq!(json::minify(&out), None, "fixed point");
        }
    }
}

#[test]
fn log_and_json_256k_stay_fast() {
    let cfg = candidates();
    let tests: String = (0..6000).map(|i| format!("test crate_{}::module::case_{i} ... ok\n", i / 300)).collect();
    let mut build = String::new();
    for i in 0..9000 {
        build.push_str(&format!("   Compiling dependency-{i} v0.{}.0\n", i % 40));
    }
    // the largest JSON the layout takes (MAX_JSON_BYTES)
    let v: Vec<serde_json::Value> =
        (0..1200).map(|i| serde_json::json!({"id": i, "name": format!("item {i}"), "tags": ["a", "b"], "x": null})).collect();
    let pretty = serde_json::to_string_pretty(&v).unwrap();
    assert!(pretty.len() <= json::MAX_JSON_BYTES, "{}", pretty.len());
    for (cmd, text) in [("cargo test", tests), ("cargo build", build), ("jq .", pretty)] {
        let t = std::time::Instant::now();
        let o = compress(&Input { tool: "bash", command: Some(cmd), output: &text, raw_ref: Some(RAW_REF) }, &cfg);
        assert!(o.changed(), "{cmd}: {:?}", o.why);
        assert!(t.elapsed().as_millis() < 500, "{cmd}: {:?}", t.elapsed());
    }
}
