//! LOG: passing-test lines and build-progress lines collapsed into counts.
//!
//! Two transforms, both lossy (the collapsed lines are gone, so they only run
//! when the host keeps the raw output at `raw_ref`):
//!
//! - [`TESTS`]: a run of three or more consecutive *passing* test lines from a
//!   test runner (cargo/libtest `test x ... ok`, jest/vitest/mocha `✓ x`,
//!   node --test `✔ x (1ms)`, pytest -v `f.py::t PASSED`, go test -v
//!   `--- PASS: T` plus the `=== RUN/PAUSE/CONT` lines of tests that passed)
//!   becomes one note, in words, with the modules the tests were in:
//!
//!   ```text
//!   [crush: 118 passing tests not listed: 72 in tui::render, 30 in tui::input, 16 in config]
//!   ```
//!
//! - [`BUILD`]: a run of two or more build-progress lines (cargo `Compiling`,
//!   `Checking`, `Downloaded`, ...; pip `Collecting`, `Downloading`, `Using
//!   cached`, `Requirement already satisfied`, progress bars; `go: downloading`;
//!   maven `Downloading from`) becomes one note with the count per verb.
//!
//! Everything else stays byte for byte where it was: failures, panics,
//! assertions, backtraces, `failures:` sections, ignored and unfinished tests,
//! every `test result:` / summary line, compiler warnings and errors with their
//! code frames, `Finished`, `Running`, and any interleaved test stdout. A line
//! is only collapsed when its whole shape matches; anything unusual (a status
//! other than pass, trailing text, escape codes) keeps the line.
//!
//! One closing line points at the raw output.

use super::{group_digits, lines};

pub const TESTS: &str = "test_pass";
pub const BUILD: &str = "build_progress";

/// Shortest run of passing-test lines worth a note.
const MIN_TEST_RUN: usize = 3;
/// Shortest run of build-progress lines worth a note.
const MIN_BUILD_RUN: usize = 2;
/// Most module groups named in one note (the rest are summed).
const MAX_GROUPS: usize = 6;

/// The closing line both transforms share. The pointer is the expensive part
/// (a spill path is ~60 tokens), so the line says no more than it must; the
/// notes themselves say what was collapsed.
fn closing(raw_ref: &str) -> String {
    format!("[crush: the collapsed lines are in the raw output: {raw_ref}]")
}

/// Already pointing at the raw output (ours, or `progress`'s marker)?
fn has_closing(text: &str, raw_ref: &str) -> bool {
    memchr::memmem::find(text.as_bytes(), format!("raw output: {raw_ref}]").as_bytes()).is_some()
}

// ── passing tests ───────────────────────────────────────────────────────────

/// What a passing-test line tells us: the group it is counted under.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pass<'a> {
    /// libtest `test a::b::c ... ok` -> module `a::b`, or a doc test's file.
    Named(&'a str),
    /// jest/mocha/node/go: no module to report.
    Plain,
    /// go `=== RUN`/`PAUSE`/`CONT` of a test that passed: removed, not counted.
    Bookkeeping,
}

fn libtest_ok(l: &str) -> Option<&str> {
    let name = l.strip_prefix("test ")?.strip_suffix(" ... ok")?;
    let b = name.as_bytes();
    if b.is_empty() || b[0] == b' ' || b.contains(&b'\t') {
        return None;
    }
    // a second ` ... ` means two results on one line (interleaved output)
    if memchr::memchr(b'.', b).is_some() && name.contains(" ... ") {
        return None;
    }
    Some(name)
}

/// Module of a libtest name: `a::b::c` -> `a::b`; a doc test
/// `src/lib.rs - foo::bar (line 12)` -> `src/lib.rs`; a bare name -> "".
fn libtest_group(name: &str) -> &str {
    let b = name.as_bytes();
    // doc tests: `src/lib.rs - foo::bar (line 12)` (module paths have no '-')
    if memchr::memchr(b'-', b).is_some() {
        if let Some((file, _)) = name.split_once(" - ") {
            return file;
        }
    }
    // last `::` (byte search: no searcher setup per line)
    let mut k = b.len();
    while k >= 2 {
        k -= 1;
        if b[k] == b':' && b[k - 1] == b':' {
            return &name[..k - 1];
        }
    }
    ""
}

