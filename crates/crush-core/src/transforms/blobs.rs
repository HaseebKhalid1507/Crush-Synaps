//! Elide long base64 / hex runs and minified lines.
//!
//! ```text
//! "data": "[crush: 5,120-char base64 blob elided, starts iVBORw0KGgoAAAANS; raw output: <ref>]"
//! ```
//!
//! Lossy, so it only runs when the host keeps the raw output (`raw_ref`). The
//! marker says what was cut, how long it was, how it started, and where the
//! raw is. Detection is strict so prose, paths, identifiers and single hashes
//! survive: base64 needs ≥ 256 chars mixing upper, lower and digits; hex needs
//! ≥ 128 chars (two SHA-256s' worth) with letters and digits; a minified line
//! needs ≥ 4 KiB with almost no whitespace and code-like punctuation, and is
//! never JSON (that's for a STRUCTURED transform to handle losslessly).

use super::{group_digits, lines};

pub const NAME: &str = "blobs";

const MIN_B64: usize = 256;
const MIN_HEX: usize = 128;
const MIN_MINIFIED: usize = 4096;
const SHOW: usize = 16;

fn b64_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'-' || c == b'_'
}

fn marker(kind: &str, len: usize, head: &str, raw_ref: &str) -> String {
    format!("[crush: {}-char {kind} elided, starts {head}; raw output: {raw_ref}]", group_digits(len))
}

/// Is `run` (all base64/hex alphabet) a blob, and of which kind?
fn blob_kind(run: &[u8]) -> Option<&'static str> {
    let (mut up, mut lo, mut dig, mut slash, mut hex) = (0usize, 0usize, 0usize, 0usize, true);
    for &c in run {
        if c.is_ascii_uppercase() {
            up += 1;
        } else if c.is_ascii_lowercase() {
            lo += 1;
        } else if c.is_ascii_digit() {
            dig += 1;
        } else if c == b'/' {
            slash += 1;
        }
        if !c.is_ascii_hexdigit() {
            hex = false;
        }
    }
    let n = run.len();
    if hex && n >= MIN_HEX && dig > 0 && (up + lo) > 0 {
        return Some("hex blob");
    }
    if n >= MIN_B64 && up * 10 >= n && lo * 10 >= n && dig * 20 >= n && slash * 10 <= n {
        // long natural-language-ish runs can't get here (no separators), but
        // identifiers_joined_like_this have no digits; paths have many '/'
        return Some("base64 blob");
    }
    None
}

fn minified(line: &str) -> bool {
    let b = line.as_bytes();
    if b.len() < MIN_MINIFIED {
        return false;
    }
    let t = line.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        return false;
    }
    let ws = b.iter().filter(|c| c.is_ascii_whitespace()).count();
    let punct = b.iter().filter(|c| matches!(c, b'{' | b'}' | b'(' | b')' | b';' | b',' | b'=' | b'<' | b'>' | b':')).count();
    ws * 50 < b.len() && punct * 7 > b.len()
}

fn head(s: &str) -> &str {
    let mut end = SHOW.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn elide_line(line: &str, raw_ref: &str, out: &mut String) -> bool {
    if minified(line) {
        let lead = line.len() - line.trim_start().len();
        out.push_str(&line[..lead]);
        out.push_str(&marker("minified line", line.len() - lead, head(&line[lead..]), raw_ref));
        return true;
    }
    if line.len() < MIN_HEX {
        out.push_str(line);
        return false;
    }
    let b = line.as_bytes();
    let mut changed = false;
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        if !b64_char(b[i]) {
            i += 1;
            continue;
        }
        let s = i;
        while i < b.len() && b64_char(b[i]) {
            i += 1;
        }
        let mut e = i;
        // base64 padding
        while e < b.len() && b[e] == b'=' && e - i < 2 {
            e += 1;
        }
        if e - s >= MIN_HEX {
            if let Some(kind) = blob_kind(&b[s..i]) {
                out.push_str(&line[last..s]);
                out.push_str(&marker(kind, e - s, head(&line[s..e]), raw_ref));
                last = e;
                changed = true;
            }
        }
        i = e.max(i);
    }
    out.push_str(&line[last..]);
    changed
}

pub fn elide(text: &str, raw_ref: &str) -> Option<String> {
    let (ls, trailing) = lines(text);
    if !ls.iter().any(|l| l.len() >= MIN_HEX) {
        return None;
    }
    let mut out = String::with_capacity(text.len().min(1 << 16));
    let mut changed = false;
    for (k, l) in ls.iter().enumerate() {
        if k > 0 {
            out.push('\n');
        }
        changed |= elide_line(l, raw_ref, &mut out);
    }
    if !changed {
        return None;
    }
    if trailing {
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_hex() {
        let b64: String = (0..400).map(|i| b"QmFzZTY0IGlzIGZ1bjEyMw9xYZ+/"[i % 28] as char).collect();
        let raw = format!("{{\"data\": \"{b64}==\"}}\n");
        let got = elide(&raw, "/r/x.txt").unwrap();
        assert_eq!(got, "{\"data\": \"[crush: 402-char base64 blob elided, starts QmFzZTY0IGlzIGZ1; raw output: /r/x.txt]\"}\n");
        let hex = "deadbeef0123456789".repeat(10);
        assert!(elide(&format!("x {hex} y"), "R").unwrap().contains("180-char hex blob elided"));
    }

    #[test]
    fn leaves_ordinary_text() {
        let sha = "a".repeat(10) + &"0123456789abcdef".repeat(3) + "012345";
        assert_eq!(elide(&format!("commit {sha}\n"), "R"), None, "a single sha256 stays");
        let path = "/very/long/path/".repeat(30);
        assert_eq!(elide(&path, "R"), None);
        let ident = "some_identifier_name_".repeat(20);
        assert_eq!(elide(&ident, "R"), None);
    }
}
