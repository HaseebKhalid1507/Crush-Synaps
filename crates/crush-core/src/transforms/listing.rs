//! LISTING: `ls -l` owner and group, said once in words when (nearly) every
//! row carries the same pair.
//!
//! ```text
//! total 24                                   total 24
//! drwxr-xr-x  3 haseeb haseeb   80 Oct 14 .  [crush: 3 of the 4 entries below are owned by user haseeb, group haseeb; their rows omit those two columns (ls -go layout); the other entry shows its own owner and group]
//! drwxrwxrwt 61 root   root   141K Oct 14 .. drwxr-xr-x  3   80 Oct 14 .
//! -rw-r--r--  1 haseeb haseeb  152 Oct 14 a  drwxrwxrwt 61 root   root   141K Oct 14 ..
//! -rw-r--r--  1 haseeb haseeb   56 Oct 14 b  -rw-r--r--  1  152 Oct 14 a
//!                                            -rw-r--r--  1   56 Oct 14 b
//! ```
//!
//! A factored row is exactly `ls -go`'s layout (mode, links, size, date,
//! name), so every value a model reads off a row is still on that row and
//! verbatim; only the pair that is the same everywhere moves into the note,
//! written in words with the row count (no codes, no legend to carry). Rows
//! with a different owner or group (typically `..`) stay byte for byte.
//!
//! Lossless: [`restore`] puts ` owner group` back after the links column of
//! every factored row, and the gate checks `restore(out) == input` on every
//! result. A block is only factored when that round trip is unambiguous (a
//! kept row can't be mistaken for a factored one) and the pair sits in the
//! row as exactly ` owner group` (single spaces, so the note says it all).

use super::{group_digits, lines};
use crate::shell;

pub const NAME: &str = "ls_columns";

/// Fewest rows in a block worth a note.
const MIN_ROWS: usize = 4;
/// Share of a block's rows that must carry the factored pair.
const MIN_SHARE_PCT: usize = 80;
/// Larger listings pass through. A listing that big is over any spill
/// threshold (Synaps previews results over 12 KiB), where factoring measured
/// no gain after spill, while the cost grows with every row; the bound keeps
/// the 256 KiB worst case of `compress` well under 1 ms. (No LISTING result in
/// the 30-day corpus is over 32 KiB.)
pub const MAX_INPUT: usize = 64 * 1024;

/// The owner/group of one `ls -l` row and where they sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row<'a> {
    /// End of the links digits: the pair is inserted / removed right here.
    links_end: usize,
    owner: &'a str,
    group: &'a str,
    /// End of the group name (exclusive).
    group_end: usize,
}

fn is_mode(b: &[u8]) -> Option<usize> {
    if b.len() < 10 || !matches!(b[0], b'-' | b'd' | b'l' | b'c' | b'b' | b'p' | b's') {
        return None;
    }
    if !b[1..10].iter().all(|c| matches!(c, b'r' | b'w' | b'x' | b's' | b'S' | b't' | b'T' | b'-')) {
        return None;
    }
    Some(if matches!(b.get(10), Some(b'.' | b'+' | b'@')) { 11 } else { 10 })
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'$'))
        && !s.starts_with('-')
}

/// A size column: `239`, `4.0K`, `1.1M` (`-h`), or a device's `8,` (`major, minor`).
fn size_ok(s: &str) -> bool {
    let b = s.as_bytes();
    let d = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if d == 0 || d > 20 {
        return false;
    }
    match &b[d..] {
        [] | [b','] => true,
        [b'.', rest @ ..] => {
            let f = rest.iter().take_while(|c| c.is_ascii_digit()).count();
            f > 0 && matches!(&rest[f..], [] | [b'K' | b'M' | b'G' | b'T' | b'P' | b'E' | b'k'])
        }
        [b'K' | b'M' | b'G' | b'T' | b'P' | b'E' | b'k'] => true,
        _ => false,
    }
}

