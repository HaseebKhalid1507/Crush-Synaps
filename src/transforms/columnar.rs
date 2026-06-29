//! Columnar fold — the tool-aware compression engine.
//!
//! Fixed-width / delimited tool output (`ls -lah`, `ps aux`, `git log
//! --pretty=…|…`) is dense with waste: interior padding, columns identical on
//! every row (author=`Haseeb Khalid`…), and low-cardinality columns repeating a
//! handful of long values thousands of times (perms, email…).
//!
//! Given a [`Schema`] — the fixed columns that precede a free-text tail, and the
//! [`Delim`] that separates them — [`fold`] re-emits the block as: an optional
//! `@crush/1.delim` line, `@crush/1.dict` code tables for low-cardinality
//! columns, a `@crush/1.cols` header (constants factored to `name=value`), then
//! one compact row per line.
//!
//! Lossless contract: [`unfold`] reconstructs every field value and the tail
//! exactly. Only interior whitespace *padding* (whitespace schemas) is dropped —
//! it carries no information to an LLM. Proven by the field-level round-trip test.

use crate::markers;
use std::collections::{HashMap, HashSet};

/// How a tool's columns are separated.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Delim {
    /// Runs of ASCII whitespace (`ls`, `ps`). Interior padding is normalised.
    Whitespace,
    /// A single literal character (`git log --pretty=…|…|…`). Values may contain
    /// spaces; they just cannot contain the delimiter (the tail absorbs the rest).
    Char(char),
}

impl Delim {
    /// The separator used in the emitted wire format (header/dict/rows).
    fn sep(self) -> char {
        match self {
            Delim::Whitespace => ' ',
            Delim::Char(c) => c,
        }
    }
}

/// Describes the fixed columns that precede a free-text tail in a tool's output.
pub struct Schema {
    /// Names of the fixed columns, in order.
    pub cols: &'static [&'static str],
    /// Name of the trailing free-text column (e.g. "name", "command", "subject").
    pub tail: &'static str,
    /// Column separator.
    pub delim: Delim,
    /// Whether to hold aside a leading `total <n>` line (coreutils `ls`).
    pub skip_total: bool,
    /// Whether to hold aside the first line as a column-name header (`ps aux`).
    pub skip_header: bool,
}

/// Single-char code alphabet (62 values). Columns with more distinct values than
/// this are left un-dictionaried.
const CODES: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Per-column encoding chosen by [`fold`].
enum Enc {
    /// Same value on every row — declared once, omitted from rows.
    Constant(String),
    /// Low-cardinality — each value coded to one char. `map`: code→value;
    /// `rev`: value→code (O(1), miss-proof row emit).
    Dict {
        map: Vec<(u8, String)>,
        rev: HashMap<String, u8>,
    },
    /// Emitted verbatim per row.
    Plain,
}

