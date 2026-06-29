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
    collapse_dup_runs(&collapse_blank_runs(&rstrip_lines(&strip_ansi(s))))
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
