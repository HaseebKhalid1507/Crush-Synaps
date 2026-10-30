//! SEARCH: an over-long hit line keeps the text around its match.
//!
//! ```text
//! dist/app.min.js:1:<14,000 chars>   ->   dist/app.min.js:1:[crush: 9,120 chars cut]…ctx… matchedWord …ctx…[crush: 4,610 chars cut]
//!                                         ...
//!                                         [crush: 1 line over 1,024 bytes cut to the text around the match; full lines in raw output: <ref>]
//! ```
//!
//! Every line keeps its own `path:N:` (or `N:`) prefix, verbatim: nothing is
//! moved into a heading. What is kept of a cut line is one contiguous slice of
//! it, byte for byte, so an `old_string` copied from the slice still matches
//! the file. Each `[crush: N chars cut]` says exactly how many characters are
//! missing on that side, which the gate checks against the input
//! ([`verify`]). Lossy, so it only runs when the host keeps the raw output.
//!
//! Where the match is: the grep / rg / git grep pattern is read from the
//! command (`-e`, `--regexp`, or the first operand; `-i`, `-F`, `-E`/`-P`,
//! BRE `\|`), reduced to the literal runs any match must contain. A line is
//! only cut when one of them is found in it, so a window is never placed
//! blind; lines with no literal hit (context lines, `-v`, regexes without a
//! usable literal) stay whole.

use super::{group_digits, lines};
use crate::shell;

pub const NAME: &str = "search_elide";

/// A line's text (after its `path:N:` prefix) must be longer than this.
pub const MIN_LINE: usize = 1024;
/// Bytes kept before the match and after it.
const BEFORE: usize = 160;
const AFTER: usize = 240;
/// A side shorter than this isn't cut (the marker would cost about as much).
const MIN_CUT: usize = 64;
/// Literals shorter than this only count when they are the whole pattern.
const MIN_LITERAL: usize = 3;

/// The literal runs a match must contain, and how to compare them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patterns {
    pub literals: Vec<String>,
    pub ignore_case: bool,
}

const GREP_PROGS: &[&str] = &["grep", "egrep", "fgrep", "rg", "ag", "ack", "zgrep"];

/// Short flags that take a value (the rest of the cluster or the next word).
fn short_takes_value(prog: &str, c: char) -> bool {
    match prog {
        "rg" => matches!(c, 'A' | 'B' | 'C' | 'm' | 'e' | 'f' | 'g' | 't' | 'T' | 'M' | 'j' | 'E' | 'r' | 'd'),
        "ag" => matches!(c, 'A' | 'B' | 'C' | 'm' | 'G' | 'g' | 'p'),
        _ => matches!(c, 'A' | 'B' | 'C' | 'm' | 'e' | 'f' | 'd' | 'D'),
    }
}

/// Long flags that take the next word as their value when written without `=`.
fn long_takes_value(name: &str) -> bool {
    matches!(
        name,
        "regexp"
            | "file"
            | "after-context"
            | "before-context"
            | "context"
            | "max-count"
            | "include"
            | "exclude"
            | "exclude-dir"
            | "exclude-from"
            | "label"
            | "devices"
            | "directories"
            | "binary-files"
            | "glob"
            | "iglob"
            | "type"
            | "type-not"
            | "max-columns"
            | "threads"
            | "encoding"
            | "replace"
            | "max-depth"
            | "max-filesize"
            | "sort"
            | "sortr"
            | "pre"
            | "pre-glob"
            | "path-separator"
            | "ignore-file"
            | "type-add"
            | "engine"
            | "dfa-size-limit"
            | "regex-size-limit"
            | "field-match-separator"
            | "field-context-separator"
            | "context-separator"
            | "max-columns-preview"
    )
}

#[derive(Debug, Default)]
struct GrepCall {
    patterns: Vec<String>,
    ignore_case: bool,
    smart_case: bool,
    fixed: bool,
    extended: bool,
    invert: bool,
}

