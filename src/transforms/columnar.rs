//! Columnar fold — the tool-aware compression engine.
//!
//! Fixed-width / whitespace-aligned tool output (`ls -lah`, `ps aux`) is dense
//! with waste: interior alignment padding, columns identical on every row
//! (owner=`root`…), and low-cardinality columns repeating a handful of long
//! values thousands of times (perms = `-rwxr-xr-x`…).
//!
//! Given a [`Schema`] — how many leading whitespace-delimited columns precede a
//! free-text tail — [`fold`] re-emits the block as: optional `@crush/1.dict`
//! lines (code tables for low-cardinality columns), a `@crush/1.cols` header
//! (constants factored to `name=value`), then one compact row per line carrying
//! codes / varying values plus the verbatim tail.
//!
//! Lossless contract: [`unfold`] reconstructs every field value and the tail
//! **exactly**. The only thing not preserved is the original whitespace *padding*
//! between columns (collapsed to single spaces) — which carries no information to
//! an LLM. Proven by the field-level round-trip test.

use crate::markers;
use std::collections::{HashMap, HashSet};

/// Describes the fixed columns that precede a free-text tail in a tool's output.
pub struct Schema {
    /// Names of the fixed, whitespace-delimited columns, in order.
    pub cols: &'static [&'static str],
    /// Name of the trailing free-text column (e.g. "name", "command").
    pub tail: &'static str,
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
    /// Low-cardinality — each value coded to one char. `map`: code→value (for the
    /// dict line); `rev`: value→code (for O(1), miss-proof row emit).
    Dict {
        map: Vec<(u8, String)>,
        rev: HashMap<String, u8>,
    },
    /// Emitted verbatim per row.
    Plain,
}

