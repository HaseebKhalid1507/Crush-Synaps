//! STRUCTURED: pretty-printed JSON from command output, re-laid out compactly.
//!
//! Each top-level array element (or object member) goes on its own line with
//! no other whitespace:
//!
//! ```text
//! [
//! {"number":12,"title":"Fix the thing","state":"OPEN","author":null},
//! {"number":11,"title":"","state":"CLOSED","author":"dev2"}
//! ]
//! ```
//!
//! Measured on real `count_tokens` (eval/p2b_layouts.py, 25 pretty JSON
//! outputs): fully minified saves 20.7% / 29.3% (5.x / 4.x), this layout
//! 20.1% / 28.4%, while keeping an array's length visible as its line count
//! and every object whole on one line (`", "` / `": "` separators would give
//! back a fifth to a quarter of it).
//!
//! Only whitespace *between* tokens changes: every key, string (escapes
//! included), number, `true`/`false`/`null`, and the key order are copied byte
//! for byte, so it is lossless in meaning and needs no raw output. It applies
//! to
//!
//! - an output that is one or more JSON values separated by whitespace (jq
//!   prints one value per result) where at least one value spans lines: each
//!   value is laid out as above;
//! - otherwise, blocks inside the output that start with a line holding only
//!   `{` or `[` and end with the matching `}` / `]` line at column 0, when the
//!   block parses as JSON.
//!
//! The parser is strict (RFC 8259, nesting at most [`MAX_DEPTH`]); anything
//! that doesn't parse is left exactly as it was, and so is anything over
//! [`MAX_JSON_BYTES`]. `cat foo.json` never gets
//! here: file views are CONTENT.

pub const NAME: &str = "json_minify";

const MAX_DEPTH: usize = 128;
/// Larger outputs are left alone. The strict parse costs ~1.3 ns/byte, which
/// would eat most of the 1 ms budget at 256 KiB, and in Synaps anything this
/// big spills anyway: history keeps a fixed 7 KiB head + tail preview, so
/// minifying first saves ~nothing there (eval/RESULTS.md, "after spill").
pub const MAX_JSON_BYTES: usize = 128 * 1024;