fn parse_grep(prog: &str, args: &[String]) -> GrepCall {
    let mut g = GrepCall {
        extended: matches!(prog, "egrep" | "rg" | "ag" | "ack"),
        fixed: prog == "fgrep",
        smart_case: prog == "ag",
        ..Default::default()
    };
    let mut positional: Vec<&str> = Vec::new();
    let mut explicit = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        i += 1;
        if a == "--" {
            positional.extend(args[i..].iter().map(String::as_str));
            break;
        }
        if let Some(long) = a.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "ignore-case" => g.ignore_case = true,
                "no-ignore-case" | "case-sensitive" => {
                    g.ignore_case = false;
                    g.smart_case = false;
                }
                "smart-case" => g.smart_case = true,
                "fixed-strings" => g.fixed = true,
                "extended-regexp" | "perl-regexp" => g.extended = true,
                "basic-regexp" => g.extended = false,
                "invert-match" => g.invert = true,
                _ => {}
            }
            if long_takes_value(name) {
                let v = match val {
                    Some(v) => Some(v),
                    None if i < args.len() => {
                        i += 1;
                        Some(args[i - 1].clone())
                    }
                    None => None,
                };
                if name == "regexp" {
                    if let Some(v) = v {
                        g.patterns.push(v);
                        explicit = true;
                    }
                } else if name == "file" {
                    explicit = true; // patterns from a file: unknown
                }
            }
            continue;
        }
        if let Some(cluster) = a.strip_prefix('-').filter(|c| !c.is_empty()) {
            let chars: Vec<char> = cluster.chars().collect();
            let mut k = 0;
            while k < chars.len() {
                let c = chars[k];
                k += 1;
                match c {
                    'i' | 'y' => g.ignore_case = true,
                    'S' if prog == "rg" => g.smart_case = true,
                    's' if prog == "rg" => {
                        g.ignore_case = false;
                        g.smart_case = false;
                    }
                    'F' => g.fixed = true,
                    'E' | 'P' if prog != "rg" => g.extended = true,
                    'G' if prog != "ag" => g.extended = false,
                    'v' => g.invert = true,
                    _ => {}
                }
                if short_takes_value(prog, c) {
                    let rest: String = chars[k..].iter().collect();
                    let v = if !rest.is_empty() {
                        Some(rest)
                    } else if i < args.len() {
                        i += 1;
                        Some(args[i - 1].clone())
                    } else {
                        None
                    };
                    if c == 'e' {
                        if let Some(v) = v {
                            g.patterns.push(v);
                            explicit = true;
                        }
                    } else if c == 'f' {
                        explicit = true;
                    }
                    break;
                }
            }
            continue;
        }
        positional.push(a);
    }
    if !explicit {
        if let Some(p) = positional.first() {
            g.patterns.push((*p).to_string());
        }
    }
    g
}

