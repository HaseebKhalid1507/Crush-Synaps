//! The transform suite. Each transform is `&str -> Option<String>`: it returns
//! `Some(smaller)` when it can shrink the output, or `None` when it doesn't
//! apply. [`run`] tries them all and keeps the smallest result.

mod tabular;

/// A named transform over tool output.
struct Transform {
    name: &'static str,
    apply: fn(&str) -> Option<String>,
}

/// Registry — order doesn't matter; [`run`] keeps the smallest winner.
const TRANSFORMS: &[Transform] = &[Transform {
    name: "tabular",
    apply: tabular::fold,
}];

/// Run every transform and return the smallest result strictly smaller than the
/// input, or `None` if nothing helped.
pub fn run(output: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for t in TRANSFORMS {
        if let Some(candidate) = (t.apply)(output) {
            if candidate.len() < output.len() {
                let better = best
                    .as_ref()
                    .map(|b| candidate.len() < b.len())
                    .unwrap_or(true);
                if better {
                    let _ = t.name; // reserved for future provenance/telemetry
                    best = Some(candidate);
                }
            }
        }
    }
    best
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
    fn run_returns_none_on_plain_text() {
        assert!(run("just some plain text output\nwith lines\n").is_none());
    }
}
