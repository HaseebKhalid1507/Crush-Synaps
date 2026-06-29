//! Columnar fold — the tool-aware compression engine.
//!
//! Fixed-width / whitespace-aligned tool output (`ls -lah`, `ps aux`) is dense
//! with waste: interior alignment padding, columns that are identical on every
//! row (owner=`root`…), and an implicit schema repeated structurally per line.
//!
//! Given a [`Schema`] — how many leading whitespace-delimited columns precede a
//! free-text tail — [`fold`] re-emits the block as: a one-line header declaring
//! the schema (with constant columns factored to `name=value`), then one compact
//! row per line carrying only the *varying* fixed values plus the verbatim tail.
//!
//! Lossless in meaning: [`unfold`] reconstructs every field exactly (only the
//! original whitespace padding — which carries no information to an LLM — is
//! dropped). The round-trip is proven in tests.

/// Describes the fixed columns that precede a free-text tail in a tool's output.
pub struct Schema {
    /// Names of the fixed, whitespace-delimited columns, in order.
    pub cols: &'static [&'static str],
    /// Name of the trailing free-text column (e.g. "name", "command").
    pub tail: &'static str,
    /// Whether to hold aside a leading `total <n>` line (coreutils `ls`).
    pub skip_total: bool,
}

const HEADER_TAG: &str = "@crush.cols";

/// Fold whitespace-aligned columnar text per `schema`. Returns `None` when the
/// input doesn't fit the schema (any line short on columns), has too few rows to
/// pay for the header, or wouldn't shrink. Never corrupts: ambiguity → `None`.
pub fn fold(text: &str, schema: &Schema) -> Option<String> {
    let n = schema.cols.len();
    if n == 0 {
        return None;
    }

    let lines = text.lines();
    let mut preamble: Option<&str> = None;

    // Optionally hold aside a leading `total <n>` line.
    let mut body: Vec<&str> = Vec::new();
    if schema.skip_total {
        // peek the first line without a Peekable borrow tangle
        let collected: Vec<&str> = lines.collect();
        let mut iter = collected.into_iter();
        if let Some(first) = iter.next() {
            if first.starts_with("total ") {
                preamble = Some(first);
            } else {
                body.push(first);
            }
        }
        body.extend(iter);
    } else {
        body.extend(lines);
    }

    // Parse every non-empty line into (fixed fields, tail). Any miss → bail.
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

    // Detect constant columns (same value on every row).
    let constant: Vec<Option<&str>> = (0..n)
        .map(|i| {
            let v0 = rows[0].0[i];
            if rows.iter().all(|(f, _)| f[i] == v0) {
                Some(v0)
            } else {
                None
            }
        })
        .collect();

    // ---- emit ----
    let mut out = String::with_capacity(text.len());
    if let Some(p) = preamble {
        out.push_str(p);
        out.push('\n');
    }
    // Header: declare schema; constants inline as name=value.
    out.push_str(HEADER_TAG);
    for (i, name) in schema.cols.iter().enumerate() {
        out.push(' ');
        match constant[i] {
            Some(v) => {
                out.push_str(name);
                out.push('=');
                out.push_str(v);
            }
            None => out.push_str(name),
        }
    }
    out.push(' ');
    out.push_str(schema.tail);
    out.push('\n');

    // Rows: only the non-constant fixed values, single-space-joined, + tail.
    for (fields, tail) in &rows {
        let mut first = true;
        for (i, val) in fields.iter().enumerate() {
            if constant[i].is_some() {
                continue;
            }
            if !first {
                out.push(' ');
            }
            out.push_str(val);
            first = false;
        }
        if !first {
            out.push(' ');
        }
        out.push_str(tail);
        out.push('\n');
    }
    out.pop(); // trailing newline

    if out.len() < text.len() {
        Some(out)
    } else {
        None
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
/// padding). Used to prove losslessness; not on the production path.
#[cfg(test)]
pub fn unfold(folded: &str) -> Option<String> {
    let mut lines = folded.lines();
    let mut preamble: Option<&str> = None;
    let mut header = lines.next()?;
    if !header.starts_with(HEADER_TAG) {
        // first line is the held-aside preamble (e.g. `total 1.9G`)
        preamble = Some(header);
        header = lines.next()?;
    }
    if !header.starts_with(HEADER_TAG) {
        return None;
    }

    // Parse schema: each token is `name` or `name=value`.
    let mut schema: Vec<(String, Option<String>)> = Vec::new();
    for tok in header[HEADER_TAG.len()..].split_whitespace() {
        match tok.split_once('=') {
            Some((_name, val)) => schema.push((tok.to_string(), Some(val.to_string()))),
            None => schema.push((tok.to_string(), None)),
        }
    }
    // Last schema entry is the tail column.
    let fixed = &schema[..schema.len() - 1];
    let varying = fixed.iter().filter(|(_, c)| c.is_none()).count();

    let mut out = String::new();
    if let Some(p) = preamble {
        out.push_str(p);
        out.push('\n');
    }
    for line in lines {
        let (vals, tail) = split_fixed(line, varying)?;
        let mut vi = 0;
        let mut cells: Vec<&str> = Vec::new();
        for (_, constant) in fixed {
            match constant {
                Some(v) => cells.push(v),
                None => {
                    cells.push(vals[vi]);
                    vi += 1;
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
        let mut s = String::from("total 1.9G\n");
        for i in 0..20 {
            s.push_str(&format!(
                "-rwxr-xr-x  1 root root    {:>3}K Jun 28 18:51 binary_{i}.sh\n",
                10 + i
            ));
        }
        s
    }

    #[test]
    fn folds_ls_output_and_shrinks() {
        let input = ls_sample();
        let out = fold(&input, &LS).expect("should fold");
        assert!(out.len() < input.len(), "{} vs {}", out.len(), input.len());
        assert!(out.contains("@crush.cols"));
        assert!(out.contains("owner=root"));
        assert!(out.contains("total 1.9G"));
    }

    #[test]
    fn fold_is_lossless_modulo_padding() {
        let input = ls_sample();
        let folded = fold(&input, &LS).unwrap();
        let restored = unfold(&folded).unwrap();
        // Compare token-by-token (padding is intentionally dropped).
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
        for _ in 0..5 {
            s.push_str("-rw-r--r-- 1 root root 100 Jan 1 00:00 my file name.txt\n");
        }
        let folded = fold(&s, &LS).unwrap();
        assert!(folded.contains("my file name.txt"));
        let restored = unfold(&folded).unwrap();
        assert!(restored.contains("my file name.txt"));
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
