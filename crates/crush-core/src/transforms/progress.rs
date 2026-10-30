//! Collapse `\r` progress rewrites to what the terminal finally showed.
//!
//! `"  5%\r 50%\r100%"` is drawn over itself; a terminal ends up showing
//! `100%`. Each line is replayed with carriage-return overwrite semantics and
//! only the final visible text is kept. A CRLF line ending (a lone `\r` right
//! before the newline) is left exactly as it was. Lines with escape sequences
//! are left alone (we don't emulate a terminal). The intermediate frames are
//! gone, so this is lossy: one marker at the end points at the raw output.

use super::{group_digits, lines};

pub const NAME: &str = "progress";

/// Replay one line; `None` if it has no interior `\r`.
fn replay(line: &str) -> Option<(String, usize)> {
    let (body, crlf) = match line.strip_suffix('\r') {
        Some(b) => (b, true),
        None => (line, false),
    };
    if !body.contains('\r') || body.contains('\x1b') {
        return None;
    }
    let mut buf: Vec<char> = Vec::new();
    let mut frames = 0usize;
    for (k, seg) in body.split('\r').enumerate() {
        if k > 0 {
            frames += 1;
        }
        for (pos, ch) in seg.chars().enumerate() {
            if pos < buf.len() {
                buf[pos] = ch;
            } else {
                buf.push(ch);
            }
        }
    }
    let mut s: String = buf.into_iter().collect();
    if crlf {
        s.push('\r');
    }
    Some((s, frames))
}

pub fn collapse(text: &str, raw_ref: &str) -> Option<String> {
    if memchr::memchr(b'\r', text.as_bytes()).is_none() {
        return None;
    }
    let (ls, trailing) = lines(text);
    let mut out = String::with_capacity(text.len());
    let mut frames = 0usize;
    let mut changed = 0usize;
    for (i, l) in ls.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match replay(l) {
            Some((s, f)) if s != *l => {
                out.push_str(&s);
                frames += f;
                changed += 1;
            }
            _ => out.push_str(l),
        }
    }
    if changed == 0 {
        return None;
    }
    out.push('\n');
    out.push_str(&format!(
        "[crush: {} carriage-return progress updates on {} line{} collapsed to their final state; raw output: {}]",
        group_digits(frames),
        group_digits(changed),
        if changed == 1 { "" } else { "s" },
        raw_ref
    ));
    if trailing {
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays() {
        let raw = "start\n  5%\r 50%\r100%\ndone\n";
        assert_eq!(
            collapse(raw, "R").unwrap(),
            "start\n100%\ndone\n[crush: 2 carriage-return progress updates on 1 line collapsed to their final state; raw output: R]\n"
        );
        // overlay keeps the longer tail, like a terminal
        assert_eq!(replay("abcdef\rXY").unwrap().0, "XYcdef");
    }

    #[test]
    fn crlf_untouched() {
        assert_eq!(collapse("a\r\nb\r\n", "R"), None);
        assert_eq!(collapse("x\x1b[K\r50%\r100%\n", "R"), None);
    }
}