/// Parse `mode links owner group size rest` (rest non-empty). Hot path:
/// plain index loops (this runs on every line of a listing).
fn parse_row(line: &str) -> Option<Row<'_>> {
    let b = line.as_bytes();
    let n = b.len();
    let mut i = is_mode(b)?;
    let spaces = |mut i: usize| {
        let s = i;
        while i < n && b[i] == b' ' {
            i += 1;
        }
        (i > s).then_some(i)
    };
    let word = |mut i: usize| {
        let s = i;
        while i < n && b[i] != b' ' && b[i] != b'\t' {
            i += 1;
        }
        (s, i)
    };
    i = spaces(i)?;
    let s = i;
    while i < n && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == s || i - s > 9 {
        return None;
    }
    let links_end = i;
    let (os, oe) = word(spaces(i)?);
    let (gs, ge) = word(spaces(oe)?);
    let (ss, se) = word(spaces(ge)?);
    let rest = spaces(se)?;
    if rest >= n {
        return None;
    }
    let (owner, group, size) = (&line[os..oe], &line[gs..ge], &line[ss..se]);
    if !name_ok(owner) || !name_ok(group) || !size_ok(size) {
        return None;
    }
    // `ls -o`: the "group" would be the size; a numeric group only with a
    // numeric owner (`ls -n`)
    let numeric = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
    if numeric(group) && !numeric(owner) {
        return None;
    }
    Some(Row { links_end, owner, group, group_end: ge })
}

/// The note, stated as a fact about the entries (Sonnet read the first
/// wording, "...; those columns are left out", as "owner unknown": see
/// eval/RESULTS.md, Phase 2A).
fn note(n: usize, total: usize, owner: &str, group: &str) -> String {
    if n == total {
        format!(
            "[crush: all {} entries below are owned by user {owner}, group {group}; their rows omit those two columns (ls -go layout)]",
            group_digits(total)
        )
    } else {
        let other = total - n;
        format!(
            "[crush: {} of the {} entries below are owned by user {owner}, group {group}; their rows omit those two columns (ls -go layout); the other {} {} its own owner and group]",
            group_digits(n),
            group_digits(total),
            if other == 1 { "entry".to_string() } else { format!("{} entries", group_digits(other)) },
            if other == 1 { "shows" } else { "show" }
        )
    }
}

/// `line` with ` owner group` put back after its links column, if it then
/// parses as a row with exactly that pair.
fn reinsert(line: &str, owner: &str, group: &str) -> Option<String> {
    let b = line.as_bytes();
    let mut i = is_mode(b)?;
    let s = b[i..].iter().take_while(|&&c| c == b' ').count();
    if s == 0 {
        return None;
    }
    i += s;
    let d = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if d == 0 {
        return None;
    }
    i += d;
    let full = format!("{} {owner} {group}{}", &line[..i], &line[i..]);
    let r = parse_row(&full)?;
    (r.owner == owner && r.group == group && r.links_end == i).then_some(full)
}

/// Does the command ask `ls` to leave out the owner or group (`-o`, `-g`,
/// `-G`, `--no-group`)? Then what looks like the group is something else.
fn drops_columns(command: Option<&str>) -> bool {
    let Some(cmd) = command else { return false };
    shell::parse(cmd).iter().flat_map(|s| s.stages.iter()).any(|st| {
        matches!(st.prog.as_str(), "ls" | "dir" | "vdir")
            && (st.has_flag('o', &["no-group"]) || st.has_flag('g', &[]) || st.has_flag('G', &[]))
    })
}

/// One line of the input as `factor` sees it: where it is, and for a row
/// where its pair sits (offsets within the line; compact so a 256 KiB
/// listing's index stays small).
#[derive(Clone, Copy)]
struct Slim {
    start: u32,
    end: u32,
    /// 0 when the line is not a row.
    links_end: u16,
    owner: (u16, u16),
    group: (u16, u16),
}

impl Slim {
    fn is_row(&self) -> bool {
        self.links_end != 0
    }
    fn line<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start as usize..self.end as usize]
    }
    fn pair<'a>(&self, text: &'a str) -> (&'a str, &'a str) {
        let l = self.line(text);
        (&l[self.owner.0 as usize..self.owner.1 as usize], &l[self.group.0 as usize..self.group.1 as usize])
    }
}