/// Literal runs of one regex alternative branch set.
fn regex_literals(p: &str, extended: bool) -> Vec<String> {
    let b: Vec<char> = p.chars().collect();
    let mut out: Vec<Vec<String>> = vec![Vec::new()]; // per alternative: its runs
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<Vec<String>>| {
        if !cur.is_empty() {
            out.last_mut().expect("alt").push(std::mem::take(cur));
        }
    };
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        match c {
            '\\' => {
                let Some(&n) = b.get(i) else { break };
                i += 1;
                if n == '|' && !extended {
                    flush(&mut cur, &mut out);
                    out.push(Vec::new());
                } else if !extended && matches!(n, '(' | ')' | '{' | '}') {
                    if n == '{' {
                        cur.pop();
                        while i < b.len() && b[i] != '}' {
                            i += 1;
                        }
                        i += 1;
                    }
                    flush(&mut cur, &mut out);
                } else if !extended && matches!(n, '+' | '?') {
                    cur.pop();
                    flush(&mut cur, &mut out);
                } else if n.is_ascii_alphanumeric() || n == '<' || n == '>' || n == '`' || n == '\'' {
                    // classes, anchors, back-references, escapes we don't model
                    flush(&mut cur, &mut out);
                } else {
                    cur.push(n);
                }
            }
            '|' if extended => {
                flush(&mut cur, &mut out);
                out.push(Vec::new());
            }
            '*' => {
                cur.pop();
                flush(&mut cur, &mut out);
            }
            '+' | '?' if extended => {
                cur.pop();
                flush(&mut cur, &mut out);
            }
            '{' if extended => {
                cur.pop();
                flush(&mut cur, &mut out);
                while i < b.len() && b[i] != '}' {
                    i += 1;
                }
                i += 1;
            }
            '(' | ')' if extended => {
                // `(?i)` and other inline groups: no literal inside the marker
                if c == '(' && b.get(i) == Some(&'?') {
                    while i < b.len() && b[i] != ')' && b[i] != ':' {
                        i += 1;
                    }
                    i += 1;
                }
                flush(&mut cur, &mut out);
            }
            '[' => {
                flush(&mut cur, &mut out);
                // skip the class: `[]...]`, `[^]...]`
                if b.get(i) == Some(&'^') {
                    i += 1;
                }
                if b.get(i) == Some(&']') {
                    i += 1;
                }
                while i < b.len() && b[i] != ']' {
                    i += 1;
                }
                i += 1;
            }
            '.' | '^' | '$' => flush(&mut cur, &mut out),
            _ => cur.push(c),
        }
    }
    flush(&mut cur, &mut out);
    // a branch with no usable literal can match anywhere: we can't place a
    // window for it, but the others still find *a* match
    out.into_iter()
        .filter_map(|runs| {
            let whole = runs.len() == 1;
            runs.into_iter()
                .filter(|r| !r.trim().is_empty() && (r.len() >= MIN_LITERAL || (whole && r.len() >= 2)))
                .reduce(|a, b| if b.len() > a.len() { b } else { a })
        })
        .collect()
}

/// Patterns from every grep-like stage of `command` (none when the output
/// isn't plain grep hits: `-v`, `-f FILE`, unknown programs).
pub fn patterns(command: Option<&str>) -> Patterns {
    let Some(cmd) = command else { return Patterns::default() };
    let mut pats = Patterns::default();
    for seg in shell::parse(cmd) {
        for st in &seg.stages {
            let prog = st.prog.as_str();
            let args: &[String] = if prog == "git" && st.sub() == "grep" {
                let k = st.args.iter().position(|a| a == "grep").map_or(0, |k| k + 1);
                &st.args[k..]
            } else if GREP_PROGS.contains(&prog) {
                &st.args
            } else {
                continue;
            };
            let prog = if prog == "git" { "grep" } else { prog };
            let g = parse_grep(prog, args);
            if g.invert {
                continue;
            }
            for p in &g.patterns {
                let ci = g.ignore_case || (g.smart_case && !p.chars().any(char::is_uppercase)) || p.contains("(?i)");
                let lits = if g.fixed {
                    p.split('\n').filter(|s| !s.is_empty()).map(str::to_string).collect()
                } else {
                    regex_literals(p, g.extended)
                };
                for l in lits {
                    // one case-insensitive stage folds them all (a superset)
                    pats.ignore_case |= ci;
                    if !pats.literals.contains(&l) {
                        pats.literals.push(l);
                    }
                }
            }
        }
    }
    pats
}

/// Earliest occurrence of any literal in `hay`: (start, end). ASCII case
/// folding keeps byte offsets, so a folded copy is searched in linear time.
fn find_first(hay: &str, pats: &Patterns) -> Option<(usize, usize)> {
    let folded: Option<Vec<u8>> = pats.ignore_case.then(|| hay.bytes().map(|c| c.to_ascii_lowercase()).collect());
    let mut best: Option<(usize, usize)> = None;
    for lit in &pats.literals {
        let at = match &folded {
            Some(h) => memchr::memmem::find(h, lit.to_ascii_lowercase().as_bytes()),
            None => memchr::memmem::find(hay.as_bytes(), lit.as_bytes()),
        };
        if let Some(s) = at {
            if best.map_or(true, |(b, _)| s < b) {
                best = Some((s, s + lit.len()));
            }
        }
    }
    best
}

