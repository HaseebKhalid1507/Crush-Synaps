//! Columnar fold — the tool-aware compression engine.
//!
//! Fixed-width / whitespace-aligned tool output (`ls -lah`, `ps aux`) is dense
//! with waste: interior alignment padding, columns identical on every row
//! (owner=`root`…), and low-cardinality columns repeating a handful of long
//! values thousands of times (perms = `-rwxr-xr-x`…).
//!
//! Given a [`Schema`] — how many leading whitespace-delimited columns precede a
//! free-text tail — [`fold`] re-emits the block as: optional `@crush.dict` lines
//! (code tables for low-cardinality columns), a `@crush.cols` header (constants
//! factored to `name=value`), then one compact row per line carrying codes /
//! varying values plus the verbatim tail.
//!
//! Lossless in meaning: [`unfold`] reconstructs every field exactly (only the
//! original whitespace padding — no information to an LLM — is dropped).

#[cfg(test)]
use std::collections::BTreeMap;

/// Describes the fixed columns that precede a free-text tail in a tool's output.
pub struct Schema {
    /// Names of the fixed, whitespace-delimited columns, in order.
    pub cols: &'static [&'static str],
    /// Name of the trailing free-text column (e.g. "name", "command").
    pub tail: &'static str,
    /// Whether to hold aside a leading `total <n>` line (coreutils `ls`).
    pub skip_total: bool,
}

const COLS_TAG: &str = "@crush.cols";
const DICT_TAG: &str = "@crush.dict";
/// Single-char code alphabet (62 values). Columns with more distinct values than
/// this are left un-dictionaried.
const CODES: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Per-column encoding chosen by [`fold`].
enum Enc {
    /// Same value on every row — declared once, omitted from rows.
    Constant(String),
    /// Low-cardinality — each value coded to one char; table in a `@crush.dict`.
    Dict(Vec<(u8, String)>),
    /// Emitted verbatim per row.
    Plain,
}

/// Fold whitespace-aligned columnar text per `schema`. `None` when the input
/// doesn't fit (any line short on columns), has too few rows, or wouldn't
/// shrink. Never corrupts: ambiguity → `None`.
pub fn fold(text: &str, schema: &Schema) -> Option<String> {
    let n = schema.cols.len();
    if n == 0 {
        return None;
    }

    let (preamble, body) = split_preamble(text, schema.skip_total);

    let mut rows: Vec<(Vec<&str>, &str)> = Vec::new();
    for line in &body {
        if line.is_empty() {
            continue;
        }
        match split_fixed(line, n) {
            Some(r) => rows.push(r),
            None => return None,
        }
    }
    if rows.len() < 4 {
        return None;
    }

    // Choose an encoding per fixed column.
    let encs: Vec<Enc> = (0..n).map(|i| choose_enc(&rows, i)).collect();

    // ---- emit ----
    let mut out = String::with_capacity(text.len());
    if let Some(p) = preamble {
        out.push_str(p);
        out.push('\n');
    }
    // Dict tables first, so a reader sets them up before the schema/rows.
    for (i, enc) in encs.iter().enumerate() {
        if let Enc::Dict(map) = enc {
            out.push_str(DICT_TAG);
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
    out.push_str(COLS_TAG);
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
            let cell: Option<String> = match &encs[i] {
                Enc::Constant(_) => None,
                Enc::Dict(map) => Some(
                    (map.iter()
                        .find(|(_, v)| v == val)
                        .map(|(c, _)| *c)
                        .unwrap_or(b'?') as char)
                        .to_string(),
                ),
                Enc::Plain => Some((*val).to_string()),
            };
            if let Some(c) = cell {
                if !first {
                    out.push(' ');
                }
                out.push_str(&c);
                first = false;
            }
        }
        if !first {
            out.push(' ');
        }
        out.push_str(tail);
        out.push('\n');
    }
    out.pop();

    if out.len() < text.len() {
        Some(out)
    } else {
        None
    }
}

