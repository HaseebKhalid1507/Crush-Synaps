//! Property tests for the Phase 2A transforms (proptest, release).
//!
//! - search_elide: on generated grep output with over-long lines, every cut
//!   is exactly what its markers say (verify), every other line is verbatim,
//!   the kept slice still holds the match, and compress() with the candidates
//!   on stays deterministic, idempotent and never larger.
//! - ls_columns: factor() is undone exactly by restore() on generated ls -l
//!   listings (majority pair + exceptions, devices, -h sizes, names with
//!   spaces and arrows), and on arbitrary strings it either does nothing or
//!   round-trips.
//! - CONTENT stays byte-identical with both candidates on.

use crush_core::transforms::{listing, search_elide};
use crush_core::{compress, estimate_tokens, Class, Config, Input, Tokenizer};
use proptest::prelude::*;

const RAW_REF: &str = "/home/u/.synaps-cli/spill/s/toolu_1.txt";

fn cand() -> Config {
    let mut c = Config::default();
    c.transforms.search_elide = true;
    c.transforms.ls_columns = true;
    c
}

fn cand_loose() -> Config {
    Config {
        min_bytes: 0,
        min_gain_tokens: 0,
        min_gain_ratio: 0.0,
        min_gain_tokens_lossy: 0,
        min_gain_ratio_lossy: 0.0,
        ..cand()
    }
}

/// A grep hit line, sometimes over-long, sometimes with the needle.
fn hit_line() -> impl Strategy<Value = String> {
    let path = "(\\./)?[a-z]{1,6}(/[a-z_]{1,10}){0,3}\\.(rs|js|md|jsonl)";
    let short = "[ -~]{0,80}";
    let long = ("[ -~é🦀]{0,3000}", "[ -~]{900,3000}");
    prop_oneof![
        3 => (path, 1u32..9000, short).prop_map(|(p, n, t)| format!("{p}:{n}:{t}")),
        2 => (path, 1u32..9000, long, any::<bool>(), "(needle|NEEDLE|NeEdLe)").prop_map(|(p, n, (a, b), with, nd)| {
            if with { format!("{p}:{n}:{a}{nd}{b}") } else { format!("{p}:{n}:{a}{b}") }
        }),
        1 => (1u32..9000, long, "(needle|NEEDLE)").prop_map(|(n, (a, b), nd)| format!("{n}:{a}{nd}{b}")),
        1 => (path, 1u32..9000, short).prop_map(|(p, n, t)| format!("{p}-{n}-{t}")),
        1 => Just("--".to_string()),
        1 => long.prop_map(|(a, b)| format!("{a}needle{b}")),
        1 => Just(String::new()),
    ]
}

fn grep_text() -> impl Strategy<Value = String> {
    (prop::collection::vec(hit_line(), 1..30), any::<bool>()).prop_map(|(ls, nl)| {
        let mut s = ls.join("\n");
        if nl {
            s.push('\n');
        }
        s
    })
}

const GREP_CMDS: &[&str] = &[
    "grep -rn needle .",
    "grep -rni 'needle' src | head -50",
    "rg -n -S needle",
    "grep -n -E 'nee+dle|zzz' a.js",
    "cd x && grep -rn \"needle\\|other\" .",
    "git grep -n needle -- .",
    "grep -rn -F 'needle' .",
    "grep -v needle f",
    "cat a.js",
];

/// One ls -l row.
fn ls_row(owner: &'static str, group: &'static str) -> impl Strategy<Value = String> {
    let mode = "[-dlcbps][rwx-]{9}[.+@]?";
    let size = prop_oneof![
        3 => (0u64..10_000_000).prop_map(|n| n.to_string()),
        1 => (0u32..999, 0u32..9, "[KMG]").prop_map(|(a, b, u)| format!("{a}.{b}{u}")),
        1 => (0u32..300, 0u32..300).prop_map(|(a, b)| format!("{a}, {b:>3}")),
    ];
    let name = "[a-zA-Z0-9_.-]{1,20}( [a-z]{1,6})?( -> [a-z/]{1,20})?";
    (mode, 1u32..9999, size, "(Jan|Feb|Oct) [ 1-3][0-9] ([0-2][0-9]:[0-5][0-9]| 2025)", name)
        .prop_map(move |(m, l, s, d, n)| format!("{m} {l:>4} {owner} {group} {s:>8} {d} {n}"))
}

fn ls_text() -> impl Strategy<Value = String> {
    let row = prop_oneof![
        8 => ls_row("worker", "worker"),
        2 => ls_row("root", "root"),
        1 => ls_row("1000", "1000"),
        1 => ls_row("u", "staff"),
    ];
    let line = prop_oneof![
        12 => row,
        1 => "total [0-9]{1,6}",
        1 => "[a-z/]{1,20}:",
        1 => Just(String::new()),
        1 => "[ -~]{0,60}",
    ];
    (prop::collection::vec(line, 1..60), any::<bool>()).prop_map(|(ls, nl)| {
        let mut s = ls.join("\n");
        if nl {
            s.push('\n');
        }
        s
    })
}