/// Factor owner/group out of `ls -l` blocks. `None` when nothing changed.
pub fn factor(text: &str, command: Option<&str>) -> Option<String> {
    // our own notes in the input would make the result ambiguous to restore
    // (the gate never lets marker-like input this far; this keeps the
    // transform self-consistent on its own)
    if text.len() > MAX_INPUT || drops_columns(command) || memchr::memmem::find(text.as_bytes(), b"[crush:").is_some() {
        return None;
    }
    let trailing = text.ends_with('\n');
    let body = if trailing { &text[..text.len() - 1] } else { text };
    let mut idx: Vec<Slim> = Vec::with_capacity(body.len() / 48 + 1);
    let mut pos = 0usize;
    loop {
        let end = memchr::memchr(b'\n', &body.as_bytes()[pos..]).map_or(body.len(), |k| pos + k);
        let l = &body[pos..end];
        let mut sl = Slim { start: pos as u32, end: end as u32, links_end: 0, owner: (0, 0), group: (0, 0) };
        if l.len() < u16::MAX as usize {
            if let Some(r) = parse_row(l) {
                let off = |x: &str| (x.as_ptr() as usize - l.as_ptr() as usize) as u16;
                sl.links_end = r.links_end as u16;
                sl.owner = (off(r.owner), off(r.owner) + r.owner.len() as u16);
                sl.group = (off(r.group), r.group_end as u16);
            }
        }
        idx.push(sl);
        if end == body.len() {
            break;
        }
        pos = end + 1;
    }
    let n = idx.len();
    // (start, end, owner, group, count) of every block worth factoring
    let mut plan: Vec<(usize, usize, &str, &str, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if !idx[i].is_row() {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && idx[i].is_row() {
            i += 1;
        }
        let block = &idx[start..i];
        let total = block.len();
        if total < MIN_ROWS {
            continue;
        }
        // the pair on >= 80% of the rows, if any, is the block's majority:
        // Boyer-Moore vote finds the only candidate in one pass, a second
        // pass counts it (linear, no hashing, deterministic)
        let mut cand: (&str, &str) = ("", "");
        let mut votes = 0usize;
        for r in block {
            let k = r.pair(body);
            if votes == 0 {
                cand = k;
                votes = 1;
            } else if k == cand {
                votes += 1;
            } else {
                votes -= 1;
            }
        }
        let (owner, group) = cand;
        let count = block.iter().filter(|r| r.pair(body) == cand).count();
        let span_len = owner.len() + group.len() + 2;
        let ok = count * 100 >= total * MIN_SHARE_PCT
            && count >= MIN_ROWS
            // worth it: the bytes left out pay for the note. (Per-block
            // bytes, not the estimator: it prices rare names like `haseeb`
            // at ~1 token where count_tokens says 3-4. The gate still judges
            // the whole result in estimated tokens, so this only picks blocks.)
            && count * span_len > note(count, total, owner, group).len() + 1
            && block.iter().all(|r| {
                let l = r.line(body).as_bytes();
                let le = r.links_end as usize;
                if r.pair(body) == cand {
                    // exactly ` owner group` (single spaces): the note then
                    // says everything that was left out. Putting it back after
                    // the links digits rebuilds this very line.
                    r.group.1 as usize - le == span_len && l[le] == b' ' && l[le + 1 + owner.len()] == b' '
                } else {
                    // a row that keeps its own pair must not read as factored
                    reinsert(r.line(body), owner, group).is_none()
                }
            });
        if ok {
            plan.push((start, i, owner, group, count));
        }
    }
    if plan.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut k = 0;
    let mut p = plan.iter().peekable();
    while k < n {
        if k > 0 {
            out.push('\n');
        }
        match p.peek() {
            Some(&&(start, end, owner, group, count)) if start == k => {
                out.push_str(&note(count, end - start, owner, group));
                for r in &idx[start..end] {
                    out.push('\n');
                    let l = r.line(body);
                    if r.pair(body) == (owner, group) {
                        out.push_str(&l[..r.links_end as usize]);
                        out.push_str(&l[r.group.1 as usize..]);
                    } else {
                        out.push_str(l);
                    }
                }
                k = end;
                p.next();
            }
            _ => {
                out.push_str(idx[k].line(body));
                k += 1;
            }
        }
    }
    if trailing {
        out.push('\n');
    }
    Some(out)
}

/// Parse one of our notes: (factored rows, total rows, owner, group).
fn parse_note(l: &str) -> Option<(usize, usize, &str, &str)> {
    let num = |s: &str| -> Option<usize> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit() || c == b',') {
            return None;
        }
        s.replace(',', "").parse().ok()
    };
    let rest = l.strip_prefix("[crush: ")?;
    let (counts, rest) = rest.split_once(" entries below are owned by user ")?;
    let (owner, rest) = rest.split_once(", group ")?;
    let (group, _) = rest.split_once("; their rows omit")?;
    let (k, n) = match counts.strip_prefix("all ") {
        Some(n) => (num(n)?, num(n)?),
        None => {
            let (k, n) = counts.split_once(" of the ")?;
            (num(k)?, num(n)?)
        }
    };
    (note(k, n, owner, group) == l).then_some((k, n, owner, group))
}