/// Length of a grep prefix: `path:N:`, `path-N-`, `N:`, `N-`, `path:`.
fn prefix_len(line: &str) -> usize {
    if let Some(h) = super::search::parse_hit(line) {
        return line.len() - h.rest.len() + {
            let d = h.rest.bytes().take_while(u8::is_ascii_digit).count();
            d + 1
        };
    }
    let b = line.as_bytes();
    let d = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if (1..=10).contains(&d) && matches!(b.get(d), Some(b':' | b'-')) {
        return d + 1;
    }
    // `path-N-text` context or unnumbered `path:text`: the first `:` or
    // `-N-` after a path-like run; kept verbatim either way
    if let Some(c) = memchr::memchr(b':', b) {
        if c < 300 && !b[..c].iter().any(|x| x.is_ascii_whitespace()) && b[..c].contains(&b'/') {
            return c + 1;
        }
    }
    0
}

fn floor_cb(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_cb(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn cut_marker(chars: usize) -> String {
    format!("[crush: {} chars cut]", group_digits(chars))
}

fn end_note(n: usize, raw_ref: &str) -> String {
    format!(
        "[crush: {} line{} over {} bytes cut to the text around the match; full lines in raw output: {raw_ref}]",
        group_digits(n),
        if n == 1 { "" } else { "s" },
        group_digits(MIN_LINE)
    )
}

/// The cut form of one line, if it is over-long and its match is found.
fn elide_line(line: &str, pats: &Patterns) -> Option<String> {
    let p = prefix_len(line);
    let text = &line[p..];
    if text.len() <= MIN_LINE {
        return None;
    }
    let (ms, me) = find_first(text, pats)?;
    let mut a = floor_cb(text, ms.saturating_sub(BEFORE));
    let mut b = ceil_cb(text, (me + AFTER).min(text.len()));
    // no recognised `path:N:` prefix: never cut the start of the line (it may
    // be a prefix we don't parse, e.g. a context line's `path-N-`)
    if a < MIN_CUT || p == 0 {
        a = 0;
    }
    if text.len() - b < MIN_CUT {
        b = text.len();
    }
    if a == 0 && b == text.len() {
        return None;
    }
    let mut out = String::with_capacity(p + (b - a) + 64);
    out.push_str(&line[..p]);
    if a > 0 {
        out.push_str(&cut_marker(text[..a].chars().count()));
    }
    out.push_str(&text[a..b]);
    if b < text.len() {
        out.push_str(&cut_marker(text[b..].chars().count()));
    }
    Some(out)
}

/// Cut over-long hit lines to the text around their match. `None` when no
/// line qualifies.
pub fn elide(text: &str, command: Option<&str>, raw_ref: &str) -> Option<String> {
    if text.len() <= MIN_LINE || memchr::memmem::find(text.as_bytes(), b"[crush:").is_some() {
        return None;
    }
    let pats = patterns(command);
    if pats.literals.is_empty() {
        return None;
    }
    let (ls, trailing) = lines(text);
    let mut out = String::with_capacity(text.len() / 2);
    let mut n = 0;
    for (k, l) in ls.iter().enumerate() {
        if k > 0 {
            out.push('\n');
        }
        match elide_line(l, &pats) {
            Some(c) => {
                n += 1;
                out.push_str(&c);
            }
            None => out.push_str(l),
        }
    }
    if n == 0 {
        return None;
    }
    // the note is a line of its own after every line (an empty last line too)
    out.push('\n');
    out.push_str(&end_note(n, raw_ref));
    if trailing {
        out.push('\n');
    }
    Some(out)
}

/// Parse a cut line back: (prefix + kept slice, chars cut before, chars cut after).
fn parse_cut(l: &str) -> Option<(String, String, usize, usize)> {
    const OPEN: &str = "[crush: ";
    const CLOSE: &str = " chars cut]";
    let num = |s: &str| -> Option<usize> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit() || c == b',') {
            return None;
        }
        let v: usize = s.replace(',', "").parse().ok()?;
        (group_digits(v) == s).then_some(v)
    };
    // leading marker: the first `[crush: N chars cut]`
    let (head, rest, before) = match l.find(OPEN) {
        Some(i) => {
            let after = &l[i + OPEN.len()..];
            let j = after.find(CLOSE)?;
            let n = num(&after[..j])?;
            let rest = &after[j + CLOSE.len()..];
            (l[..i].to_string(), rest, Some(n))
        }
        None => return None,
    };
    // a trailing marker ends the line
    let (kept, after_n) = match rest.rfind(OPEN) {
        Some(i) if rest.ends_with(CLOSE) => {
            let n = num(&rest[i + OPEN.len()..rest.len() - CLOSE.len()])?;
            (rest[..i].to_string(), Some(n))
        }
        _ => (rest.to_string(), None),
    };
    // `head` holds the prefix; if `before` belongs to a trailing-only cut the
    // first marker is the trailing one (no leading cut)
    if kept.is_empty() && after_n.is_none() && rest.is_empty() {
        // only one marker, at the very end: it is a trailing cut
        return Some((head, String::new(), 0, before?));
    }
    Some((head, kept, before?, after_n.unwrap_or(0)))
}

