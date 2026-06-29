//! The transform suite.
//!
//! Three families, tried in priority order by [`run`]:
//! - **tool-aware** ([`columnar`] via [`schema`]) — when the producing tool is
//!   known (e.g. `ls`), fold its columnar output with a per-tool schema. Biggest
//!   wins, but only for recognised tools.
//! - **structural** ([`tabular`]) — JSON array-of-objects → schema + CSV.
//! - **textual** ([`text`]) — composed cleanup chain (ANSI, whitespace, runs)
//!   for logs and unstructured output.

mod columnar;
mod schema;
mod tabular;
mod text;

/// Run the transform suite for output produced by `tool_name` (with `command`
/// for bash sniffing). Returns the smallest result strictly smaller than the
/// input, or `None` if nothing helped.
pub fn run(tool_name: &str, command: &str, output: &str) -> Option<String> {
    // Tool-aware: a known columnar producer folds with its schema.
    if let Some(s) = schema::schema_for(tool_name, command) {
        if let Some(folded) = columnar::fold(output, s) {
            if folded.len() < output.len() {
                return Some(folded);
            }
        }
    }
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
        let out = run("bash", "echo", input).expect("tabular should win");
        assert!(out.len() < input.len());
    }

    #[test]
    fn run_cleans_noisy_log_text() {
        let input = "\x1b[31mfail\x1b[0m\nsame\nsame\nsame\nsame\nsame\n";
        let out = run("bash", "make", input).expect("text chain should win");
        assert!(out.len() < input.len());
        assert!(out.contains("same (×5)"));
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn run_folds_ls_output_tool_aware() {
        let mut input = String::from("total 1.9G\n");
        for i in 0..20 {
            input.push_str(&format!(
                "-rwxr-xr-x  1 root root    {:>3}K Jun 28 18:51 binary_{i}.sh\n",
                10 + i
            ));
        }
        let out = run("ls", "", &input).expect("columnar should win");
        assert!(out.len() < input.len());
        assert!(out.contains("@crush.cols"));
        assert!(out.contains("owner=root"));
    }

    #[test]
    fn run_returns_none_on_incompressible_text() {
        assert!(run("bash", "echo", "abc\ndef\nghi\n").is_none());
    }
}
