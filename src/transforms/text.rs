//! Text-cleanup transforms for unstructured tool output (build logs, test
//! runs, verbose traces). Unlike [`super::tabular`] these **compose** — strip
//! ANSI, then trailing whitespace, then blank-line runs, then duplicate-line
//! runs — each feeding the next. [`clean`] runs the whole chain.
//!
//! Meaning-preserving: color codes carry no information to an LLM, trailing
//! whitespace is invisible, and `(×N)` is a faithful, compact stand-in for N
//! identical lines.

/// Run the full text-cleanup chain.
pub fn clean(s: &str) -> String {
    let s = strip_ansi(s);
    let s = rstrip_lines(&s);
    let s = collapse_blank_runs(&s);
    let s = collapse_dup_runs(&s);
    let s = elide_blobs(&s);
    fold_common_prefix(&s)
}

/// Strip ANSI escape sequences (CSI colour/cursor codes, OSC strings).
pub fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            match bytes.get(i + 1) {
                // CSI: ESC [ ... <final 0x40..=0x7E>
                Some(b'[') => {
                    i += 2;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1; // consume final byte
                }
                // OSC: ESC ] ... (BEL | ESC \)
                Some(b']') => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != 0x07 {
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                // Other two-byte escapes — drop ESC + the next byte.
                Some(_) => i += 2,
                None => i += 1,
            }
        } else {
            // Copy one full UTF-8 char starting at i.
            let ch_len = utf8_len(bytes[i]);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(std::str::from_utf8(&bytes[i..end]).unwrap_or(""));
            i = end;
        }
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Strip trailing whitespace from every line. Normalises CRLF → LF.
pub fn rstrip_lines(s: &str) -> String {
    let mut out: String = s
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    if s.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Collapse runs of 2+ blank lines into a single blank line.
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

/// Collapse runs of 2+ identical consecutive lines into `line (×N)`.
pub fn collapse_dup_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.lines().peekable();
    while let Some(line) = iter.next() {
        let mut count = 1usize;
        while iter.peek() == Some(&line) {
            iter.next();
            count += 1;
        }
        out.push_str(line);
        if count > 1 {
            out.push_str(&format!(" (×{count})"));
        }
        out.push('\n');
    }
    if !s.ends_with('\n') {
        out.pop();
    }
    out
}

/// Elide long contiguous base64/hex-ish blobs (> 1 KiB) — embedded files, data
/// URIs, dumps the model can't use anyway. Conservative threshold leaves hashes,
/// tokens and short IDs untouched. This is the one *lossy* transform.
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
                out.push_str(&format!("[crush.blob {run} chars]"));
            } else {
                out.push_str(&s[start..i]);
            }
        } else {
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
/// `@crush.prefix=` header and strip it from each line. Lossless: prepend the
/// prefix to reconstruct. Only fires for 8+ lines and a 12+ char prefix.
pub fn fold_common_prefix(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let non_empty: Vec<&str> = lines.iter().copied().filter(|l| !l.is_empty()).collect();
    if non_empty.len() < 8 {
        return s.to_string();
    }
    let common = longest_common_prefix(&non_empty);
    // Cut at the last path separator so we only factor whole directories.
    let cut = match common.rfind('/') {
        Some(i) => i + 1,
        None => return s.to_string(),
    };
    if cut < 12 {
        return s.to_string();
    }
    let prefix = &common[..cut];
    let mut out = format!("@crush.prefix={prefix}\n");
    for (idx, line) in lines.iter().enumerate() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(line.strip_prefix(prefix).unwrap_or(line));
            out.push('\n');
        }
        let _ = idx;
    }
    if !s.ends_with('\n') {
        out.pop();
    }
    out
}

fn longest_common_prefix(lines: &[&str]) -> String {
    let first = match lines.first() {
        Some(f) => *f,
        None => return String::new(),
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
    // Floor to a UTF-8 boundary.
    while end > 0 && !first.is_char_boundary(end) {
        end -= 1;
    }
    first[..end].to_string()
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
    fn dup_collapse_leaves_singletons_alone() {
        assert_eq!(collapse_dup_runs("a\nb\nc\n"), "a\nb\nc\n");
    }

    #[test]
    fn elides_long_base64_blob() {
        let blob = "A".repeat(2000);
        let input = format!("data: {blob} end");
        let out = elide_blobs(&input);
        assert!(out.contains("[crush.blob 2000 chars]"));
        assert!(out.len() < input.len());
    }

    #[test]
    fn leaves_short_tokens_and_hashes_untouched() {
        // A 64-char sha256 must survive — it's information, not a blob.
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
        assert!(out.starts_with("@crush.prefix=/home/haseeb/Projects/app/src/"));
        assert!(out.contains("file0.rs"));
        assert!(!out.contains("/home/haseeb/Projects/app/src/file0.rs"));
        assert!(out.len() < input.len());
    }

    #[test]
    fn does_not_fold_when_too_few_lines() {
        let input = "/a/b/c/one\n/a/b/c/two\n";
        assert_eq!(fold_common_prefix(input), input);
    }

    #[test]
    fn does_not_fold_when_no_shared_prefix() {
        let mut input = String::new();
        for i in 0..10 {
            input.push_str(&format!("unique-line-{i}-no-common-root\n"));
        }
        // No '/' → no directory to factor.
        assert_eq!(fold_common_prefix(&input), input);
    }

    #[test]
    fn full_chain_cleans_a_noisy_log() {
        let input = "\x1b[33mBUILD\x1b[0m   \n\n\n\
                     repeat\nrepeat\nrepeat\nrepeat\n\n\ndone   \n";
        let out = clean(input);
        assert!(out.contains("BUILD\n"));
        assert!(out.contains("repeat (×4)"));
        assert!(out.contains("done\n"));
        assert!(out.len() < input.len());
        assert!(!out.contains('\x1b'));
    }
}