/// `✓ name`, `√ name`, `✔ name`, with leading indentation, nothing odd.
fn check_mark(l: &str) -> bool {
    let t = l.trim_start_matches(' ');
    let rest = t.strip_prefix('✓').or_else(|| t.strip_prefix('√')).or_else(|| t.strip_prefix('✔'));
    let Some(rest) = rest else { return false };
    let Some(name) = rest.strip_prefix(' ') else { return false };
    let name = name.trim_end();
    // at least a word, and no second mark (a summary table) or tabs
    name.len() >= 2 && !name.contains(['✓', '✔', '✗', '✕', '✖', '×', '\t'])
}

/// pytest -v: `tests/test_a.py::test_x PASSED  [ 10%]` -> file.
fn pytest_pass(l: &str) -> Option<&str> {
    let (id, rest) = l.split_once(' ')?;
    if !id.contains("::") || id.starts_with(':') {
        return None;
    }
    let rest = rest.trim_start();
    let tail = rest.strip_prefix("PASSED")?.trim();
    if !(tail.is_empty() || (tail.starts_with('[') && tail.ends_with("%]"))) {
        return None;
    }
    Some(id.split_once("::").map_or(id, |(f, _)| f))
}

/// go test -v `--- PASS: TestX (0.00s)` (subtests indented) -> name.
fn go_pass(l: &str) -> Option<&str> {
    let t = l.trim_start_matches(' ');
    let rest = t.strip_prefix("--- PASS: ")?;
    let (name, dur) = rest.rsplit_once(" (")?;
    if name.is_empty() || name.contains(' ') || !dur.ends_with("s)") {
        return None;
    }
    Some(name)
}

/// go test -v `=== RUN   TestX`, `=== PAUSE TestX`, `=== CONT  TestX`.
fn go_bookkeeping(l: &str) -> Option<&str> {
    let rest = l.trim_start_matches(' ').strip_prefix("=== ")?;
    let (verb, name) = rest.split_once(' ')?;
    if !matches!(verb, "RUN" | "PAUSE" | "CONT" | "NAME") {
        return None;
    }
    let name = name.trim_start();
    (!name.is_empty() && !name.contains(' ')).then_some(name)
}

fn classify_pass<'a>(l: &'a str, go_passed: &[&str], controls: bool) -> Option<Pass<'a>> {
    if controls && (l.contains('\x1b') || l.contains('\r')) {
        return None;
    }
    if let Some(n) = libtest_ok(l) {
        return Some(Pass::Named(libtest_group(n)));
    }
    if let Some(f) = pytest_pass(l) {
        return Some(Pass::Named(f));
    }
    if let Some(n) = go_pass(l) {
        // subtests count under their parent test; top-level tests are plain
        return Some(n.split_once('/').map_or(Pass::Plain, |(parent, _)| Pass::Named(parent)));
    }
    if check_mark(l) {
        return Some(Pass::Plain);
    }
    if let Some(n) = go_bookkeeping(l) {
        if go_passed.binary_search(&n).is_ok() {
            return Some(Pass::Bookkeeping);
        }
    }
    None
}

/// `[crush: 118 passing tests not listed: 72 in a::b, 30 in c, 16 in 3 other modules]`
fn test_note(indent: &str, groups: &[(&str, usize)], total: usize) -> String {
    let mut s = format!(
        "{indent}[crush: {} passing test{} not listed",
        group_digits(total),
        if total == 1 { "" } else { "s" }
    );
    let named: Vec<&(&str, usize)> = groups.iter().filter(|(g, _)| !g.is_empty()).collect();
    if !named.is_empty() {
        s.push_str(": ");
        let shown = if named.len() > MAX_GROUPS { MAX_GROUPS - 1 } else { named.len() };
        let mut parts: Vec<String> =
            named[..shown].iter().map(|(g, n)| format!("{} in {g}", group_digits(*n))).collect();
        let rest_tests: usize = named[shown..].iter().map(|(_, n)| n).sum();
        if rest_tests > 0 {
            parts.push(format!("{} in {} other modules", group_digits(rest_tests), named.len() - shown));
        }
        let top: usize = groups.iter().filter(|(g, _)| g.is_empty()).map(|(_, n)| n).sum();
        if top > 0 {
            parts.push(format!("{} at the top level", group_digits(top)));
        }
        s.push_str(&parts.join(", "));
    }
    s.push(']');
    s
}