struct P<'a> {
    b: &'a [u8],
    i: usize,
    /// Bytes copied from `b` (valid UTF-8 slices cut at ASCII) plus ASCII
    /// punctuation; validated once at the end.
    out: Vec<u8>,
    /// `, ` / `: ` instead of `,` / `:` (measurement only).
    spaced: bool,
    /// One top-level element per line.
    lines: bool,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        let rest = &self.b[self.i..];
        self.i += rest.iter().position(|&c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r')).unwrap_or(rest.len());
    }

    fn value(&mut self, depth: usize) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                self.out.push(b'{');
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    self.out.push(b'}');
                    return Some(());
                }
                let nl = self.lines && depth == 0;
                if nl {
                    self.out.push(b'\n');
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return None;
                    }
                    self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return None;
                    }
                    self.i += 1;
                    self.out.extend_from_slice(if self.spaced { b": " } else { b":" });
                    self.value(depth + 1)?;
                    self.ws();
                    match self.b.get(self.i)? {
                        b',' => {
                            self.i += 1;
                            self.out.extend_from_slice(if nl { b",\n" } else if self.spaced { b", " } else { b"," });
                        }
                        b'}' => {
                            self.i += 1;
                            self.out.extend_from_slice(if nl { b"\n}" } else { b"}" });
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.i += 1;
                self.out.push(b'[');
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    self.out.push(b']');
                    return Some(());
                }
                let nl = self.lines && depth == 0;
                if nl {
                    self.out.push(b'\n');
                }
                loop {
                    self.value(depth + 1)?;
                    self.ws();
                    match self.b.get(self.i)? {
                        b',' => {
                            self.i += 1;
                            self.out.extend_from_slice(if nl { b",\n" } else if self.spaced { b", " } else { b"," });
                        }
                        b']' => {
                            self.i += 1;
                            self.out.extend_from_slice(if nl { b"\n]" } else { b"]" });
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => self.string(),
            b't' => self.lit(b"true"),
            b'f' => self.lit(b"false"),
            b'n' => self.lit(b"null"),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn lit(&mut self, w: &[u8]) -> Option<()> {
        if self.b[self.i..].starts_with(w) {
            self.i += w.len();
            self.out.extend_from_slice(w);
            Some(())
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<()> {
        let s = self.i;
        let b = self.b;
        let mut i = s;
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        match b.get(i)? {
            b'0' => i += 1,
            b'1'..=b'9' => {
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
            }
            _ => return None,
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            let d = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == d {
                return None;
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(b.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            let d = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == d {
                return None;
            }
        }
        self.out.extend_from_slice(&b[s..i]);
        self.i = i;
        Some(())
    }

    /// Copy a string literal verbatim, validating escapes and controls.
    fn string(&mut self) -> Option<()> {
        let s = self.i;
        let b = self.b;
        let mut i = s + 1;
        loop {
            // most strings are short: a plain loop beats memchr's setup there
            let lim = (i + 32).min(b.len());
            let mut j = i;
            while j < lim && b[j] != b'"' && b[j] != b'\\' && b[j] >= 0x20 {
                j += 1;
            }
            if j == lim && j < b.len() {
                // long run: jump to the next quote or backslash; nothing
                // before it may be a raw control character
                let rel = memchr::memchr2(b'"', b'\\', &b[j..])?;
                if b[j..j + rel].iter().any(|&c| c < 0x20) {
                    return None;
                }
                j += rel;
            }
            match *b.get(j)? {
                b'"' => {
                    i = j + 1;
                    break;
                }
                b'\\' => match *b.get(j + 1)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => i = j + 2,
                    b'u' => {
                        let h = b.get(j + 2..j + 6)?;
                        if !h.iter().all(u8::is_ascii_hexdigit) {
                            return None;
                        }
                        i = j + 6;
                    }
                    _ => return None,
                },
                _ => return None, // raw control character: not JSON
            }
        }
        self.out.extend_from_slice(&b[s..i]);
        self.i = i;
        Some(())
    }
}

/// Lay out one JSON text holding one or more whitespace-separated values.
/// Returns the values (one after another, each starting on a new line) and
/// whether any of them spanned lines in the input.
fn minify_values(s: &str, st: Style) -> Option<(String, bool)> {
    let mut p = P { b: s.as_bytes(), i: 0, out: Vec::with_capacity(s.len()), spaced: st.spaced, lines: st.lines };
    let mut pretty = false;
    let mut n = 0usize;
    loop {
        p.ws();
        if p.i >= p.b.len() {
            break;
        }
        if n > 0 {
            p.out.push(b'\n');
        }
        let start = p.i;
        p.value(0)?;
        // a value that isn't a container (or a one-line one) carries no layout
        if memchr::memchr(b'\n', &p.b[start..p.i]).is_some() {
            pretty = true;
        }
        n += 1;
        // values must be separated by whitespace (jq prints one per line)
        if p.i < p.b.len() && !matches!(p.b[p.i], b' ' | b'\t' | b'\n' | b'\r') {
            return None;
        }
    }
    if n == 0 {
        return None;
    }
    // slices of a &str cut at ASCII bytes, plus ASCII: always UTF-8
    String::from_utf8(p.out).ok().map(|o| (o, pretty))
}

/// Layout knobs; the shipped layout is [`Style::SHIPPED`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// `, ` / `: ` separators.
    pub spaced: bool,
    /// One top-level element per line (else everything on one line).
    pub lines: bool,
}

impl Style {
    pub const SHIPPED: Style = Style { spaced: false, lines: true };
}

/// Re-lay out pretty JSON (whole output or column-0 blocks). `None` when
/// nothing changed.
pub fn minify(text: &str) -> Option<String> {
    minify_with(text, Style::SHIPPED)
}

/// [`minify`] with a chosen layout (the eval measures the alternatives).
pub fn minify_with(text: &str, st: Style) -> Option<String> {
    if text.len() > MAX_JSON_BYTES || memchr::memchr(b'\n', text.as_bytes()).is_none() {
        return None;
    }
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let lead = &text[..text.len() - text.trim_start().len()];
    let tail = &text[text.trim_end().len()..];
    if t.starts_with(['{', '[']) {
        if let Some((m, pretty)) = minify_values(t, st) {
            if !pretty {
                return None;
            }
            // keep the output's own leading/trailing whitespace
            let mut out = String::with_capacity(m.len() + lead.len() + tail.len());
            out.push_str(lead);
            out.push_str(&m);
            out.push_str(tail);
            // `[1,\n2]` would come out longer one-per-line: leave it
            return (out.len() < text.len()).then_some(out);
        }
    }
    blocks(text, st)
}

/// Column-0 `{`/`[` ... `}`/`]` blocks that parse as JSON.
fn blocks(text: &str, st: Style) -> Option<String> {
    let b = text.as_bytes();
    if memchr::memmem::find(b, b"\n{\n").is_none()
        && memchr::memmem::find(b, b"\n[\n").is_none()
        && !text.starts_with("{\n")
        && !text.starts_with("[\n")
    {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut pos = 0usize; // start of the current line
    let mut copied = 0usize; // text[..copied] is already in `out`
    while pos < text.len() {
        let end = memchr::memchr(b'\n', &b[pos..]).map_or(text.len(), |k| pos + k);
        let line = &text[pos..end];
        if line == "{" || line == "[" {
            let close = if line == "{" { "}" } else { "]" };
            // find the first column-0 closing line after it that makes a valid block
            let mut q = end;
            let mut done = None;
            while q < text.len() {
                let ls = q + 1;
                if ls > text.len() {
                    break;
                }
                let le = memchr::memchr(b'\n', &b[ls..]).map_or(text.len(), |k| ls + k);
                if &text[ls..le] == close {
                    if let Some((m, _)) = minify_values(&text[pos..le], st) {
                        if m.len() < le - pos {
                            done = Some((le, m));
                        }
                    }
                    break; // the first column-0 close decides: no guessing past it
                }
                if !text[ls..le].starts_with([' ', '\t']) && !text[ls..le].is_empty() {
                    break; // a column-0 line that isn't the close: not a pretty block
                }
                q = le;
            }
            if let Some((le, m)) = done {
                out.push_str(&text[copied..pos]);
                out.push_str(&m);
                copied = le;
                changed = true;
                pos = le;
                continue;
            }
        }
        pos = end + 1;
    }
    if !changed {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAT: Style = Style { spaced: false, lines: false };

    #[test]
    fn whole_and_stream() {
        let raw = "{\n  \"a\": 1,\n  \"b\": [\n    null,\n    \"\",\n    \"x \\\"y\\\" \\u00e9\"\n  ],\n  \"c\": {}\n}\n";
        assert_eq!(minify_with(raw, FLAT).unwrap(), "{\"a\":1,\"b\":[null,\"\",\"x \\\"y\\\" \\u00e9\"],\"c\":{}}\n");
        assert_eq!(
            minify_with(raw, Style { spaced: true, lines: false }).unwrap(),
            "{\"a\": 1, \"b\": [null, \"\", \"x \\\"y\\\" \\u00e9\"], \"c\": {}}\n"
        );
        let out = minify(raw).unwrap();
        assert_eq!(out, "{\n\"a\":1,\n\"b\":[null,\"\",\"x \\\"y\\\" \\u00e9\"],\n\"c\":{}\n}\n");
        assert_eq!(minify(&out), None, "idempotent");
        let jq = "{\n  \"n\": 1\n}\n{\n  \"n\": 2.50e+3\n}\n";
        assert_eq!(minify_with(jq, FLAT).unwrap(), "{\"n\":1}\n{\"n\":2.50e+3}\n");
        let arr = "[\n  {\n    \"id\": 1\n  },\n  {\n    \"id\": 2\n  }\n]\n";
        assert_eq!(minify(arr).unwrap(), "[\n{\"id\":1},\n{\"id\":2}\n]\n");
        assert_eq!(minify("{\"a\":1}\n"), None, "already minified");
        assert_eq!(minify("\"just a string\"\n"), None);
    }

    #[test]
    fn rejects() {
        for bad in ["{\n \"a\": 01\n}", "{\n \"a\": 1,\n}", "[\n 1 2\n]", "{\n \"a\": \"x\ty\"\n}", "{\n a: 1\n}", "[\n -\n]"] {
            assert_eq!(minify(bad), None, "{bad:?}");
        }
        let deep = format!("{}{}", "[\n".repeat(200), "]".repeat(200));
        assert_eq!(minify(&deep), None);
    }

    #[test]
    fn embedded_blocks() {
        let raw = "status: ok\n{\n  \"id\": 7,\n  \"tags\": [\n    \"a\"\n  ]\n}\ndone\n[\n  1,\n  oops\n]\n";
        let out = minify(raw).unwrap();
        assert_eq!(out, "status: ok\n{\n\"id\":7,\n\"tags\":[\"a\"]\n}\ndone\n[\n  1,\n  oops\n]\n");
        assert_eq!(minify(&out), None, "idempotent");
        assert_eq!(minify_with("x\n{\n  \"a\": 1\n}", FLAT), Some("x\n{\"a\":1}".into()));
    }
}