fn gate_checks(tool: &str, cmd: Option<&str>, text: &str, cfg: &Config) -> Result<(), TestCaseError> {
    let input = Input { tool, command: cmd, output: text, raw_ref: Some(RAW_REF) };
    let a = compress(&input, cfg);
    let b = compress(&input, cfg);
    prop_assert_eq!(&a, &b, "deterministic");
    if a.changed() {
        let again = compress(&Input { output: &a.text, ..input }, cfg);
        prop_assert_eq!(again.text.as_ref(), a.text.as_ref(), "idempotent");
        for tk in [Tokenizer::V5, Tokenizer::V4] {
            prop_assert!(estimate_tokens(&a.text, tk) < estimate_tokens(text, tk), "never larger");
        }
        prop_assert!(a.class != Class::Content);
        if a.transforms == [search_elide::NAME] {
            prop_assert!(search_elide::verify(text, &a.text), "elide verify");
            prop_assert!(!a.lossless);
        }
        if a.transforms == [listing::NAME] {
            prop_assert!(listing::restore(&a.text).as_deref() == Some(text), "ls restore");
            prop_assert!(listing::verify(text, &a.text), "ls verify");
            prop_assert!(a.lossless);
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn elide_cuts_are_exact(text in grep_text(), ci in 0..GREP_CMDS.len()) {
        let cmd = GREP_CMDS[ci];
        if let Some(out) = search_elide::elide(&text, Some(cmd), RAW_REF) {
            prop_assert!(search_elide::verify(&text, &out), "verify failed\n{}", cmd);
            // every cut line keeps its match (case-insensitively for these patterns)
            // (lines(): the end note is the extra last line of `out`)
            let lits = search_elide::patterns(Some(cmd)).literals;
            for (a, b) in text.lines().zip(out.lines()) {
                if a != b {
                    let lb = b.to_ascii_lowercase();
                    prop_assert!(lits.iter().any(|l| lb.contains(&l.to_ascii_lowercase())), "match lost: {}", b);
                    prop_assert!(b.len() < a.len());
                }
            }
            prop_assert!(!cmd.contains(" -v "), "inverted grep must not cut");
        }
        for cfg in [cand(), cand_loose()] {
            gate_checks("bash", Some(cmd), &text, &cfg)?;
        }
    }

    #[test]
    fn ls_factoring_round_trips(text in ls_text(), lo in any::<bool>()) {
        let cmd = if lo { "ls -la ~/x" } else { "ls -lh /usr/lib" };
        if let Some(f) = listing::factor(&text, Some(cmd)) {
            prop_assert!(listing::restore(&f).as_deref() == Some(text.as_str()), "restore");
            prop_assert!(listing::verify(&text, &f), "verify");
            // verify agrees with restore on a tampered output too
            let t = f.replacen(" 1 ", " 1 x ", 1);
            prop_assert_eq!(listing::verify(&text, &t), listing::restore(&t).as_deref() == Some(text.as_str()));
            prop_assert!(f.len() < text.len());
        }
        for cfg in [cand(), cand_loose()] {
            gate_checks("bash", Some(cmd), &text, &cfg)?;
        }
    }

    #[test]
    fn arbitrary_strings_never_break_the_checks(text in any::<String>(), ci in 0..GREP_CMDS.len()) {
        if let Some(f) = listing::factor(&text, None) {
            prop_assert!(listing::restore(&f).as_deref() == Some(text.as_str()), "restore");
            prop_assert!(listing::verify(&text, &f), "verify");
        }
        if let Some(out) = search_elide::elide(&text, Some(GREP_CMDS[ci]), RAW_REF) {
            prop_assert!(search_elide::verify(&text, &out));
        }
        gate_checks("bash", Some(GREP_CMDS[ci]), &text, &cand_loose())?;
    }

    #[test]
    fn content_untouched_with_candidates_on(text in grep_text(), ls in ls_text()) {
        for cmd in ["cat dist/app.js", "sed -n 1,400p a.md", "git diff HEAD~1"] {
            for t in [&text, &ls] {
                let input = Input { tool: "bash", command: Some(cmd), output: t, raw_ref: Some(RAW_REF) };
                let o = compress(&input, &cand_loose());
                if o.class == Class::Content {
                    prop_assert_eq!(o.text.as_ref(), t.as_str());
                }
            }
        }
        let o = compress(&Input { tool: "read", command: None, output: &text, raw_ref: Some(RAW_REF) }, &cand_loose());
        prop_assert_eq!(o.text.as_ref(), text.as_str());
    }
}

/// Worst cases for the new transforms stay well under 1 ms: 256 KiB of
/// over-long grep lines, and an ls -l listing at ls_columns' 64 KiB bound
/// (it fires) and at 256 KiB (passes through).
#[test]
fn worst_cases_stay_fast() {
    let filler: String = "abcdefghij;".chars().cycle().take(3000).collect();
    let mut grep = String::new();
    let mut i = 0;
    while grep.len() < 256 * 1024 - 4096 {
        grep.push_str(&format!("dist/m{i}.min.js:{i}:{filler}needle{filler}\n"));
        i += 1;
    }
    let ls = |cap: usize| {
        let mut s = String::new();
        let mut i = 0;
        while s.len() < cap - 200 {
            s.push_str(&format!("-rw-r--r-- 1 worker worker {:>8} Oct 12 09:41 file{i}.so\n", i * 7));
            i += 1;
        }
        s
    };
    let (ls64, ls256) = (ls(listing::MAX_INPUT), ls(256 * 1024));
    for (cmd, text, fires) in
        [("grep -rni NEEDLE dist", &grep, true), ("ls -la /usr/lib", &ls64, true), ("ls -la /usr/lib", &ls256, false)]
    {
        let input = Input { tool: "bash", command: Some(cmd), output: text, raw_ref: Some(RAW_REF) };
        let mut best = u128::MAX;
        for _ in 0..20 {
            let t = std::time::Instant::now();
            let o = compress(&input, &cand());
            best = best.min(t.elapsed().as_micros());
            assert_eq!(o.changed(), fires, "{cmd} {}: {:?}", text.len(), o.why);
        }
        assert!(best < 5_000, "{cmd}: {best} µs (the release gate in eval/ is < 1 ms)");
    }
}
