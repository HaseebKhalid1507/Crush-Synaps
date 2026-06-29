//! Text-cleanup transforms for unstructured tool output (build logs, test runs,
//! verbose traces). Unlike [`super::tabular`]/[`super::columnar`] these
//! **compose** — strip ANSI, then trailing whitespace, then blank-line runs,
//! then duplicate-line runs, then blob elision, then path-prefix folding.
//!
//! Meaning-preserving (lossy only via [`elide_blobs`], by design): colour codes
//! carry no information to an LLM, trailing whitespace is invisible, and `(×N)`
//! is a faithful compact stand-in for N identical lines.

use crate::markers;

/// Run the full text-cleanup chain.
#[must_use]
pub fn clean(s: &str) -> String {
    let s = strip_ansi(s);
    let s = rstrip_lines(&s);
    let s = collapse_blank_runs(&s);
    let s = collapse_dup_runs(&s);
    let s = elide_blobs(&s);
    fold_common_prefix(&s)
}

/// Strip ANSI escape sequences (CSI colour/cursor codes, OSC strings).
#[must_use]
pub fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            match bytes.get(i + 1) {
                // CSI: ESC [ ... <final 0x40..=0x7E>
                Some(b'[') => {
                    i += 2;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1; // consume final byte
                }
                // OSC: ESC ] ... terminated by BEL (0x07) or ST (ESC \).
                Some(b']') => {
                    i += 2;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1; // consume BEL
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 2; // consume ESC and backslash (ST)
                            break;
                        }
                        i += 1;
                    }
                }
                // Other two-byte escapes — drop ESC + the next byte.
                Some(_) => i += 2,
                None => i += 1,
            }
        } else {
            let end = (i + utf8_len(bytes[i])).min(bytes.len());
            out.push_str(std::str::from_utf8(&bytes[i..end]).unwrap_or(""));
            i = end;
        }
    }
    out
}

/// Length in bytes of a UTF-8 char given its lead byte. Continuation/invalid
/// bytes advance by 1 (they only appear mid-sequence in valid UTF-8, and this
/// keeps the scanner safe on any input).
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Strip trailing whitespace from every line. Normalises CRLF → LF.
#[must_use]
pub fn rstrip_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut lines = s.lines().peekable();
    while let Some(line) = lines.next() {
        out.push_str(line.trim_end());
        if lines.peek().is_some() || s.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Collapse runs of 2+ blank lines into a single blank line.
#[must_use]
pub fn collapse_blank_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = false;
    for line in s.lines() {
        if line.is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(line);
            out.push('\n');
            blank = false;
        }
    }
    if !s.ends_with('\n') {
        out.pop();
    }
    out
}

