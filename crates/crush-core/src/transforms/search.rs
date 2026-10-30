//! SEARCH: group grep / rg / git grep hits by file.
//!
//! ```text
//! src/a.rs:12:foo            src/a.rs
//! src/a.rs:13-ctx      ->    12:foo
//! src/b.rs:7:bar             13-ctx
//!                            src/b.rs
//!                            7:bar
//! ```
//!
//! That is ripgrep's `--heading` layout. The heading is the **full path exactly
//! as grep printed it** (no shared-prefix factoring: models drop factored
//! prefixes). Everything after `path:` / `path-` is copied byte for byte, so
//! `heading + sep + line` rebuilds every original line and a quoted `N:text`
//! still matches. Only *consecutive* lines of one file are grouped; nothing is
//! reordered. Lines that aren't hits (`--`, `Binary file … matches`, `-l`/`-c`
//! output, other commands' output) stay where they are, verbatim.
//!
//! Context lines (`path-N-text`) are only recognised against the path of a
//! neighbouring match line, which settles GNU grep's `-` ambiguity
//! (`foo-12-bar.rs-4-x`). Without `-n`, a group that mixes matches and context
//! can't be told apart after grouping, so it is left alone.

pub const NAME: &str = "search_group";

/// Extension-less file names accepted as grep paths.
const BARE_FILES: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "justfile",
    "Rakefile",
    "Gemfile",
    "Procfile",
    "LICENSE",
    "README",
    "CHANGELOG",
    "NOTICE",
    "AUTHORS",
    "COPYING",
    "Jenkinsfile",
    "Vagrantfile",
    "BUILD",
    "WORKSPACE",
    "CODEOWNERS",
    "Brewfile",
    "Pipfile",
    "Caddyfile",
];

/// Layout knobs (the default is what ships; the others exist for measurement).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Blank line between file groups (rg style).
    pub blank_between: bool,
    /// Heading written as `path:` instead of `path`.
    pub colon: bool,
    /// Group a file with a single line too (uniform layout).
    pub group_singles: bool,
}

/// Blank lines between adjacent groups measured free (`\n\n` is one token in
/// both families; eval/search_layout.py) and make the layout rg's own.
pub const DEFAULT_STYLE: Style = Style { blank_between: true, colon: false, group_singles: true };

/// Is `p` plausibly a path grep printed?
fn plausible_path(p: &str) -> bool {
    if p.is_empty() || p.len() > 512 {
        return false;
    }
    let b = p.as_bytes();
    if b[0] == b'-' || b[0] == b' ' {
        return false;
    }
    let mut letter = false;
    for &c in b {
        if c.is_ascii_alphabetic() {
            letter = true;
        }
        // no whitespace, no ':' (we split at the first one), no controls/quotes
        if c <= b' '
            || c == b':'
            || c == b'"'
            || c == b'\''
            || c == b'`'
            || c == 0x7f
            || c == b'<'
            || c == b'>'
            || c == b'|'
        {
            return false;
        }
    }
    if !letter {
        return false;
    }
    let base = p.rsplit('/').next().unwrap_or(p);
    p.contains('/') || BARE_FILES.contains(&base) || {
        match base.rsplit_once('.') {
            Some((stem, ext)) => {
                !ext.is_empty()
                    && ext.len() <= 12
                    && ext.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                    && (!stem.is_empty() || base.len() > 1)
            }
            None => false,
        }
    }
}

/// A match line: `path:N:text` or (no `-n`) `path:text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit<'a> {
    pub path: &'a str,
    /// Everything after `path:`, byte-exact.
    pub rest: &'a str,
    pub numbered: bool,
}

fn starts_numbered(rest: &str, sep: u8) -> bool {
    let b = rest.as_bytes();
    let d = b.iter().take_while(|c| c.is_ascii_digit()).count();
    (1..=10).contains(&d) && b.get(d) == Some(&sep)
}

/// Parse a `path:N:text` match line (numbered only: the sniffing form).
pub fn parse_hit(line: &str) -> Option<Hit<'_>> {
    let h = parse_match(line)?;
    h.numbered.then_some(h)
}