/// Fold delimited columnar text per `schema`. `None` when the input doesn't fit
/// (any line short on columns, empty mid-body line), has too few rows, or
/// wouldn't shrink. Never corrupts: any ambiguity → `None`.
#[must_use]
pub fn fold(text: &str, schema: &Schema) -> Option<String> {
    let n = schema.cols.len();
    if n == 0 {
        return None;
    }
    let sep = schema.delim.sep();

    let (preamble, body) = split_preamble(text, schema);

    let mut rows: Vec<(Vec<&str>, &str)> = Vec::new();
    for line in body {
        if line.is_empty() {
            return None; // can't represent/reconstruct an empty row
        }
        rows.push(split_fixed(line, n, schema.delim)?);
    }
    if rows.len() < 4 {
        return None;
    }

    let encs: Vec<Enc> = (0..n).map(|i| choose_enc(&rows, i, sep)).collect();

    // ---- emit ----
    let mut out = String::with_capacity(text.len());
    if let Some(p) = preamble {
        out.push_str(p);
        out.push('\n');
    }
    // Declare a non-default delimiter so `unfold` can split rows correctly.
    if let Delim::Char(c) = schema.delim {
        out.push_str(markers::DELIM);
        out.push(' ');
        out.push(c);
        out.push('\n');
    }
    // Dict tables first, separated by the schema delimiter.
    for (i, enc) in encs.iter().enumerate() {
        if let Enc::Dict { map, .. } = enc {
            out.push_str(markers::DICT);
            out.push(sep);
            out.push_str(schema.cols[i]);
            for (code, val) in map {
                out.push(sep);
                out.push(*code as char);
                out.push('=');
                out.push_str(val);
            }
            out.push('\n');
        }
    }
    // Schema header.
    out.push_str(markers::COLS);
    for (i, name) in schema.cols.iter().enumerate() {
        out.push(sep);
        match &encs[i] {
            Enc::Constant(v) => {
                out.push_str(name);
                out.push('=');
                out.push_str(v);
            }
            _ => out.push_str(name),
        }
    }
    out.push(sep);
    out.push_str(schema.tail);
    out.push('\n');

    // Rows: codes / values for non-constant columns, then the verbatim tail.
    for (fields, tail) in &rows {
        let mut first = true;
        for (i, val) in fields.iter().enumerate() {
            match &encs[i] {
                Enc::Constant(_) => {}
                Enc::Dict { rev, .. } => {
                    let code = *rev.get(*val)?; // built from these rows; miss → bail
                    if !first {
                        out.push(sep);
                    }
                    out.push(code as char);
                    first = false;
                }
                Enc::Plain => {
                    if !first {
                        out.push(sep);
                    }
                    out.push_str(val);
                    first = false;
                }
            }
        }
        if !tail.is_empty() {
            if !first {
                out.push(sep);
            }
            out.push_str(tail);
        }
        out.push('\n');
    }
    out.pop();

    if out.len() < text.len() {
        Some(out)
    } else {
        None
    }
}

/// Decide how to encode column `i`. A value containing the separator can't be
/// factored/dictionaried (it would break the delimited header/dict line), so
/// such columns stay Plain.
fn choose_enc(rows: &[(Vec<&str>, &str)], i: usize, sep: char) -> Enc {
    let token_unsafe = rows.iter().any(|(f, _)| f[i].contains([sep, '\n']));

    let v0 = rows[0].0[i];
    if rows.iter().all(|(f, _)| f[i] == v0) {
        return if token_unsafe {
            Enc::Plain
        } else {
            Enc::Constant(v0.to_string())
        };
    }
    if token_unsafe {
        return Enc::Plain;
    }

    let mut seen: HashSet<&str> = HashSet::new();
    let mut distinct: Vec<&str> = Vec::new();
    for (f, _) in rows {
        if seen.insert(f[i]) {
            distinct.push(f[i]);
        }
    }
    if distinct.len() > CODES.len() {
        return Enc::Plain;
    }
    let plain_bytes: usize = rows.iter().map(|(f, _)| f[i].len()).sum();
    let coded_bytes = rows.len();
    let dict_header: usize = distinct.iter().map(|v| v.len() + 3).sum();
    if coded_bytes + dict_header >= plain_bytes {
        return Enc::Plain;
    }
    let map: Vec<(u8, String)> = distinct
        .iter()
        .enumerate()
        .map(|(idx, v)| (CODES[idx], (*v).to_string()))
        .collect();
    let rev: HashMap<String, u8> = map.iter().map(|(c, v)| (v.clone(), *c)).collect();
    Enc::Dict { map, rev }
}

fn split_preamble<'a>(text: &'a str, schema: &Schema) -> (Option<&'a str>, Vec<&'a str>) {
    let lines: Vec<&str> = text.lines().collect();
    let take = (schema.skip_total
        && lines
            .first()
            .map(|l| l.starts_with("total "))
            .unwrap_or(false))
        || (schema.skip_header && !lines.is_empty());
    if take {
        let (head, rest) = lines.split_at(1);
        (Some(head[0]), rest.to_vec())
    } else {
        (None, lines)
    }
}

