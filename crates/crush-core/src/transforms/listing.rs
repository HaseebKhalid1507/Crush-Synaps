//! LISTING: `ls -l` owner and group, said once in words when (nearly) every
//! row carries the same pair.
//!
//! ```text
//! total 24                                   total 24
//! drwxr-xr-x  3 haseeb haseeb   80 Oct 14 .  [crush: owner haseeb, group haseeb on 3 of the 4 rows below (left out of them); the other row shows its own]
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

/// Parse `mode links owner group size rest` (rest non-empty).
fn parse_row(line: &str) -> Option<Row<'_>> {
    let b = line.as_bytes();
    let mut i = is_mode(b)?;
    let sp = |i: usize| b[i..].iter().take_while(|&&c| c == b' ').count();
    let word = |i: usize| b[i..].iter().take_while(|&&c| c != b' ' && c != b'\t').count();
    let s = sp(i);
    if s == 0 {
        return None;
    }
    i += s;
    let d = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if d == 0 || d > 9 {
        return None;
    }
    i += d;
    let links_end = i;
    let s = sp(i);
    if s == 0 {
        return None;
    }
    i += s;
    let w = word(i);
    let owner = &line[i..i + w];
    i += w;
    let s = sp(i);
    if s == 0 {
        return None;
    }
    i += s;
    let w = word(i);
    let group = &line[i..i + w];
    i += w;
    let group_end = i;
    let s = sp(i);
    if s == 0 {
        return None;
    }
    i += s;
    let w = word(i);
    let size = &line[i..i + w];
    i += w;
    if !name_ok(owner) || !name_ok(group) || !size_ok(size) || sp(i) == 0 || i + sp(i) >= b.len() {
        return None;
    }
    // `ls -o`: the "group" would be the size; a numeric group only with a
    // numeric owner (`ls -n`)
    let numeric = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
    if numeric(group) && !numeric(owner) {
        return None;
    }
    Some(Row { links_end, owner, group, group_end })
}