/// Parse a match line, numbered or not.
fn parse_match(line: &str) -> Option<Hit<'_>> {
    let colon = memchr::memchr(b':', line.as_bytes())?;
    let path = &line[..colon];
    if !plausible_path(path) {
        return None;
    }
    let rest = &line[colon + 1..];
    if !starts_numbered(rest, b':') {
        // `git grep PAT REV`: `REV:path:N:text`; the heading is `REV:path`
        if let Some(c2) = memchr::memchr(b':', rest.as_bytes()) {
            let (p2, r2) = (&rest[..c2], &rest[c2 + 1..]);
            if plausible_path(p2) && starts_numbered(r2, b':') {
                return Some(Hit { path: &line[..colon + 1 + c2], rest: r2, numbered: true });
            }
        }
    }
    Some(Hit { path, rest, numbered: starts_numbered(rest, b':') })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Other,
    Match {
        numbered: bool,
    },
    /// `path-N-text` (numbered) or `path-text`.
    Context {
        numbered: bool,
    },
    /// `--` group separator.
    Sep,
}

/// Group consecutive hits by file. `None` when there is nothing to group.
pub fn group(text: &str) -> Option<String> {
    group_with(text, DEFAULT_STYLE)
}

pub fn group_with(text: &str, style: Style) -> Option<String> {
    let (lines, trailing) = super::lines(text);
    let n = lines.len();
    if n < 2 {
        return None;
    }
    // pass 1: match lines and their path lengths
    let mut kind = vec![Kind::Other; n];
    let mut plen = vec![0usize; n];
    let mut any = false;
    for (i, l) in lines.iter().enumerate() {
        if *l == "--" {
            kind[i] = Kind::Sep;
        } else if let Some(h) = parse_match(l) {
            kind[i] = Kind::Match { numbered: h.numbered };
            plen[i] = h.path.len();
            any = true;
        }
    }
    if !any {
        return None;
    }
    // pass 2: context lines, against the nearest match path on either side
    // (scanning over other context candidates and `--`, never over Other)
    let mut prev_path: Option<&str> = None;
    let mut next_path: Vec<Option<usize>> = vec![None; n];
    {
        let mut nxt: Option<usize> = None;
        for i in (0..n).rev() {
            match kind[i] {
                Kind::Match { .. } => nxt = Some(i),
                Kind::Sep => {}
                _ => {
                    next_path[i] = nxt;
                    // an Other line that is not a context candidate breaks the chain;
                    // we can't know yet, so keep it and validate below
                }
            }
        }
    }
    for i in 0..n {
        match kind[i] {
            Kind::Match { .. } => prev_path = Some(&lines[i][..plen[i]]),
            Kind::Sep => {}
            Kind::Other => {
                let l = lines[i];
                let next = next_path[i].map(|j| &lines[j][..plen[j]]);
                let mut found = None;
                for cand in [prev_path, next].into_iter().flatten() {
                    if l.len() > cand.len() && l.starts_with(cand) && l.as_bytes()[cand.len()] == b'-' {
                        let rest = &l[cand.len() + 1..];
                        found = Some((cand.len(), starts_numbered(rest, b'-')));
                        break;
                    }
                }
                match found {
                    Some((len, numbered)) => {
                        kind[i] = Kind::Context { numbered };
                        plen[i] = len;
                    }
                    None => prev_path = None,
                }
            }
            Kind::Context { .. } => {}
        }
    }
    // pass 3: blocks of consecutive same-path lines (a `--` between two lines
    // of the same file stays inside the block)
    struct Block {
        start: usize,
        end: usize, // exclusive
        lines: usize,
    }
    let path_of = |i: usize| -> Option<&str> {
        match kind[i] {
            Kind::Match { .. } | Kind::Context { .. } => Some(&lines[i][..plen[i]]),
            _ => None,
        }
    };
    let mut blocks: Vec<Block> = Vec::new();
    let mut i = 0;
    while i < n {
        let Some(p) = path_of(i) else {
            i += 1;
            continue;
        };
        let start = i;
        let mut end = i + 1;
        let mut count = 1;
        let mut has_match = matches!(kind[i], Kind::Match { .. });
        let mut numbered_all = matches!(kind[i], Kind::Match { numbered: true } | Kind::Context { numbered: true });
        let mut has_ctx = matches!(kind[i], Kind::Context { .. });
        loop {
            // allow `--` runs inside if the next real line has the same path
            let mut j = end;
            while j < n && kind[j] == Kind::Sep {
                j += 1;
            }
            if j < n && path_of(j) == Some(p) {
                count += 1;
                has_match |= matches!(kind[j], Kind::Match { .. });
                has_ctx |= matches!(kind[j], Kind::Context { .. });
                numbered_all &= matches!(kind[j], Kind::Match { numbered: true } | Kind::Context { numbered: true });
                end = j + 1;
            } else {
                break;
            }
        }
        let min = if style.group_singles { 1 } else { 2 };
        // unnumbered matches mixed with context can't be told apart afterwards;
        // an unnumbered line that is empty or looks like a path or a hit would
        // read as a heading or a hit once grouped
        let ambiguous = !numbered_all
            && (start..end).any(|k| {
                kind[k] != Kind::Sep && {
                    let rest = &lines[k][plen[k] + 1..];
                    rest.is_empty() || rest == "--" || plausible_path(rest.trim_end()) || parse_match(rest).is_some()
                }
            });
        let ok = has_match && count >= min && (numbered_all || !has_ctx) && !ambiguous;
        if ok {
            blocks.push(Block { start, end, lines: count });
        }
        i = end;
    }
    // a lone unnumbered `path:text` line with no neighbours is too weak a signal
    // (kept when it sits right next to another block: part of the same grep)
    let keep: Vec<bool> = (0..blocks.len())
        .map(|k| {
            let b = &blocks[k];
            b.lines >= 2
                || matches!(kind[b.start], Kind::Match { numbered: true })
                || (k > 0 && blocks[k - 1].end == b.start)
                || (k + 1 < blocks.len() && blocks[k + 1].start == b.end)
        })
        .collect();
    let mut k = 0;
    blocks.retain(|_| {
        k += 1;
        keep[k - 1]
    });
    if blocks.is_empty() || !blocks.iter().any(|b| b.lines >= 2) {
        return None;
    }
    // emit
    let mut out = String::with_capacity(text.len());
    let mut bi = 0;
    let mut i = 0;
    let mut prev_end = usize::MAX;
    while i < n {
        if bi < blocks.len() && blocks[bi].start == i {
            let b = &blocks[bi];
            let p = &lines[i][..plen[i]];
            // only between two groups that touch (never next to other lines)
            if style.blank_between && prev_end == i {
                out.push('\n');
            }
            prev_end = b.end;
            out.push_str(p);
            if style.colon {
                out.push(':');
            }
            out.push('\n');
            for k in b.start..b.end {
                if kind[k] == Kind::Sep {
                    out.push_str("--");
                } else {
                    out.push_str(&lines[k][plen[k] + 1..]);
                }
                out.push('\n');
            }
            i = b.end;
            bi += 1;
        } else {
            out.push_str(lines[i]);
            out.push('\n');
            i += 1;
        }
    }
    if !trailing {
        out.pop();
        if out.ends_with('\n') {
            // last line empty and no final newline: ambiguous once grouped
            return None;
        }
    }
    Some(out)
}