/// Collapse runs of passing-test lines. `None` when nothing changed.
pub fn collapse_tests(text: &str, raw_ref: &str) -> Option<String> {
    let b = text.as_bytes();
    // cheap reject: no shape we know
    let has = memchr::memmem::find(b, b" ... ok").is_some()
        || memchr::memmem::find(b, "✓".as_bytes()).is_some()
        || memchr::memmem::find(b, "✔".as_bytes()).is_some()
        || memchr::memmem::find(b, "√".as_bytes()).is_some()
        || memchr::memmem::find(b, b" PASSED").is_some()
        || memchr::memmem::find(b, b"--- PASS: ").is_some();
    if !has {
        return None;
    }
    let (ls, trailing) = lines(text);
    let mut go_passed: Vec<&str> = if memchr::memmem::find(b, b"--- PASS: ").is_some() {
        ls.iter().filter_map(|l| go_pass(l)).collect()
    } else {
        Vec::new()
    };
    go_passed.sort_unstable();
    go_passed.dedup();
    let controls = memchr::memchr2(b'\x1b', b'\r', b).is_some();
    let kinds: Vec<Option<Pass>> = ls.iter().map(|l| classify_pass(l, &go_passed, controls)).collect();
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut i = 0;
    // `out.is_empty()` is ambiguous after a leading empty line: track it
    let mut first = true;
    let mut emit = |out: &mut String, l: &str| {
        if !first {
            out.push('\n');
        }
        first = false;
        out.push_str(l);
    };
    while i < ls.len() {
        if kinds[i].is_none() {
            emit(&mut out, ls[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < ls.len() && kinds[j].is_some() {
            j += 1;
        }
        let counted = kinds[i..j].iter().filter(|k| !matches!(k, Some(Pass::Bookkeeping))).count();
        if counted >= MIN_TEST_RUN {
            let mut groups: Vec<(&str, usize)> = Vec::new();
            for k in kinds[i..j].iter().flatten() {
                let g = match k {
                    Pass::Named(g) => *g,
                    Pass::Plain => "",
                    Pass::Bookkeeping => continue,
                };
                match groups.iter_mut().find(|(x, _)| *x == g) {
                    Some(e) => e.1 += 1,
                    None => groups.push((g, 1)),
                }
            }
            let indent_len = ls[i].len() - ls[i].trim_start_matches(' ').len();
            emit(&mut out, &test_note(&ls[i][..indent_len], &groups, counted));
            changed = true;
        } else {
            for l in &ls[i..j] {
                emit(&mut out, l);
            }
        }
        i = j;
    }
    if !changed {
        return None;
    }
    Some(finish(out, trailing, raw_ref))
}

// ── build progress ──────────────────────────────────────────────────────────

/// Cargo status verbs that only report progress (never `Finished`, `Running`,
/// `Executable`, `Installed`, `Replaced`, `warning`, `error`).
const CARGO_VERBS: &[&str] = &[
    "Compiling",
    "Checking",
    "Documenting",
    "Downloaded",
    "Downloading",
    "Fresh",
    "Locking",
    "Adding",
    "Updating",
    "Blocking",
    "Unpacking",
    "Packaging",
    "Verifying",
    "Archiving",
    "Fetching",
];

/// pip progress line prefixes and the label each is counted under.
const PIP_PREFIXES: &[(&str, &str)] = &[
    ("Collecting ", "Collecting"),
    ("  Downloading ", "Downloading"),
    ("Downloading ", "Downloading"),
    ("  Using cached ", "Using cached"),
    ("Using cached ", "Using cached"),
    ("Requirement already satisfied: ", "Requirement already satisfied"),
    ("  Preparing metadata ", "Preparing metadata"),
    ("Preparing metadata ", "Preparing metadata"),
    ("  Getting requirements to build ", "Getting requirements to build"),
    ("  Installing build dependencies", "Installing build dependencies"),
    ("Installing build dependencies", "Installing build dependencies"),
    ("  Building wheel for ", "Building wheel"),
    ("Building wheel for ", "Building wheel"),
    ("  Created wheel for ", "Created wheel"),
    ("Created wheel for ", "Created wheel"),
    ("  Stored in directory: ", "Stored in directory"),
    ("Obtaining ", "Obtaining"),
];

/// The verb a build-progress line is counted under, or `None`.
fn build_verb(l: &str) -> Option<&str> {
    let t = l.trim_start_matches(' ');
    let indent = l.len() - t.len();
    // every shape below starts with one of these (after indentation)
    if !matches!(t.as_bytes().first(), Some(b'A'..=b'Z' | b'g' | 0xE2)) {
        return None;
    }
    let v = build_verb_shape(l, t, indent)?;
    (!(l.contains('\x1b') || l.contains('\r') || l.contains('\t'))).then_some(v)
}

fn build_verb_shape<'a>(l: &'a str, t: &'a str, indent: usize) -> Option<&'a str> {
    if let Some((verb, rest)) = t.split_once(' ') {
        // cargo's own shape: the verb right-aligned to 12 columns, one space,
        // a crate / index name (so git's `Updating a..b` never matches)
        if CARGO_VERBS.contains(&verb) && indent + verb.len() == 12 && !rest.is_empty() && !rest.starts_with(' ') {
            return Some(verb);
        }
    }
    for (p, label) in PIP_PREFIXES {
        if l.starts_with(p) {
            return Some(label);
        }
    }
    if let Some(rest) = l.strip_prefix("go: downloading ") {
        if !rest.is_empty() {
            return Some("go: downloading");
        }
    }
    // maven: `Downloading from central: https://…` / `Downloaded from …`
    if (l.starts_with("Downloading from ") || l.starts_with("Downloaded from ")) && l.contains("://") {
        return Some(if l.starts_with("Downloading") { "Downloading" } else { "Downloaded" });
    }
    // pip/rich progress bars: indentation, then box-drawing bar and a rate
    if indent >= 2 && t.starts_with(['━', '╸', '╺']) && (t.contains(" eta ") || t.contains("/s ")) {
        return Some("progress bar");
    }
    None
}

fn build_note(verbs: &[(&str, usize)], total: usize) -> String {
    let parts: Vec<String> = verbs.iter().map(|(v, n)| format!("{} {v}", group_digits(*n))).collect();
    format!("[crush: {} build-progress lines not shown: {}]", group_digits(total), parts.join(", "))
}

/// Collapse runs of build-progress lines. `None` when nothing changed.
pub fn collapse_build(text: &str, raw_ref: &str) -> Option<String> {
    let (ls, trailing) = lines(text);
    let verbs: Vec<Option<&str>> = ls.iter().map(|l| build_verb(l)).collect();
    if verbs.iter().filter(|v| v.is_some()).count() < MIN_BUILD_RUN {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut first = true;
    let mut emit = |out: &mut String, l: &str| {
        if !first {
            out.push('\n');
        }
        first = false;
        out.push_str(l);
    };
    let mut changed = false;
    let mut i = 0;
    while i < ls.len() {
        if verbs[i].is_none() {
            emit(&mut out, ls[i]);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < ls.len() && verbs[j].is_some() {
            j += 1;
        }
        if j - i >= MIN_BUILD_RUN {
            let mut by: Vec<(&str, usize)> = Vec::new();
            for v in verbs[i..j].iter().flatten() {
                match by.iter_mut().find(|(x, _)| x == v) {
                    Some(e) => e.1 += 1,
                    None => by.push((v, 1)),
                }
            }
            emit(&mut out, &build_note(&by, j - i));
            changed = true;
        } else {
            for l in &ls[i..j] {
                emit(&mut out, l);
            }
        }
        i = j;
    }
    if !changed {
        return None;
    }
    Some(finish(out, trailing, raw_ref))
}

/// Append the closing line (once) and restore the trailing newline.
fn finish(mut out: String, trailing: bool, raw_ref: &str) -> String {
    if !has_closing(&out, raw_ref) {
        out.push('\n');
        out.push_str(&closing(raw_ref));
    }
    if trailing {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: &str = "R";

    fn cargo_block(n: usize, fail: Option<usize>) -> String {
        let mut s = format!("\nrunning {n} tests\n");
        for k in 0..n {
            let st = if Some(k) == fail { "FAILED" } else { "ok" };
            let m = if k % 3 == 0 { "tui::render" } else { "config::parse" };
            s.push_str(&format!("test {m}::case_{k} ... {st}\n"));
        }
        s.push_str("\ntest result: ok. 9 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n");
        s
    }

    #[test]
    fn cargo_runs_collapse_failures_stay() {
        let raw = cargo_block(10, Some(5));
        let out = collapse_tests(&raw, R).unwrap();
        assert!(out.contains("test config::parse::case_5 ... FAILED\n"));
        assert!(out.contains("[crush: 5 passing tests not listed: 2 in tui::render, 3 in config::parse]"), "{out}");
        assert!(out.contains("[crush: 4 passing tests not listed: 2 in tui::render, 2 in config::parse]"), "{out}");
        assert!(out.contains("test result: ok. 9 passed; 1 failed;"));
        assert!(out.ends_with(&format!("{}\n", closing(R))));
        assert!(collapse_tests(&out, R).is_none(), "idempotent");
    }

    #[test]
    fn short_runs_and_odd_lines_stay() {
        let raw = "test a::b ... ok\ntest a::c ... ok\ntest a::d ... FAILED\ntest a::e ... ok extra\n";
        assert_eq!(collapse_tests(raw, R), None);
        // unfinished test (no status) and ignored stay; ok-lines with escapes stay
        let raw = "test a ... ok\ntest b ... ok\ntest c ... \x1b[32mok\x1b[0m\ntest d ... ignored\n";
        assert_eq!(collapse_tests(raw, R), None);
    }

    #[test]
    fn other_runners() {
        let jest = "PASS src/a.test.js\n  Cart\n    ✓ adds (3 ms)\n    ✓ removes\n    ✓ totals (1 ms)\n    ✕ pays (2 ms)\n";
        let out = collapse_tests(jest, R).unwrap();
        assert!(out.starts_with("PASS src/a.test.js\n  Cart\n    [crush: 3 passing tests not listed]\n    ✕ pays (2 ms)\n"), "{out}");
        let py = "t/a.py::test_1 PASSED  [ 10%]\nt/a.py::test_2 PASSED  [ 20%]\nt/b.py::C::test_3 PASSED [ 30%]\nt/b.py::test_4 FAILED [ 40%]\n";
        let out = collapse_tests(py, R).unwrap();
        assert!(out.starts_with("[crush: 3 passing tests not listed: 2 in t/a.py, 1 in t/b.py]\nt/b.py::test_4 FAILED"), "{out}");
        let go = "=== RUN   TestA\n--- PASS: TestA (0.00s)\n=== RUN   TestB\n    --- PASS: TestA/sub (0.00s)\n--- PASS: TestB (0.01s)\n=== RUN   TestC\n    c_test.go:9: boom\n--- FAIL: TestC (0.00s)\n=== RUN   TestD\n--- PASS: TestD (0.00s)\n";
        let out = collapse_tests(go, R).unwrap();
        // TestC failed: its RUN line stays with its output
        assert!(out.starts_with("[crush: 3 passing tests not listed: 1 in TestA, 2 at the top level]\n=== RUN   TestC\n    c_test.go:9: boom\n--- FAIL: TestC"), "{out}");
        assert!(out.contains("=== RUN   TestD\n--- PASS: TestD (0.00s)\n"), "a lone pass stays: {out}");
    }

    #[test]
    fn groups_cap() {
        let mut raw = String::new();
        for m in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            raw.push_str(&format!("test {m}::x ... ok\ntest {m}::y ... ok\n"));
        }
        raw.push_str("test top ... ok\n");
        let out = collapse_tests(&raw, R).unwrap();
        assert!(
            out.starts_with("[crush: 17 passing tests not listed: 2 in a, 2 in b, 2 in c, 2 in d, 2 in e, 6 in 3 other modules, 1 at the top level]"),
            "{out}"
        );
    }

    #[test]
    fn build_progress() {
        let raw = "   Compiling libc v0.2.150\n   Compiling cfg-if v1.0.0\n    Checking foo v0.1.0 (/w/foo)\nwarning: unused variable: `x`\n --> src/main.rs:2:9\n  |\n2 |     let x = 1;\n  |         ^ help: if this is intentional, prefix it with an underscore: `_x`\n\n   Compiling bar v0.1.0\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.2s\n";
        let out = collapse_build(raw, R).unwrap();
        assert!(out.starts_with("[crush: 3 build-progress lines not shown: 2 Compiling, 1 Checking]\nwarning: unused variable"), "{out}");
        assert!(out.contains("\n   Compiling bar v0.1.0\n    Finished"), "a lone line stays: {out}");
        assert!(out.contains("2 |     let x = 1;\n  |         ^ help"));
        let pip = "Collecting requests\n  Downloading requests-2.32.3-py3-none-any.whl.metadata (4.6 kB)\nCollecting idna<4,>=2.5 (from requests)\n  Using cached idna-3.7-py3-none-any.whl.metadata (9.9 kB)\nInstalling collected packages: idna, requests\nSuccessfully installed idna-3.7 requests-2.32.3\n";
        let out = collapse_build(pip, R).unwrap();
        assert!(out.starts_with("[crush: 4 build-progress lines not shown: 2 Collecting, 1 Downloading, 1 Using cached]\nInstalling collected"), "{out}");
        assert!(collapse_build(&out, R).is_none());
    }

    #[test]
    fn shared_closing_line() {
        let raw = format!("   Compiling a v1\n   Compiling b v1\n{}", cargo_block(6, None));
        let a = collapse_build(&raw, R).unwrap();
        let b = collapse_tests(&a, R).unwrap();
        assert_eq!(b.matches("raw output: R").count(), 1, "{b}");
        assert!(b.trim_end().ends_with(&closing(R)));
    }
}