fn note(n: usize, total: usize, owner: &str, group: &str) -> String {
    if n == total {
        format!(
            "[crush: owner {owner}, group {group} on all {} rows below; those columns are left out]",
            group_digits(total)
        )
    } else {
        let other = total - n;
        format!(
            "[crush: owner {owner}, group {group} on {} of the {} rows below (left out of them); the other {} {} its own]",
            group_digits(n),
            group_digits(total),
            if other == 1 { "row".to_string() } else { format!("{} rows", group_digits(other)) },
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

/// Factor owner/group out of `ls -l` blocks. `None` when nothing changed.
pub fn factor(text: &str, command: Option<&str>) -> Option<String> {
    if drops_columns(command) {
        return None;
    }
    let (ls, trailing) = lines(text);
    let rows: Vec<Option<Row>> = ls.iter().map(|l| parse_row(l)).collect();
    let mut changed = false;
    let mut i = 0;
    let n = ls.len();
    let mut emitted: Vec<String> = Vec::with_capacity(n + 4);
    while i < n {
        if rows[i].is_none() {
            emitted.push(ls[i].to_string());
            i += 1;
            continue;
        }
        let start = i;
        while i < n && rows[i].is_some() {
            i += 1;
        }
        let block = &rows[start..i];
        let total = block.len();
        // most common pair (first seen wins ties: deterministic)
        let mut best: Option<((&str, &str), usize)> = None;
        for r in block.iter().flatten() {
            let k = (r.owner, r.group);
            let c = block.iter().flatten().filter(|x| (x.owner, x.group) == k).count();
            if best.map_or(true, |(_, bc)| c > bc) {
                best = Some((k, c));
            }
        }
        let ((owner, group), count) = best.expect("non-empty block");
        let ok = total >= MIN_ROWS
            && count * 100 >= total * MIN_SHARE_PCT
            && count >= MIN_ROWS
            && {
                // the pair must sit as exactly ` owner group`, and the round
                // trip must be unambiguous for every row of the block
                block.iter().zip(&ls[start..i]).all(|(r, l)| {
                    let r = r.expect("row");
                    if (r.owner, r.group) == (owner, group) {
                        l[r.links_end..r.group_end] == format!(" {owner} {group}")
                            && reinsert(&cut(l, &r), owner, group).as_deref() == Some(*l)
                    } else {
                        reinsert(l, owner, group).is_none()
                    }
                })
            }
            && {
                // worth it: the bytes left out pay for the note. (Per-block bytes,
                // not the estimator: it prices rare names like `haseeb` at ~1
                // token where count_tokens says 3-4. The gate still judges the
                // whole result in estimated tokens, so this only picks blocks.)
                count * (owner.len() + group.len() + 2) >= note(count, total, owner, group).len()
            };
        if !ok {
            emitted.extend(ls[start..i].iter().map(|l| l.to_string()));
            continue;
        }
        changed = true;
        emitted.push(note(count, total, owner, group));
        for (r, l) in block.iter().zip(&ls[start..i]) {
            let r = r.expect("row");
            if (r.owner, r.group) == (owner, group) {
                emitted.push(cut(l, &r));
            } else {
                emitted.push(l.to_string());
            }
        }
    }
    if !changed {
        return None;
    }
    let mut out = emitted.join("\n");
    if trailing {
        out.push('\n');
    }
    Some(out)
}

fn cut(line: &str, r: &Row) -> String {
    format!("{}{}", &line[..r.links_end], &line[r.group_end..])
}

/// Parse one of our notes: (factored rows, total rows, owner, group).
fn parse_note(l: &str) -> Option<(usize, usize, &str, &str)> {
    let num = |s: &str| -> Option<usize> {
        if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit() || c == b',') {
            return None;
        }
        s.replace(',', "").parse().ok()
    };
    let rest = l.strip_prefix("[crush: owner ")?;
    let (owner, rest) = rest.split_once(", group ")?;
    let (group, rest) = rest.split_once(" on ")?;
    let (k, n) = if let Some(r) = rest.strip_prefix("all ") {
        let n = num(r.split_once(' ')?.0)?;
        (n, n)
    } else {
        let (k, r) = rest.split_once(" of the ")?;
        (num(k)?, num(r.split_once(' ')?.0)?)
    };
    (note(k, n, owner, group) == l).then_some((k, n, owner, group))
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
-rw-r--r--    1 haseeb haseeb   19 Oct  3 08:01 .env\n";

    #[test]
    fn factors_the_common_pair_and_keeps_exceptions() {
        let f = factor(LA, Some("ls -la /tmp/x")).unwrap();
        assert_eq!(
            f,
            "total 24\n\
[crush: owner haseeb, group haseeb on 8 of the 9 rows below (left out of them); the other row shows its own]\n\
drwxr-xr-x    3   80 Oct 14 00:39 .\n\
drwxrwxrwt 6210 root   root   141K Oct 14 00:39 ..\n\
drwxr-xr-x    2   60 Oct 14 00:39 nested\n\
-rw-r--r--    1  152 Oct 14 00:39 stand.md\n\
-rw-r--r--    1   56 Oct 14 02:53 stand.txt\n\
lrwxrwxrwx    1   11 Oct 14 02:53 link -> stand.txt\n\
-rwxr-xr-x    1 4096 Oct  1  2025 old.sh\n\
-rw-r--r--    1  733 Oct  2 11:20 notes.md\n\
-rw-r--r--    1   19 Oct  3 08:01 .env\n"
        );
        assert_eq!(restore(&f).unwrap(), LA);
    }

    #[test]
    fn constant_pair_says_all() {
        let raw: String =
            (0..8).map(|i| format!("-rw-r--r-- 1 root root {:>5} Oct 12 09:41 lib{i}.so\n", 1000 + i * 7)).collect();
        let f = factor(&raw, Some("ls -l /usr/lib")).unwrap();
        assert!(f.starts_with("[crush: owner root, group root on all 8 rows below; those columns are left out]\n"));
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
crw--w---- 1 root root 4,  10 Oct 30 00:00 tty10\n";
        let f = factor(dev, Some("ls -l /dev")).unwrap();
        assert_eq!(restore(&f).unwrap(), dev);
        let h: String = (0..9).map(|i| format!("-rw-r--r-- 1 1000 1000 {i}.{i}M Jul 22 13:11 v{i}.mp4\n")).collect();
        let f = factor(&h, Some("ls -lnh")).unwrap();
        assert!(f.contains("owner 1000, group 1000 on all 9 rows"));
        assert_eq!(restore(&f).unwrap(), h);
    }

    #[test]
    fn restore_rejects_tampering() {
        let f = factor(LA, None).unwrap();
        assert!(restore(&f.replace("8 of the 9", "7 of the 9")).is_none());
        assert!(restore(&f.replace("8 of the 9", "8 of the 10")).is_none());
        assert_eq!(restore("plain\ntext\n").unwrap(), "plain\ntext\n");
        assert!(restore("[crush: something else]\n").is_none());
    }
}