/// Does `grouped` rebuild `original` exactly? A line of `original` must appear
/// either verbatim, or as `heading` + `:`/`-` + a line of `grouped` under that
/// heading. This is the byte-exactness check the gate runs on every SEARCH
/// result (and the eval and fuzzers run independently).
pub fn rebuilds(original: &str, grouped: &str) -> bool {
    let (x, tx) = super::lines(original);
    let (y, ty) = super::lines(grouped);
    if tx != ty {
        return false;
    }
    let mut j = 0;
    let mut heading: Option<&str> = None;
    let under = |l: &str, h: &str, rest: &str| {
        l.len() == h.len() + 1 + rest.len()
            && l.starts_with(h)
            && matches!(l.as_bytes()[h.len()], b':' | b'-')
            && l.ends_with(rest)
    };
    for l in x {
        if j < y.len() && y[j] == l {
            j += 1;
            if l != "--" {
                heading = None;
            }
            continue;
        }
        // a blank line crush put between two touching groups
        if heading.is_some() && j + 2 < y.len() && y[j].is_empty() && under(l, y[j + 1], y[j + 2]) {
            j += 1;
        }
        if j + 1 < y.len() && under(l, y[j], y[j + 1]) {
            heading = Some(y[j]);
            j += 2;
            continue;
        }
        match heading {
            Some(h) if j < y.len() && under(l, h, y[j]) => j += 1,
            _ => return false,
        }
    }
    j == y.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_numbered_hits() {
        let raw = "src/a.rs:12:foo\nsrc/a.rs:13:bar\nsrc/b.rs:7:baz\n";
        assert_eq!(group(raw).unwrap(), "src/a.rs\n12:foo\n13:bar\n\nsrc/b.rs\n7:baz\n");
        assert!(rebuilds(raw, &group(raw).unwrap()));
    }

    #[test]
    fn context_and_separators() {
        let raw = "x/foo-12-bar.rs-3-before\nx/foo-12-bar.rs:4:hit\nx/foo-12-bar.rs-5-after\n--\nx/foo-12-bar.rs:40:again\n--\ny.rs-1-c\ny.rs:2:d";
        assert_eq!(group(raw).unwrap(), "x/foo-12-bar.rs\n3-before\n4:hit\n5-after\n--\n40:again\n--\ny.rs\n1-c\n2:d");
    }

    #[test]
    fn leaves_non_hits_alone() {
        let raw = "---\nsrc/a.rs:1:x\nsrc/a.rs:2:y\nBinary file z.bin matches\nsrc/a.rs:9:w\n";
        assert_eq!(group(raw).unwrap(), "---\nsrc/a.rs\n1:x\n2:y\nBinary file z.bin matches\nsrc/a.rs\n9:w\n");
    }

    #[test]
    fn nothing_to_group() {
        // single-file grep (no path), -l, -c, timestamps
        assert_eq!(group("12:foo\n13:bar\n"), None);
        assert_eq!(group("src/a.rs\nsrc/b.rs\n"), None);
        assert_eq!(group("src/a.rs:3\nsrc/b.rs:0\n"), None);
        assert_eq!(group("12:34:56 up\n12:35:01 down\n"), None);
    }

    #[test]
    fn unnumbered_hits() {
        let raw = "src/a.rs:    let x: u8 = 1;\nsrc/a.rs:    y();\nsrc/b.rs:z\n";
        assert_eq!(group(raw).unwrap(), "src/a.rs\n    let x: u8 = 1;\n    y();\n\nsrc/b.rs\nz\n");
        // unnumbered matches mixed with context: left alone
        assert_eq!(group("a.rs:x\na.rs-y\nb.rs:z\nb.rs:w\n").unwrap(), "a.rs:x\na.rs-y\nb.rs\nz\nw\n");
    }

    #[test]
    fn rebuilds_exactly() {
        let raw = "a/b.rs:1:x\na/b.rs:2:y:z\nc.py:3:\n";
        let g = group(raw).unwrap();
        assert_eq!(g, "a/b.rs\n1:x\n2:y:z\n\nc.py\n3:\n");
        assert!(rebuilds(raw, &g));
        assert!(!rebuilds(raw, "a/b.rs\n1:x\n2:y:Z\n\nc.py\n3:\n"));
        assert!(!rebuilds(raw, "b.rs\n1:x\n2:y:z\n\nc.py\n3:\n"));
        assert!(!rebuilds(raw, "a/b.rs\n1:x\n\n2:y:z\n\nc.py\n3:\n"));
        // a real blank line between groups is kept, and no second one is added
        let gap = "a.rs:1:x\na.rs:2:x\n\nb.rs:2:y\n";
        assert_eq!(group(gap).unwrap(), "a.rs\n1:x\n2:x\n\nb.rs\n2:y\n");
        assert!(rebuilds(gap, &group(gap).unwrap()));
        let ctx = "x/a.rs-3-before\nx/a.rs:4:hit\n--\nx/a.rs:9:w\nother\n";
        assert!(rebuilds(ctx, &group(ctx).unwrap()));
    }

    #[test]
    fn fuzz_regressions() {
        let x = "cratch{\ncrates/m/src/lib.rs:\ncrates/m/src/lib.rs:";
        assert!(group(x).is_none_or(|g| rebuilds(x, &g)));
        let p = "/work/rg/crates/searcher/src/lines.rs";
        let x = format!("\n{p}:383\n{p}:{p}\n{p}");
        assert!(group(&x).is_none_or(|g| rebuilds(&x, &g)));
    }

    #[test]
    fn git_grep_revision_prefix() {
        let raw = "origin/dev:tests/a.rs:27:fn x() {}\norigin/dev:tests/a.rs:44:async fn y() {}\n";
        assert_eq!(group(raw).unwrap(), "origin/dev:tests/a.rs\n27:fn x() {}\n44:async fn y() {}\n");
    }

    #[test]
    fn idempotent_on_own_output() {
        let raw = "src/a.rs:12:foo\nsrc/a.rs:13:src/x.rs:1:bar\n";
        let g = group(raw).unwrap();
        assert_eq!(group(&g), None, "{g}");
    }
}