/// Does `out` follow from `src` by cutting lines exactly as the markers say?
/// Every line is verbatim or `prefix + [cut] + slice + [cut]` of its source
/// line, with the stated character counts; one end note, nothing else.
pub fn verify(src: &str, out: &str) -> bool {
    let (x, tx) = lines(src);
    let (y, ty) = lines(out);
    if tx != ty || y.len() != x.len() + 1 {
        return false;
    }
    let mut cut = 0;
    for (a, b) in x.iter().zip(&y) {
        if a == b {
            continue;
        }
        if !cut_matches(a, b) {
            return false;
        }
        cut += 1;
    }
    let last = y[y.len() - 1];
    cut > 0 && last.starts_with(&end_note(cut, "")[..end_note(cut, "").len() - 1]) && last.ends_with(']')
}

fn cut_matches(src: &str, out: &str) -> bool {
    // try every split of `out` into prefix / slice consistent with src
    let Some((head, kept, before, after)) = parse_cut(out) else { return false };
    if !src.starts_with(&head) {
        return false;
    }
    let text = &src[head.len()..];
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let total = chars.len();
    if before + after > total {
        return false;
    }
    let a = if before == total { text.len() } else { chars.get(before).map_or(text.len(), |c| c.0) };
    let b = if total - after == total { text.len() } else { chars[total - after].0 };
    a <= b && text[a..b] == kept && (before > 0 || after > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_line(prefix: &str, n: usize, needle: &str, at: usize) -> String {
        let filler: String = "abcdefghij;".chars().cycle().take(n).collect();
        let (l, r) = filler.split_at(at);
        format!("{prefix}{l}{needle}{r}")
    }

    #[test]
    fn patterns_from_commands() {
        let p = patterns(Some("grep -rn 'fn main' src | head"));
        assert_eq!(p.literals, vec!["fn main"]);
        assert!(!p.ignore_case);
        let p = patterns(Some("cd x && grep -n -i \"foo\\|bar.*baz\" a.rs"));
        assert_eq!(p.literals, vec!["foo", "bar"]);
        assert!(p.ignore_case);
        let p = patterns(Some("rg -n -e 'Foo(Bar|Qux)' -g '*.rs' --type rust"));
        assert_eq!(p.literals, vec!["Foo", "Qux"]);
        let p = patterns(Some("grep -rnE 'x+y{2}|tok[a-z]en' -A3 ."));
        assert_eq!(p.literals, vec!["tok"]);
        let p = patterns(Some("grep -rn -A 3 -m5 TODO --include='*.py' ."));
        assert_eq!(p.literals, vec!["TODO"]);
        let p = patterns(Some("git grep -n 'spill_bytes' -- crates"));
        assert_eq!(p.literals, vec!["spill_bytes"]);
        assert_eq!(patterns(Some("grep -v foo x")).literals, Vec::<String>::new());
        assert_eq!(patterns(Some("ls -la")).literals, Vec::<String>::new());
        assert_eq!(patterns(Some("grep -F 'a.b*c' f")).literals, vec!["a.b*c"]);
        assert_eq!(patterns(Some("grep 'a\\.b' f")).literals, vec!["a.b"]);
        assert!(patterns(Some("rg 'abc' -S")).ignore_case);
        assert!(!patterns(Some("rg 'Abc' -S")).ignore_case);
    }

    #[test]
    fn cuts_around_the_match_and_verifies() {
        let l = long_line("dist/app.min.js:1:", 6000, "NEEDLE", 3000);
        let raw = format!("src/a.rs:3:short NEEDLE\n{l}\nsrc/b.rs:9:x NEEDLE\n");
        let out = elide(&raw, Some("grep -rn NEEDLE ."), "R").unwrap();
        let ls: Vec<&str> = out.lines().collect();
        assert_eq!(ls[0], "src/a.rs:3:short NEEDLE");
        assert!(ls[1].starts_with("dist/app.min.js:1:[crush: 2,840 chars cut]"), "{}", ls[1]);
        assert!(ls[1].contains("NEEDLE"));
        assert!(ls[1].ends_with("[crush: 2,760 chars cut]"), "{}", ls[1]);
        assert_eq!(ls[2], "src/b.rs:9:x NEEDLE");
        assert_eq!(
            ls[3],
            "[crush: 1 line over 1,024 bytes cut to the text around the match; full lines in raw output: R]"
        );
        assert!(out.ends_with("R]\n"));
        assert!(verify(&raw, &out));
        // tampering is caught
        assert!(!verify(&raw, &out.replace("2,840", "2,841")));
        assert!(!verify(&raw, &out.replace("NEEDLE", "NEEDLX")));
        assert!(!verify(&raw.replace("short", "shirt"), &out));
    }

    #[test]
    fn match_near_an_end_keeps_that_end() {
        let l = long_line("12:", 3000, "needle", 10);
        let out = elide(&l, Some("grep -n needle f"), "R").unwrap();
        let first = out.lines().next().unwrap();
        assert!(first.starts_with("12:abcdefghij"), "{first}");
        assert!(first.ends_with(" chars cut]"));
        assert!(verify(&l, &out));
        let l = long_line("12:", 3000, "needle", 2990);
        let out = elide(&l, Some("grep -n needle f"), "R").unwrap();
        assert!(out.lines().next().unwrap().starts_with("12:[crush: "));
        assert!(verify(&l, &out));
    }

    #[test]
    fn leaves_alone() {
        let l = long_line("a.rs:1:", 3000, "needle", 1500);
        // no pattern known, pattern not on the line, short lines, inverted grep
        assert_eq!(elide(&l, None, "R"), None);
        assert_eq!(elide(&l, Some("grep -n other f"), "R"), None);
        assert_eq!(elide("a.rs:1:needle\n", Some("grep -n needle f"), "R"), None);
        assert_eq!(elide(&l, Some("grep -v needle f"), "R"), None);
    }

    #[test]
    fn multibyte_and_case() {
        let filler = "é🦀 ".repeat(800);
        let l = format!("7:{filler}NeEdLe{filler}");
        let out = elide(&l, Some("grep -ni needle f"), "R").unwrap();
        assert!(out.contains("NeEdLe"));
        assert!(verify(&l, &out));
    }
}
