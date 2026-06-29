//! The transform suite.
//!
//! Two families:
//! - **structural** ([`tabular`]) — JSON array-of-objects → schema + CSV. Wins
//!   big when it applies, but only on JSON.
//! - **textual** ([`text`]) — a composed cleanup chain (ANSI strip, whitespace
//!   trim, run collapse) for logs, traces and other unstructured output.
//!
//! [`run`] picks the structural win when available, else the textual one.

mod tabular;
mod text;

/// Run the transform suite and return the smallest result strictly smaller than
/// the input, or `None` if nothing helped.
pub fn run(output: &str) -> Option<String> {
    // Structural: a JSON array-of-objects folds to schema + CSV.
    if let Some(folded) = tabular::fold(output) {
        if folded.len() < output.len() {
            return Some(folded);
        }
    }
    // Textual: compose the cleanup chain for everything else.
    let cleaned = text::clean(output);
    if cleaned.len() < output.len() {
        return Some(cleaned);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_folds_a_json_array() {
        let input = r#"[{"a":1,"b":2},{"a":3,"b":4},{"a":5,"b":6}]"#;
        let out = run(input).expect("tabular should win");
        assert!(out.len() < input.len());
    }

    #[test]
    fn run_cleans_noisy_log_text() {
        let input = "\x1b[31mfail\x1b[0m\nsame\nsame\nsame\nsame\nsame\n";
        let out = run(input).expect("text chain should win");
        assert!(out.len() < input.len());
        assert!(out.contains("same (×5)"));
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn run_returns_none_on_incompressible_text() {
        assert!(run("abc\ndef\nghi\n").is_none());
    }
}
