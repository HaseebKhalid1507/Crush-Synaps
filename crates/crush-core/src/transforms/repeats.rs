//! Collapse runs of identical lines.
//!
//! A run of N identical non-blank lines becomes the line once, followed by
//!
//! ```text
//! [crush: previous line repeated 44 more times; 45 identical lines in total]
//! ```
//!
//! Both numbers are written out in words, so neither "44" nor "45" has to be
//! inferred (models misread v0.1's `(×45)` as 1 or "unknown"). Lossless: the
//! run is rebuilt by repeating the line. Blank lines are never collapsed, and a
//! run is only collapsed when it is clearly longer than the note replacing it.

use super::{group_digits, lines};

pub const NAME: &str = "repeats";

/// Shortest run considered.
const MIN_RUN: usize = 3;

fn note(more: usize) -> String {
    format!(
        "[crush: previous line repeated {} more time{}; {} identical lines in total]",
        group_digits(more),
        if more == 1 { "" } else { "s" },
        group_digits(more + 1)
    )
}

pub fn collapse(text: &str) -> Option<String> {
    let (ls, trailing) = lines(text);
    let n = ls.len();
    if n < MIN_RUN {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut i = 0;
    while i < n {
        let l = ls[i];
        let mut j = i + 1;
        while j < n && ls[j] == l {
            j += 1;
        }
        let run = j - i;
        if i > 0 {
            out.push('\n');
        }
        out.push_str(l);
        let removed = (run - 1) * (l.len() + 1);
        if run >= MIN_RUN && !l.trim().is_empty() && removed > 2 * note(run - 1).len() {
            out.push('\n');
            out.push_str(&note(run - 1));
            changed = true;
        } else {
            for _ in 1..run {
                out.push('\n');
                out.push_str(l);
            }
        }
        i = j;
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
    fn collapses_long_runs_only() {
        let line = "waiting for the build lock to be released by another process";
        let raw = format!("start\n{}\nend\n", vec![line; 45].join("\n"));
        assert_eq!(
            collapse(&raw).unwrap(),
            format!("start\n{line}\n[crush: previous line repeated 44 more times; 45 identical lines in total]\nend\n")
        );
        assert_eq!(collapse("a\na\na\na\n"), None, "short line: note would not pay");
        assert_eq!(collapse(&"\n".repeat(50)), None, "blank runs stay");
    }
}