/// Is `out` exactly what [`restore`] turns back into `src`? The gate's check,
/// walking both texts side by side (slice comparisons; an allocation only for
/// rows that keep their own pair). Equivalent to `restore(out) == Some(src)`
/// (a property test holds them together).
pub fn verify(src: &str, out: &str) -> bool {
    let (tx, ty) = (src.ends_with('\n'), out.ends_with('\n'));
    if tx != ty {
        return false;
    }
    let mut x = (if tx { &src[..src.len() - 1] } else { src }).split('\n');
    let mut y = (if ty { &out[..out.len() - 1] } else { out }).split('\n');
    while let Some(l) = y.next() {
        if let Some((k, n, owner, group)) = parse_note(l) {
            // what parse_row demands of the pair itself, once per note
            let numeric = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
            let names_ok = name_ok(owner) && name_ok(group) && (numeric(owner) || !numeric(group));
            let pair = format!(" {owner} {group}");
            let mut got = 0;
            for _ in 0..n {
                let (Some(row), Some(orig)) = (y.next(), x.next()) else { return false };
                let Some(le) = links_end(row) else { return false };
                // factored iff the pair goes back in after the links digits
                // and the line then parses with it (`parse_row` on `orig`,
                // done piecewise: same prefix, the pair, a valid tail)
                let restored = names_ok
                    && orig.len() == row.len() + pair.len()
                    && orig.as_bytes()[..le] == row.as_bytes()[..le]
                    && orig.as_bytes()[le..le + pair.len()] == *pair.as_bytes()
                    && orig.as_bytes()[le + pair.len()..] == row.as_bytes()[le..]
                    && tail_ok(&row.as_bytes()[le..]);
                if restored {
                    got += 1;
                } else if row != orig || parse_row(row).is_none() || reinsert(row, owner, group).is_some() {
                    return false;
                }
            }
            if got != k {
                return false;
            }
            continue;
        }
        if l.starts_with("[crush: ") || x.next() != Some(l) {
            return false;
        }
    }
    x.next().is_none()
}

/// After the group: spaces, a size, spaces, something (as `parse_row` needs).
fn tail_ok(t: &[u8]) -> bool {
    let s = t.iter().take_while(|&&c| c == b' ').count();
    if s == 0 {
        return false;
    }
    let w = t[s..].iter().take_while(|&&c| c != b' ' && c != b'\t').count();
    let Ok(size) = std::str::from_utf8(&t[s..s + w]) else { return false };
    let s2 = t[s + w..].iter().take_while(|&&c| c == b' ').count();
    size_ok(size) && s2 > 0 && s + w + s2 < t.len()
}

/// End of the links digits of a row-shaped line (`mode spaces digits`).
fn links_end(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut i = is_mode(b)?;
    let s = b[i..].iter().take_while(|&&c| c == b' ').count();
    if s == 0 {
        return None;
    }
    i += s;
    let d = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    (d > 0 && d <= 9).then_some(i + d)
}