/// Split a line into the first `n` fields plus the verbatim tail. `None` if
/// fewer than `n` fields exist.
fn split_fixed(line: &str, n: usize, delim: Delim) -> Option<(Vec<&str>, &str)> {
    match delim {
        Delim::Whitespace => {
            let mut fields = Vec::with_capacity(n);
            let mut rest = line;
            for _ in 0..n {
                rest = rest.trim_start();
                let end = rest.find(char::is_whitespace)?;
                fields.push(&rest[..end]);
                rest = &rest[end..];
            }
            Some((fields, rest.trim_start()))
        }
        Delim::Char(c) => {
            let mut fields = Vec::with_capacity(n);
            let mut rest = line;
            for _ in 0..n {
                let idx = rest.find(c)?;
                fields.push(&rest[..idx]);
                rest = &rest[idx + c.len_utf8()..];
            }
            Some((fields, rest)) // tail kept verbatim (may contain the delimiter)
        }
    }
}

/// Public inverse of [`fold`] — reconstruct field values + tail exactly (padding
/// normalised for whitespace schemas). Powers `crush --unfold` and the
/// round-trip test that proves the lossless contract.
#[must_use]
pub fn unfold(folded: &str) -> Option<String> {
    let mut preamble: Option<&str> = None;
    let mut dicts: HashMap<String, HashMap<char, String>> = HashMap::new();
    let mut header: Option<&str> = None;
    let mut rest_lines: Vec<&str> = Vec::new();
    let mut sep: char = ' '; // default = whitespace schema

    // First pass: pick up an explicit delimiter declaration.
    for line in folded.lines() {
        if let Some(d) = line.strip_prefix(markers::DELIM) {
            sep = d.trim().chars().next()?;
        }
    }

    let split = |s: &str| -> Vec<String> {
        if sep == ' ' {
            s.split_whitespace().map(str::to_string).collect()
        } else {
            // Marker lines emit `<marker><sep>tok<sep>tok…`, so the content here
            // begins with the separator — drop that one structural empty token,
            // but keep any genuinely-empty fields that follow.
            let s = s.strip_prefix(sep).unwrap_or(s);
            s.split(sep).map(str::to_string).collect()
        }
    };

    for line in folded.lines() {
        if line.starts_with(markers::DELIM) {
            continue;
        }
        if header.is_some() {
            rest_lines.push(line);
        } else if line.starts_with(markers::COLS) {
            header = Some(line);
        } else if let Some(d) = line.strip_prefix(markers::DICT) {
            let toks = split(d);
            let mut it = toks.into_iter();
            let col = it.next()?;
            let mut map = HashMap::new();
            for t in it {
                let (code, val) = t.split_once('=')?;
                map.insert(code.chars().next()?, val.to_string());
            }
            dicts.insert(col, map);
        } else {
            preamble = Some(line);
        }
    }

    let header = header?;
    let schema: Vec<(String, Option<String>)> = split(&header[markers::COLS.len()..])
        .into_iter()
        .map(|tok| match tok.split_once('=') {
            Some((name, val)) => (name.to_string(), Some(val.to_string())),
            None => (tok, None),
        })
        .collect();
    let fixed_len = schema.len().checked_sub(1)?;
    let fixed = &schema[..fixed_len];
    let varying = fixed.iter().filter(|(_, c)| c.is_none()).count();

    let mut out = String::new();
    if let Some(p) = preamble {
        out.push_str(p);
        out.push('\n');
    }
    let join_sep = sep.to_string();
    for line in rest_lines {
        let (vals, tail) = split_fixed(
            line,
            varying,
            if sep == ' ' {
                Delim::Whitespace
            } else {
                Delim::Char(sep)
            },
        )?;
        let mut vi = 0;
        let mut cells: Vec<&str> = Vec::with_capacity(fixed.len());
        for (name, constant) in fixed {
            match constant {
                Some(v) => cells.push(v),
                None => {
                    let raw = vals[vi];
                    vi += 1;
                    let resolved = dicts
                        .get(name)
                        .and_then(|m| raw.chars().next().and_then(|c| m.get(&c)))
                        .map(String::as_str)
                        .unwrap_or(raw);
                    cells.push(resolved);
                }
            }
        }
        out.push_str(&cells.join(&join_sep));
        if !tail.is_empty() {
            if !cells.is_empty() {
                out.push(sep);
            }
            out.push_str(tail);
        }
        out.push('\n');
    }
    out.pop();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LS: Schema = Schema {
        cols: &[
            "perms", "links", "owner", "group", "size", "month", "day", "time",
        ],
        tail: "name",
        delim: Delim::Whitespace,
        skip_total: true,
        skip_header: false,
    };

    const GITLOG: Schema = Schema {
        cols: &["sha", "author", "email", "date"],
        tail: "subject",
        delim: Delim::Char('|'),
        skip_total: false,
        skip_header: false,
    };

    fn ls_sample() -> String {
        let perms = ["-rwxr-xr-x", "lrwxrwxrwx", "drwxr-xr-x"];
        let mut s = String::from("total 1.9G\n");
        for i in 0..40 {
            s.push_str(&format!(
                "{}  1 root root    {:>3}K Jun 28 18:51 binary_{i}.sh\n",
                perms[i % perms.len()],
                10 + i
            ));
        }
        s
    }

    fn gitlog_sample() -> String {
        let mut s = String::new();
        for i in 0..30 {
            s.push_str(&format!(
                "{:040x}|Haseeb Khalid|haseeb@example.com|Mon Jun 29 04:{:02}:01 2026|fix: thing number {i}\n",
                i, i % 60
            ));
        }
        s
    }

    fn assert_field_lossless(orig: &str, restored: &str, n: usize, d: Delim) {
        let o: Vec<&str> = orig.lines().filter(|l| !l.starts_with("total ")).collect();
        let r: Vec<&str> = restored
            .lines()
            .filter(|l| !l.starts_with("total "))
            .collect();
        assert_eq!(o.len(), r.len(), "row count differs");
        for (a, b) in o.iter().zip(&r) {
            assert_eq!(
                split_fixed(a, n, d),
                split_fixed(b, n, d),
                "row mismatch:\n  {a}\n  {b}"
            );
        }
    }

    #[test]
    fn folds_ls_and_shrinks() {
        let input = ls_sample();
        let out = fold(&input, &LS).expect("should fold");
        assert!(out.len() < input.len());
        assert!(out.contains("@crush/1.cols"));
        assert!(out.contains("owner=root"));
    }

    #[test]
    fn ls_is_field_lossless() {
        let input = ls_sample();
        let restored = unfold(&fold(&input, &LS).unwrap()).unwrap();
        assert_field_lossless(&input, &restored, LS.cols.len(), Delim::Whitespace);
    }

    #[test]
    fn folds_pipe_delimited_git_log_with_spaced_constant() {
        let input = gitlog_sample();
        let out = fold(&input, &GITLOG).expect("should fold");
        assert!(out.len() < input.len());
        assert!(out.contains("@crush/1.delim |"));
        // author "Haseeb Khalid" (has a space) factored as a constant safely.
        assert!(out.contains("author=Haseeb Khalid"));
    }

    #[test]
    fn git_log_is_field_lossless() {
        let input = gitlog_sample();
        let restored = unfold(&fold(&input, &GITLOG).unwrap()).unwrap();
        assert_field_lossless(&input, &restored, GITLOG.cols.len(), Delim::Char('|'));
    }

    #[test]
    fn git_log_preserves_pipes_in_the_subject_tail() {
        let mut s = String::new();
        for i in 0..6 {
            s.push_str(&format!(
                "{:040x}|Haseeb Khalid|h@x.com|Mon Jun 29 2026|fix: a | b | c {i}\n",
                i
            ));
        }
        let restored = unfold(&fold(&s, &GITLOG).unwrap()).unwrap();
        assert!(restored.contains("fix: a | b | c 0"));
    }

    #[test]
    fn bails_when_a_line_is_short_on_columns() {
        let mut s = String::from("-rwxr-xr-x 1 root root 100 Jan 1 00:00 ok.sh\n");
        s.push_str("malformed line\n");
        for _ in 0..5 {
            s.push_str("-rwxr-xr-x 1 root root 100 Jan 1 00:00 ok.sh\n");
        }
        assert!(fold(&s, &LS).is_none());
    }

    #[test]
    fn bails_on_empty_mid_body_line() {
        let mut s = String::from("-rwxr-xr-x 1 root root 100 Jan 1 00:00 a.sh\n\n");
        for _ in 0..5 {
            s.push_str("-rwxr-xr-x 1 root root 100 Jan 1 00:00 b.sh\n");
        }
        assert!(fold(&s, &LS).is_none());
    }

    #[test]
    fn unfold_guards_empty_header() {
        assert!(unfold("@crush/1.cols\nA B C\n").is_none());
    }

    // ---- property fuzzing (std-only, deterministic seed) ----

    /// Tiny xorshift RNG — no dev-dependency, reproducible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn fuzz_roundtrip_whitespace_and_pipe() {
        // Token pools mixing low- and high-cardinality columns to exercise
        // Constant / Dict / Plain encodings.
        let perms = ["-rwxr-xr-x", "drwxr-xr-x", "lrwxrwxrwx", "-rw-r--r--"];
        let owners = ["root"]; // constant-factor target
        let ws = Schema {
            cols: &["perms", "links", "owner", "size", "month", "day", "time"],
            tail: "name",
            delim: Delim::Whitespace,
            skip_total: false,
            skip_header: false,
        };
        let pipe = Schema {
            cols: &["sha", "author", "email", "date"],
            tail: "subject",
            delim: Delim::Char('|'),
            skip_total: false,
            skip_header: false,
        };
        let mut rng = Rng(0x1234_5678_9abc_def0);
        for _ in 0..3000 {
            let rows = 4 + rng.below(40);
            // whitespace input
            let mut ws_in = String::new();
            for r in 0..rows {
                ws_in.push_str(&format!(
                    "{} {} {} {}K Jun {} {:02}:00 file_{}_name\n",
                    perms[rng.below(perms.len())],
                    1 + rng.below(9),
                    owners[0],
                    10 + rng.below(900),
                    1 + rng.below(28),
                    rng.below(24),
                    r
                ));
            }
            if let Some(f) = fold(&ws_in, &ws) {
                let back = unfold(&f).expect("unfold ws");
                assert_field_lossless(&ws_in, &back, ws.cols.len(), Delim::Whitespace);
            }
            // pipe input — author can carry spaces (the delimiter feature)
            let authors = ["Haseeb Khalid", "Jane Q. Public"];
            let mut p_in = String::new();
            for r in 0..rows {
                p_in.push_str(&format!(
                    "{:040x}|{}|x@y.com|Mon Jun {} 2026|fix: thing | with pipe {}\n",
                    r,
                    authors[rng.below(authors.len())],
                    1 + rng.below(28),
                    r
                ));
            }
            if let Some(f) = fold(&p_in, &pipe) {
                let back = unfold(&f).expect("unfold pipe");
                assert_field_lossless(&p_in, &back, pipe.cols.len(), Delim::Char('|'));
            }
        }
    }

    #[test]
    fn fuzz_arbitrary_input_never_panics_never_enlarges() {
        let schemas = [
            Schema {
                cols: &["a", "b", "c"],
                tail: "t",
                delim: Delim::Whitespace,
                skip_total: false,
                skip_header: false,
            },
            Schema {
                cols: &["a", "b"],
                tail: "t",
                delim: Delim::Char('|'),
                skip_total: true,
                skip_header: false,
            },
        ];
        let bytes = b"abc \t|=\n:/.-_0123XYZ\xe2\x9c\x93 ";
        let mut rng = Rng(0xdead_beef_cafe_babe);
        for _ in 0..5000 {
            let len = rng.below(400);
            let mut s = String::new();
            for _ in 0..len {
                let c = bytes[rng.below(bytes.len())];
                s.push(c as char); // ascii-ish; the ✓ byte handled below
            }
            // occasionally inject real multi-byte + crush markers
            if rng.below(5) == 0 {
                s.push_str("✓ @crush/1.cols x\n");
            }
            for sc in &schemas {
                if let Some(f) = fold(&s, sc) {
                    assert!(
                        f.len() < s.len(),
                        "fold enlarged: {} -> {}",
                        s.len(),
                        f.len()
                    );
                    // unfold must not panic on its own output
                    let _ = unfold(&f);
                }
            }
            // unfold on arbitrary input must never panic
            let _ = unfold(&s);
        }
    }
}