/// Collapse runs of 2+ identical consecutive lines into `line (×N)`. Lines that
/// already carry a `(×N)` suffix are emitted verbatim (no double-annotation) so
/// the transform stays idempotent and unambiguous.
#[must_use]
pub fn collapse_dup_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.lines().peekable();
    while let Some(line) = iter.next() {
        let mut count = 1usize;
        while iter.peek() == Some(&line) {
            iter.next();
            count += 1;
        }
        if count > 1 && !has_dup_suffix(line) {
            out.push_str(line);
            out.push_str(&format!(" (×{count})"));
            out.push('\n');
        } else {
            // Singleton, or a run we refuse to annotate — emit verbatim.
            for _ in 0..count {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    if !s.ends_with('\n') {
        out.pop();
    }
    out
}

/// True if a line already ends with a ` (×<digits>)` suffix.
fn has_dup_suffix(line: &str) -> bool {
    line.rsplit_once(" (×").is_some_and(|(_, n)| {
        n.strip_suffix(')')
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// Elide long contiguous base64/hex-ish blobs (> 1 KiB) — embedded files, data
/// URIs, dumps the model can't use anyway. Conservative threshold leaves hashes,
/// tokens and short IDs untouched. This is the one *lossy* transform.
#[must_use]
pub fn elide_blobs(s: &str) -> String {
    const THRESHOLD: usize = 1024;
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if is_blob_char(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_blob_char(bytes[i]) {
                i += 1;
            }
            let run = i - start;
            if run > THRESHOLD {
                out.push_str(&format!("[{} {run} chars]", markers::BLOB));
            } else {
                out.push_str(&s[start..i]);
            }
        } else {
            // is_blob_char only matches ASCII (<0x80), so multi-byte UTF-8 lead
            // and continuation bytes land here and are copied whole.
            let end = (i + utf8_len(bytes[i])).min(bytes.len());
            out.push_str(&s[i..end]);
            i = end;
        }
    }
    out
}

fn is_blob_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}

/// Factor a shared path prefix out of path-heavy output (`find`, `ls -R`). If
/// every non-empty line shares a directory prefix, declare it once in a
/// `@crush/1.prefix=` header and strip it from each line. Lossless. Only fires
/// for 8+ lines, a 12+ char prefix, and a net byte win.
#[must_use]
pub fn fold_common_prefix(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let non_empty: Vec<&str> = lines.iter().copied().filter(|l| !l.is_empty()).collect();
    if non_empty.len() < 8 {
        return s.to_string();
    }
    let common = longest_common_prefix(&non_empty);
    let cut = match common.rfind('/') {
        Some(i) => i + 1,
        None => return s.to_string(),
    };
    if cut < 12 {
        return s.to_string();
    }
    let prefix = &common[..cut];
    let mut out = format!("{}{prefix}\n", markers::PREFIX);
    for line in &lines {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(line.strip_prefix(prefix).unwrap_or(line));
            out.push('\n');
        }
    }
    if !s.ends_with('\n') {
        out.pop();
    }
    // Only keep the fold if it actually shrank the output.
    if out.len() < s.len() {
        out
    } else {
        s.to_string()
    }
}

fn longest_common_prefix<'a>(lines: &[&'a str]) -> &'a str {
    let first = match lines.first() {
        Some(f) => *f,
        None => return "",
    };
    let mut end = first.len();
    for line in &lines[1..] {
        let common = first
            .bytes()
            .zip(line.bytes())
            .take(end)
            .take_while(|(a, b)| a == b)
            .count();
        end = end.min(common);
        if end == 0 {
            break;
        }
    }
    while end > 0 && !first.is_char_boundary(end) {
        end -= 1;
    }
    &first[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_colour_codes() {
        let input = "\x1b[31mERROR\x1b[0m: \x1b[1mboom\x1b[0m";
        assert_eq!(strip_ansi(input), "ERROR: boom");
    }

    #[test]
    fn strips_osc_with_st_terminator_no_stray_backslash() {
        // OSC 0 ; title ST  →  ESC ] 0 ; t i t l e ESC \
        let input = "\x1b]0;title\x1b\\done";
        assert_eq!(strip_ansi(input), "done");
    }

    #[test]
    fn strips_osc_with_bel_terminator() {
        let input = "\x1b]0;title\x07done";
        assert_eq!(strip_ansi(input), "done");
    }

    #[test]
    fn strips_ansi_but_keeps_unicode() {
        let input = "\x1b[32m✓ passed\x1b[0m — café";
        assert_eq!(strip_ansi(input), "✓ passed — café");
    }

    #[test]
    fn rstrips_trailing_whitespace_and_normalises_crlf() {
        assert_eq!(rstrip_lines("a   \r\nb\t\n"), "a\nb\n");
    }

    #[test]
    fn collapses_blank_line_runs() {
        assert_eq!(collapse_blank_runs("a\n\n\n\nb\n"), "a\n\nb\n");
    }

    #[test]
    fn collapses_identical_consecutive_lines() {
        let input = "warn: x\nwarn: x\nwarn: x\nok\n";
        assert_eq!(collapse_dup_runs(input), "warn: x (×3)\nok\n");
    }

    #[test]
    fn does_not_double_annotate_existing_dup_suffix() {
        let input = "retry (×3)\nretry (×3)\n";
        // Must NOT become "retry (×3) (×2)" — emitted verbatim instead.
        let out = collapse_dup_runs(input);
        assert_eq!(out, "retry (×3)\nretry (×3)\n");
        assert!(!out.contains("(×2)"));
    }

    #[test]
    fn dup_collapse_leaves_singletons_alone() {
        assert_eq!(collapse_dup_runs("a\nb\nc\n"), "a\nb\nc\n");
    }

    #[test]
    fn elides_long_base64_blob() {
        let blob = "A".repeat(2000);
        let input = format!("data: {blob} end");
        let out = elide_blobs(&input);
        assert!(out.contains("[@crush/1.blob 2000 chars]"));
        assert!(out.len() < input.len());
    }

    #[test]
    fn leaves_short_tokens_and_hashes_untouched() {
        let sha = "a".repeat(64);
        let input = format!("commit {sha}\n");
        assert_eq!(elide_blobs(&input), input);
    }

    #[test]
    fn folds_a_shared_path_prefix() {
        let mut input = String::new();
        for i in 0..10 {
            input.push_str(&format!("/home/haseeb/Projects/app/src/file{i}.rs\n"));
        }
        let out = fold_common_prefix(&input);
        assert!(out.starts_with("@crush/1.prefix=/home/haseeb/Projects/app/src/"));
        assert!(out.contains("file0.rs"));
        assert!(out.len() < input.len());
    }

    #[test]
    fn does_not_fold_when_too_few_lines() {
        let input = "/a/b/c/one\n/a/b/c/two\n";
        assert_eq!(fold_common_prefix(input), input);
    }
}