/// Undo [`factor`]: the original text, or `None` if `text` isn't a
/// well-formed factored listing (a note not followed by its rows, ...).
pub fn restore(text: &str) -> Option<String> {
    let (ls, trailing) = lines(text);
    let mut out: Vec<String> = Vec::with_capacity(ls.len());
    let mut i = 0;
    while i < ls.len() {
        let l = ls[i];
        if let Some((k, n, owner, group)) = parse_note(l) {
            if note(k, n, owner, group) != l || i + n > ls.len() {
                return None;
            }
            let mut got = 0;
            for row in &ls[i + 1..i + 1 + n] {
                match reinsert(row, owner, group) {
                    Some(full) => {
                        got += 1;
                        out.push(full);
                    }
                    None => {
                        parse_row(row)?;
                        out.push(row.to_string());
                    }
                }
            }
            if got != k {
                return None;
            }
            i += 1 + n;
            continue;
        }
        if l.starts_with("[crush: ") {
            return None;
        }
        out.push(l.to_string());
        i += 1;
    }
    let mut s = out.join("\n");
    if trailing {
        s.push('\n');
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LA: &str = "total 24\n\
drwxr-xr-x    3 haseeb haseeb   80 Oct 14 00:39 .\n\
drwxrwxrwt 6210 root   root   141K Oct 14 00:39 ..\n\
drwxr-xr-x    2 haseeb haseeb   60 Oct 14 00:39 nested\n\
-rw-r--r--    1 haseeb haseeb  152 Oct 14 00:39 stand.md\n\
-rw-r--r--    1 haseeb haseeb   56 Oct 14 02:53 stand.txt\n\
lrwxrwxrwx    1 haseeb haseeb   11 Oct 14 02:53 link -> stand.txt\n\
-rwxr-xr-x    1 haseeb haseeb 4096 Oct  1  2025 old.sh\n\
-rw-r--r--    1 haseeb haseeb  733 Oct  2 11:20 notes.md\n\
-rw-r--r--    1 haseeb haseeb   19 Oct  3 08:01 .env\n\
-rw-r--r--    1 haseeb haseeb  100 Oct  4 00:10 part-00.log\n\
-rw-r--r--    1 haseeb haseeb  101 Oct  4 01:11 part-01.log\n\
-rw-r--r--    1 haseeb haseeb  102 Oct  4 02:12 part-02.log\n\
-rw-r--r--    1 haseeb haseeb  103 Oct  4 03:13 part-03.log\n\
-rw-r--r--    1 haseeb haseeb  104 Oct  4 04:14 part-04.log\n\
-rw-r--r--    1 haseeb haseeb  105 Oct  4 05:15 part-05.log\n\
-rw-r--r--    1 haseeb haseeb  106 Oct  4 06:10 part-06.log\n\
-rw-r--r--    1 haseeb haseeb  107 Oct  4 07:11 part-07.log\n";

    #[test]
    fn factors_the_common_pair_and_keeps_exceptions() {
        let f = factor(LA, Some("ls -la /tmp/x")).unwrap();
        assert_eq!(
            f,
            "total 24\n\
[crush: 16 of the 17 entries below are owned by user haseeb, group haseeb; their rows omit those two columns (ls -go layout); the other entry shows its own owner and group]\n\
drwxr-xr-x    3   80 Oct 14 00:39 .\n\
drwxrwxrwt 6210 root   root   141K Oct 14 00:39 ..\n\
drwxr-xr-x    2   60 Oct 14 00:39 nested\n\
-rw-r--r--    1  152 Oct 14 00:39 stand.md\n\
-rw-r--r--    1   56 Oct 14 02:53 stand.txt\n\
lrwxrwxrwx    1   11 Oct 14 02:53 link -> stand.txt\n\
-rwxr-xr-x    1 4096 Oct  1  2025 old.sh\n\
-rw-r--r--    1  733 Oct  2 11:20 notes.md\n\
-rw-r--r--    1   19 Oct  3 08:01 .env\n\
-rw-r--r--    1  100 Oct  4 00:10 part-00.log\n\
-rw-r--r--    1  101 Oct  4 01:11 part-01.log\n\
-rw-r--r--    1  102 Oct  4 02:12 part-02.log\n\
-rw-r--r--    1  103 Oct  4 03:13 part-03.log\n\
-rw-r--r--    1  104 Oct  4 04:14 part-04.log\n\
-rw-r--r--    1  105 Oct  4 05:15 part-05.log\n\
-rw-r--r--    1  106 Oct  4 06:10 part-06.log\n\
-rw-r--r--    1  107 Oct  4 07:11 part-07.log\n"
        );
        assert_eq!(restore(&f).unwrap(), LA);
    }

    #[test]
    fn constant_pair_says_all() {
        let raw: String =
            (0..24).map(|i| format!("-rw-r--r-- 1 root root {:>5} Oct 12 09:41 lib{i}.so\n", 1000 + i * 7)).collect();
        let f = factor(&raw, Some("ls -l /usr/lib")).unwrap();
        assert!(f.starts_with("[crush: all 24 entries below are owned by user root, group root; their rows omit those two columns (ls -go layout)]\n"));
        // too few `root root` rows to pay for the note
        let few: String = raw.lines().take(8).map(|l| format!("{l}\n")).collect();
        assert_eq!(factor(&few, None), None);
        assert!(f.contains("\n-rw-r--r-- 1  1007 Oct 12 09:41 lib1.so\n"), "{f}");
        assert_eq!(restore(&f).unwrap(), raw);
    }

    #[test]
    fn leaves_alone() {
        // too few rows; ls -o (no group); mixed owners
        assert_eq!(factor("-rw-r--r-- 1 a a 1 Oct 1 00:00 x\n-rw-r--r-- 1 a a 1 Oct 1 00:00 y\n", None), None);
        let o: String = (0..8).map(|i| format!("-rw-r--r-- 1 haseeb 4096 Oct 12 09:41 f{i}\n")).collect();
        assert_eq!(factor(&o, Some("ls -lo")), None);
        assert_eq!(factor(&o, None), None);
        let mixed: String = (0..8).map(|i| format!("-rw-r--r-- 1 u{i} g {i:>3} Oct 12 09:41 f{i}\n")).collect();
        assert_eq!(factor(&mixed, None), None);
        // `ls -o` with a numeric size the same everywhere: the command says so
        let num: String = (0..8).map(|i| format!("drwxr-xr-x 2 haseeb 4096 1730000000 d{i}\n")).collect();
        assert_eq!(factor(&num, Some("ls -o --time-style=+%s")), None);
        assert_eq!(factor(&num, None), None, "numeric group with a named owner");
    }

    #[test]
    fn devices_human_sizes_and_numeric_ids() {
        let dev = "crw-rw-rw- 1 root root 1,   3 Oct 30 00:00 null\n\
crw-rw-rw- 1 root root 1,   5 Oct 30 00:00 zero\n\
crw-rw-rw- 1 root root 1,   8 Oct 30 00:00 random\n\
crw-rw-rw- 1 root root 1,   9 Oct 30 00:00 urandom\n\
crw-rw-rw- 1 root root 5,   0 Oct 30 00:00 tty\n\
crw--w---- 1 root root 4,   0 Oct 30 00:00 tty0\n\
crw--w---- 1 root root 4,   1 Oct 30 00:00 tty1\n\
crw--w---- 1 root root 4,  10 Oct 30 00:00 tty10\n\
crw--w---- 1 root root 4,  11 Oct 30 00:00 tty11\n\
crw--w---- 1 root root 4,  12 Oct 30 00:00 tty12\n\
crw--w---- 1 root root 4,  13 Oct 30 00:00 tty13\n\
crw--w---- 1 root root 4,  14 Oct 30 00:00 tty14\n\
crw--w---- 1 root root 4,  15 Oct 30 00:00 tty15\n\
crw--w---- 1 root root 4,  16 Oct 30 00:00 tty16\n\
crw--w---- 1 root root 4,  17 Oct 30 00:00 tty17\n\
crw--w---- 1 root root 4,  18 Oct 30 00:00 tty18\n";
        let f = factor(dev, Some("ls -l /dev")).unwrap();
        assert_eq!(restore(&f).unwrap(), dev);
        let h: String =
            (0..30).map(|i| format!("-rw-r--r-- 1 1000 1000 {}.{}M Jul 22 13:11 v{i}.mp4\n", i % 10, i % 7)).collect();
        let f = factor(&h, Some("ls -lnh")).unwrap();
        assert!(f.contains("all 30 entries below are owned by user 1000, group 1000"));
        assert_eq!(restore(&f).unwrap(), h);
    }

    #[test]
    fn restore_rejects_tampering() {
        let f = factor(LA, None).unwrap();
        assert!(restore(&f.replace("16 of the 17", "15 of the 17")).is_none());
        assert!(restore(&f.replace("16 of the 17", "16 of the 18")).is_none());
        assert_eq!(restore("plain\ntext\n").unwrap(), "plain\ntext\n");
        assert!(restore("[crush: something else]\n").is_none());
        // fuzz regression: a note-like line in the input is never factored around
        let mut x = String::from("[crush: o\n");
        x.push_str(LA);
        assert_eq!(factor(&x, None), None);
    }
}