/// Decide how to encode column `i` across all rows.
fn choose_enc(rows: &[(Vec<&str>, &str)], i: usize) -> Enc {
    let v0 = rows[0].0[i];
    if rows.iter().all(|(f, _)| f[i] == v0) {
        return Enc::Constant(v0.to_string());
    }
    // Distinct values, first-seen order.
    let mut distinct: Vec<&str> = Vec::new();
    for (f, _) in rows {
        if !distinct.contains(&f[i]) {
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
    let map = distinct
        .iter()
        .enumerate()
        .map(|(idx, v)| (CODES[idx], v.to_string()))
        .collect();
    Enc::Dict(map)
}

fn split_preamble(text: &str, skip_total: bool) -> (Option<&str>, Vec<&str>) {
    let mut lines: Vec<&str> = text.lines().collect();
    if skip_total
        && lines
            .first()
            .map(|l| l.starts_with("total "))
            .unwrap_or(false)
    {
        let first = lines.remove(0);
        (Some(first), lines)
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

/// Inverse of [`fold`] — reconstruct the rows (modulo original whitespace
/// padding). Proves losslessness; not on the production path.
#[cfg(test)]
pub fn unfold(folded: &str) -> Option<String> {
    let mut preamble: Option<&str> = None;
    let mut dicts: BTreeMap<String, BTreeMap<char, String>> = BTreeMap::new();
    let mut header: Option<&str> = None;
    let mut rest_lines: Vec<&str> = Vec::new();

    for line in folded.lines() {
        if header.is_some() {
            rest_lines.push(line);
        } else if line.starts_with(COLS_TAG) {
            header = Some(line);
        } else if let Some(d) = line.strip_prefix(DICT_TAG) {
            let mut toks = d.split_whitespace();
            let col = toks.next()?.to_string();
            let mut map = BTreeMap::new();
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
    let schema: Vec<(String, Option<String>)> = header[COLS_TAG.len()..]
        .split_whitespace()
        .map(|tok| match tok.split_once('=') {
            Some((name, val)) => (name.to_string(), Some(val.to_string())),
            None => (tok.to_string(), None),
        })
        .collect();
    let fixed = &schema[..schema.len() - 1];
    let varying = fixed.iter().filter(|(_, c)| c.is_none()).count();

    let mut out = String::new();
    if let Some(p) = preamble {
        out.push_str(p);
        out.push('\n');
    }
    for line in rest_lines {
        let (vals, tail) = split_fixed(line, varying)?;
        let mut vi = 0;
        let mut cells: Vec<String> = Vec::new();
        for (name, constant) in fixed {
            match constant {
                Some(v) => cells.push(v.clone()),
                None => {
                    let raw = vals[vi];
                    vi += 1;
                    // Decode through the dictionary if this column has one.
                    let resolved = dicts
                        .get(name)
                        .and_then(|m| raw.chars().next().and_then(|c| m.get(&c)))
                        .cloned()
                        .unwrap_or_else(|| raw.to_string());
                    cells.push(resolved);
                }
            }
        }
        out.push_str(&cells.join(" "));
        out.push(' ');
        out.push_str(tail);
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

    #[test]
    fn folds_and_shrinks() {
        let input = ls_sample();
        let out = fold(&input, &LS).expect("should fold");
        assert!(out.len() < input.len());
        assert!(out.contains("@crush.cols"));
        assert!(out.contains("owner=root"));
        assert!(out.contains("total 1.9G"));
    }

    #[test]
    fn dictionaries_low_cardinality_columns() {
        let input = ls_sample();
        let out = fold(&input, &LS).unwrap();
        // perms has 3 distinct values across 40 rows → dictionaried.
        assert!(out.contains("@crush.dict perms"));
    }

    #[test]
    fn fold_is_lossless_modulo_padding() {
        let input = ls_sample();
        let folded = fold(&input, &LS).unwrap();
        let restored = unfold(&folded).unwrap();
        let norm = |s: &str| {
            s.lines()
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(norm(&restored), norm(&input));
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
    fn bails_on_too_few_rows() {
        let s = "-rwxr-xr-x 1 root root 100 Jan 1 00:00 a.sh\n\
                 -rwxr-xr-x 1 root root 100 Jan 1 00:00 b.sh\n";
        assert!(fold(s, &LS).is_none());
    }
}