/// Fold whitespace-aligned columnar text per `schema`. `None` when the input
/// doesn't fit (any line short on columns, empty mid-body line), has too few
/// rows, or wouldn't shrink. Never corrupts: any ambiguity → `None`.
#[must_use]
pub fn fold(text: &str, schema: &Schema) -> Option<String> {
    let n = schema.cols.len();
    if n == 0 {
        return None;
    }

    let (preamble, body) = split_preamble(text, schema);

    let mut rows: Vec<(Vec<&str>, &str)> = Vec::new();
    for line in body {
        // An empty line mid-body can't be represented as a row and can't be
        // reconstructed — bail rather than silently drop it.
        if line.is_empty() {
            return None;
        }
        rows.push(split_fixed(line, n)?);
    }
    if rows.len() < 4 {
        return None;
    }

    let encs: Vec<Enc> = (0..n).map(|i| choose_enc(&rows, i)).collect();

    // ---- emit ----
    let mut out = String::with_capacity(text.len());
    if let Some(p) = preamble {
        out.push_str(p);
        out.push('\n');
    }
    // Dict tables first, so a reader sets them up before the schema/rows.
    for (i, enc) in encs.iter().enumerate() {
        if let Enc::Dict { map, .. } = enc {
            out.push_str(markers::DICT);
            out.push(' ');
            out.push_str(schema.cols[i]);
            for (code, val) in map {
                out.push(' ');
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
        out.push(' ');
        match &encs[i] {
            Enc::Constant(v) => {
                out.push_str(name);
                out.push('=');
                out.push_str(v);
            }
            _ => out.push_str(name),
        }
    }
    out.push(' ');
    out.push_str(schema.tail);
    out.push('\n');

    // Rows: codes / values for non-constant columns, then the verbatim tail.
    for (fields, tail) in &rows {
        let mut first = true;
        for (i, val) in fields.iter().enumerate() {
            match &encs[i] {
                Enc::Constant(_) => {}
                Enc::Dict { rev, .. } => {
                    // Built from these exact rows, so a miss is impossible — but
                    // if it ever happened, bail rather than emit corruption.
                    let code = *rev.get(*val)?;
                    if !first {
                        out.push(' ');
                    }
                    out.push(code as char);
                    first = false;
                }
                Enc::Plain => {
                    if !first {
                        out.push(' ');
                    }
                    out.push_str(val);
                    first = false;
                }
            }
        }
        if !tail.is_empty() {
            if !first {
                out.push(' ');
            }
            out.push_str(tail);
        }
        out.push('\n');
    }
    out.pop(); // drop the final newline; callers re-add via the pipeline

    if out.len() < text.len() {
        Some(out)
    } else {
        None
    }
}

/// Decide how to encode column `i` across all rows.
fn choose_enc(rows: &[(Vec<&str>, &str)], i: usize) -> Enc {
    // Token-safety: a value containing whitespace or '=' can't be round-tripped
    // through the space-delimited dict/header format. Such columns stay Plain.
    let token_unsafe = rows
        .iter()
        .any(|(f, _)| f[i].bytes().any(|b| b == b' ' || b == b'\t' || b == b'='));

    let v0 = rows[0].0[i];
    if rows.iter().all(|(f, _)| f[i] == v0) {
        if token_unsafe {
            return Enc::Plain; // can't put a space/'=' value in the @crush.cols header
        }
        return Enc::Constant(v0.to_string());
    }
    if token_unsafe {
        return Enc::Plain;
    }

    // Distinct values, first-seen order, O(n) via a HashSet guard.
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
    // Would a 1-char-code dictionary actually shrink this column?
    let plain_bytes: usize = rows.iter().map(|(f, _)| f[i].len()).sum();
    let coded_bytes = rows.len(); // one code char per row
    let dict_header: usize = distinct.iter().map(|v| v.len() + 3).sum(); // " c=val"
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
        && lines.first().map(|l| l.starts_with("total ")).unwrap_or(false))
        || (schema.skip_header && !lines.is_empty());
    if take {
        // Avoid Vec::remove(0)'s O(n) shift — split the slice instead.
        let (head, rest) = lines.split_at(1);
        (Some(head[0]), rest.to_vec())
    } else {
        (None, lines)
    }
}

/// Split a line into the first `n` whitespace-delimited fields plus the verbatim
/// remainder (the free-text tail). `None` if fewer than `n` fields exist.
fn split_fixed(line: &str, n: usize) -> Option<(Vec<&str>, &str)> {
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

/// Public inverse of [`fold`] — reconstruct field values + tail exactly (padding
/// normalised). Available for diagnostics, a `crush --unfold` decoder, and the
/// round-trip test that proves the lossless contract.
#[must_use]
pub fn unfold(folded: &str) -> Option<String> {
    let mut preamble: Option<&str> = None;
    let mut dicts: HashMap<String, HashMap<char, String>> = HashMap::new();
    let mut header: Option<&str> = None;
    let mut rest_lines: Vec<&str> = Vec::new();

    for line in folded.lines() {
        if header.is_some() {
            rest_lines.push(line);
        } else if line.starts_with(markers::COLS) {
            header = Some(line);
        } else if let Some(d) = line.strip_prefix(markers::DICT) {
            let mut toks = d.split_whitespace();
            let col = toks.next()?.to_string();
            let mut map = HashMap::new();
            for t in toks {
                let (code, val) = t.split_once('=')?;
                map.insert(code.chars().next()?, val.to_string());
            }
            dicts.insert(col, map);
        } else {
            preamble = Some(line); // e.g. `total 1.9G`
        }
    }

    let header = header?;
    // Parse schema: each token is `name` or `name=value`.
    let schema: Vec<(String, Option<String>)> = header[markers::COLS.len()..]
        .split_whitespace()
        .map(|tok| match tok.split_once('=') {
            Some((name, val)) => (name.to_string(), Some(val.to_string())),
            None => (tok.to_string(), None),
        })
        .collect();
    // Need at least the tail column. Guard against an empty header (no underflow).
    let fixed_len = schema.len().checked_sub(1)?;
    let fixed = &schema[..fixed_len];
    let varying = fixed.iter().filter(|(_, c)| c.is_none()).count();

    let mut out = String::new();
    if let Some(p) = preamble {
        out.push_str(p);
        out.push('\n');
    }
    for line in rest_lines {
        let (vals, tail) = split_fixed(line, varying)?;
        let mut vi = 0;
        let mut cells: Vec<&str> = Vec::with_capacity(fixed.len());
        for (name, constant) in fixed {
            match constant {
                Some(v) => cells.push(v),
                None => {
                    let raw = vals[vi];
                    vi += 1;
                    // Decode through the dictionary if this column has one.
                    let resolved = dicts
                        .get(name)
                        .and_then(|m| raw.chars().next().and_then(|c| m.get(&c)))
                        .map(String::as_str)
                        .unwrap_or(raw);
                    cells.push(resolved);
                }
            }
        }
        out.push_str(&cells.join(" "));
        if !tail.is_empty() {
            if !cells.is_empty() {
                out.push(' ');
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
        skip_total: true,
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

    /// Field-level losslessness: every line's (fixed fields, tail) must match.
    /// Splitting both sides with the schema arity makes this immune to padding
    /// AND to the trailing-space / empty-tail bugs a whitespace-normaliser hides.
    fn assert_field_lossless(original: &str, restored: &str, n: usize) {
        let orig_rows: Vec<&str> = original.lines().filter(|l| !l.starts_with("total ")).collect();
        let rest_rows: Vec<&str> = restored.lines().filter(|l| !l.starts_with("total ")).collect();
        assert_eq!(orig_rows.len(), rest_rows.len(), "row count differs");
        for (o, r) in orig_rows.iter().zip(&rest_rows) {
            assert_eq!(split_fixed(o, n), split_fixed(r, n), "row mismatch:\n  {o}\n  {r}");
        }
    }

    #[test]
    fn folds_and_shrinks() {
        let input = ls_sample();
        let out = fold(&input, &LS).expect("should fold");
        assert!(out.len() < input.len());
        assert!(out.contains("@crush/1.cols"));
        assert!(out.contains("owner=root"));
        assert!(out.contains("total 1.9G"));
    }

    #[test]
    fn dictionaries_low_cardinality_columns() {
        let out = fold(&ls_sample(), &LS).unwrap();
        assert!(out.contains("@crush/1.dict perms"));
    }

    #[test]
    fn fold_is_field_lossless() {
        let input = ls_sample();
        let folded = fold(&input, &LS).unwrap();
        let restored = unfold(&folded).unwrap();
        assert_field_lossless(&input, &restored, LS.cols.len());
    }

    #[test]
    fn preserves_filenames_with_spaces_in_the_tail() {
        let mut s = String::from("total 4\n");
        for i in 0..6 {
            s.push_str(&format!(
                "-rw-r--r-- 1 root root 100 Jan 1 00:00 my file {i}.txt\n"
            ));
        }
        let folded = fold(&s, &LS).unwrap();
        let restored = unfold(&folded).unwrap();
        assert!(restored.contains("my file 0.txt"));
        assert_field_lossless(&s, &restored, LS.cols.len());
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
    fn bails_on_too_few_rows() {
        let s = "-rwxr-xr-x 1 root root 100 Jan 1 00:00 a.sh\n\
                 -rwxr-xr-x 1 root root 100 Jan 1 00:00 b.sh\n";
        assert!(fold(s, &LS).is_none());
    }

    #[test]
    fn unfold_guards_empty_header() {
        assert!(unfold("@crush/1.cols\nA B C\n").is_none());
    }
}
