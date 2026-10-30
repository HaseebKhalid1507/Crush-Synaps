//! What Synaps' spill step (SynapsCLI #334, `runtime/tool_spill.rs`) will do
//! with whatever the hook returns, so the shim only replaces when that doesn't
//! cost more after the spill than passing the raw through would.
//!
//! Synaps spills the *post-hook* text: a result over `tool_output_spill_bytes`
//! (default 12,288) goes whole to a spill file and history keeps the first
//! ~4 KiB + last ~3 KiB, cut at line boundaries, with a note naming the file.
//! [`preview`] is a byte-exact port of that cut (`Cut::new` + `spill_preview`);
//! only the spill path in the note is a stand-in of realistic length.
//!
//! The rule ([`decide`]):
//! - **No spill involved** (spill off, a tool Synaps never spills, or both
//!   texts under the threshold): replace. crush-core's gate already proved
//!   the crushed text cheaper.
//! - **Raw spills, crushed doesn't** (crush pulled it under the threshold):
//!   the whole crushed text lands in history instead of a ~7 KiB preview.
//!   Replace only if its estimated tokens are ≤ the preview's.
//! - **Both spill**: the preview costs about the same either way. Replace
//!   (spill the crushed text) when its preview is no costlier: the same byte
//!   window then covers more of the output (collapsed runs leave room for
//!   what's around them, failures and warnings included), a later `read` of
//!   the spill file costs fewer tokens, and the crush markers in it still
//!   point at the raw the shim keeps, so nothing is lost.
//! - **Exception, both cases, LOG only**: replace anyway when the crushed
//!   history view shows more failure lines (FAILED, panicked, ✕, --- FAIL,
//!   Traceback, `error:`, assertions) than the raw's preview would. A test log
//!   whose failures sit in the omitted middle is the case test collapse exists
//!   for. (In grep hits or listings those words are just text.)
//!
//! Token comparisons use crush-core's estimator under both tokenizer
//! families, like its own never-larger gate.

use crate::config::SpillPolicy;
use crush_core::estimate::{features, from_features};
use crush_core::Tokenizer;
use std::borrow::Cow;

/// Builtins Synaps never spills (they keep the legacy cut): everything
/// builtin outside its `SPILL_BUILTINS`. `read` gets a preview that points
/// back at the file, but crush never changes `read` (CONTENT). Any other name
/// (bash, grep, extension, MCP and plugin tools) is assumed to spill, which
/// is the conservative side: it only makes the shim replace less often.
const NEVER_SPILL: &[&str] = &[
    "read",
    "write",
    "edit",
    "load_skill",
    "activate_tools",
    "search_tools",
    "forum_read",
    "forum_post",
    "forum_forget",
    "memory_context",
    "memory_fetch",
    "memory_forget",
    "memory_search",
    "memory_store",
    "subagent_models",
    "subagent_model_authorize",
    "subagent_steer",
];

pub fn tool_spills(runtime_name: &str) -> bool {
    !NEVER_SPILL.contains(&runtime_name)
}

/// Where Synaps cuts `text`: `text[..head_end]` and `text[tail_start..]`.
struct Cut {
    head_end: usize,
    tail_start: usize,
    total_lines: usize,
    omitted_first: usize,
    omitted_last: usize,
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn newlines(b: &[u8]) -> usize {
    memchr::memchr_iter(b'\n', b).count()
}

impl Cut {
    fn new(text: &str, head: usize, tail: usize) -> Option<Self> {
        let len = text.len();
        if head.saturating_add(tail) >= len {
            return None;
        }
        let bytes = text.as_bytes();
        let mut head_end = floor_char_boundary(text, head);
        if let Some(nl) = memchr::memrchr(b'\n', &bytes[..head_end]) {
            if 2 * (nl + 1) >= head_end {
                head_end = nl + 1;
            }
        }
        let mut tail_start = ceil_char_boundary(text, len - tail);
        if tail_start < len && bytes[tail_start - 1] != b'\n' {
            if let Some(off) = memchr::memchr(b'\n', &bytes[tail_start..]) {
                let line_start = tail_start + off + 1;
                if 2 * (len - line_start) >= len - tail_start {
                    tail_start = line_start;
                }
            }
        }
        // Synaps debug_asserts this; never index past it here.
        if head_end >= tail_start || tail_start == 0 {
            return None;
        }
        let before = newlines(&bytes[..head_end]);
        let within = newlines(&bytes[head_end..tail_start - 1]);
        let after = newlines(&bytes[tail_start - 1..]);
        Some(Self {
            head_end,
            tail_start,
            total_lines: before + within + after + usize::from(!text.ends_with('\n')),
            omitted_first: before + 1,
            omitted_last: before + within + 1,
        })
    }
}

/// The preview Synaps keeps for `text` (spill file at `path`), or `None`
/// when head + tail already cover it.
pub fn preview(text: &str, head: usize, tail: usize, path: &str) -> Option<String> {
    let cut = Cut::new(text, head, tail)?;
    let (h, t) = (&text[..cut.head_end], &text[cut.tail_start..]);
    let (first, last) = (cut.omitted_first, cut.omitted_last);
    let mut out = String::with_capacity(h.len() + t.len() + 320 + path.len());
    out.push_str(h);
    if !h.is_empty() && !h.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("[... lines {first}-{last} omitted ...]\n"));
    out.push_str(t);
    if !t.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&format!(
        "[Output virtualised: {} bytes, {} lines; lines {first}-{last} are omitted above. \
         Full output: {path} (read it with offset={} limit={}, or grep it).]",
        text.len(),
        cut.total_lines,
        first - 1,
        last - first + 1,
    ));
    Some(out)
}

/// What history carries for `text` once Synaps' spill step has run.
pub fn history_view<'a>(text: &'a str, p: &SpillPolicy, path: &str) -> Cow<'a, str> {
    if p.threshold == 0 || text.len() <= p.threshold {
        return Cow::Borrowed(text);
    }
    preview(text, p.head, p.tail, path).map_or(Cow::Borrowed(text), Cow::Owned)
}

/// Lines a reader would call a failure (the eval's spill report uses the
/// same list). crush's own notes don't count.
pub fn failure_lines(text: &str) -> usize {
    text.lines().filter(|l| is_failure(l)).count()
}

fn is_failure(l: &str) -> bool {
    if l.starts_with("[crush:") {
        return false;
    }
    if l.contains("FAILED")
        || l.contains("panicked at")
        || l.contains('✕')
        || l.contains('✖')
        || l.contains("--- FAIL")
        || l.contains("Traceback (most recent")
        || l.contains("assertion")
        || l.contains("AssertionError")
    {
        return true;
    }
    if let Some(r) = l.strip_prefix("FAIL") {
        // `^FAIL\b`
        if !r.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            return true;
        }
    }
    if let Some(r) = l.strip_prefix("error") {
        // `^error(\[E\d+\])?:`
        if r.starts_with(':') {
            return true;
        }
        if let Some(code) = r.strip_prefix("[E") {
            if let Some((d, rest)) = code.split_once(']') {
                return !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) && rest.starts_with(':');
            }
        }
    }
    false
}

/// Estimated tokens under both families: (5.x, 4.x).
fn tokens(s: &str) -> (u32, u32) {
    let f = features(s);
    (from_features(&f, Tokenizer::V5), from_features(&f, Tokenizer::V4))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// No spill involved: crush-core's gate decides.
    NoSpill,
    /// A spill is involved and the crushed history view is no costlier.
    NoCostlier,
    /// Costlier after spill, but it keeps failure lines the preview loses.
    KeepsFailures,
    /// Costlier after spill: pass through.
    Costlier,
}

impl Why {
    pub fn as_str(self) -> &'static str {
        match self {
            Why::NoSpill => "no-spill",
            Why::NoCostlier => "spill-no-costlier",
            Why::KeepsFailures => "spill-keeps-failures",
            Why::Costlier => "spill-costlier",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub replace: bool,
    pub why: Why,
    /// Estimated tokens (in `tk`) history carries: passing through vs replacing.
    /// `None` when no spill is involved (the caller has crush-core's numbers).
    pub history: Option<(u32, u32)>,
}

/// Replace `raw` with `crushed` for `tool`? See the module docs.
///
/// `log`: the result is a build/test log (crush-core's LOG class). Only there
/// does a failure line mean a failure; a grep hit for `FAILED` doesn't.
pub fn decide(
    tool: &str,
    raw: &str,
    crushed: &str,
    p: &SpillPolicy,
    spill_path: &str,
    tk: Tokenizer,
    log: bool,
) -> Decision {
    let involved = p.threshold > 0 && tool_spills(tool) && (raw.len() > p.threshold || crushed.len() > p.threshold);
    if !involved {
        return Decision { replace: true, why: Why::NoSpill, history: None };
    }
    let vr = history_view(raw, p, spill_path);
    let vc = history_view(crushed, p, spill_path);
    let (r5, r4) = tokens(&vr);
    let (c5, c4) = tokens(&vc);
    let pick = |v5: u32, v4: u32| if tk == Tokenizer::V4 { v4 } else { v5 };
    let history = Some((pick(r5, r4), pick(c5, c4)));
    let (replace, why) = if c5 <= r5 && c4 <= r4 {
        (true, Why::NoCostlier)
    } else if log && failure_lines(&vc) > failure_lines(&vr) {
        (true, Why::KeepsFailures)
    } else {
        (false, Why::Costlier)
    };
    Decision { replace, why, history }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: SpillPolicy = SpillPolicy { threshold: 12288, head: 4096, tail: 3072 };
    const PATH: &str = "/home/user/.synaps-cli/spill/session-1/toolu_01ABCDEFGHIJKLMNOPQRSTUVWX.txt";

    fn lines(n: usize, width: usize) -> String {
        (1..=n)
            .map(|i| {
                let label = format!("line {i:05} ");
                format!("{label}{}\n", "x".repeat(width - label.len() - 1))
            })
            .collect()
    }

    #[test]
    fn preview_matches_synaps_byte_for_byte() {
        // SynapsCLI tool_spill.rs `cuts_land_on_line_boundaries_and_name_the_omitted_lines`
        let text = lines(200, 50);
        let head = lines(20, 50);
        let tail = &text[text.len() - 500..];
        let expected = format!(
            "{head}[... lines 21-190 omitted ...]\n{tail}\n\
             [Output virtualised: 10000 bytes, 200 lines; lines 21-190 are omitted above. \
             Full output: /s/sess-1/toolu_x.txt (read it with offset=20 limit=170, or grep it).]"
        );
        assert_eq!(preview(&text, 1030, 520, "/s/sess-1/toolu_x.txt").unwrap(), expected);
        assert_eq!(history_view(&text, &SpillPolicy { threshold: 10000, ..P }, PATH), text, "threshold is exclusive");
        assert_ne!(history_view(&text, &SpillPolicy { threshold: 9999, ..P }, PATH), text);
    }

    #[test]
    fn a_long_single_line_is_cut_mid_line_on_char_boundaries() {
        let text = "é🦀".repeat(5000);
        let p = preview(&text, 1001, 999, PATH).unwrap();
        assert!(p.contains("[... lines 1-1 omitted ...]"));
        assert!(p.starts_with(&text[..floor_char_boundary(&text, 1001)]));
    }

    #[test]
    fn no_spill_means_replace() {
        let d = decide("bash", "a\n", "b\n", &P, PATH, Tokenizer::V5, false);
        assert_eq!((d.replace, d.why), (true, Why::NoSpill));
        let big = lines(400, 80);
        let d = decide("memory_fetch", &big, "small", &P, PATH, Tokenizer::V5, false);
        assert_eq!(d.why, Why::NoSpill, "a builtin Synaps never spills");
        let d = decide("bash", &big, "small", &SpillPolicy { threshold: 0, ..P }, PATH, Tokenizer::V5, false);
        assert_eq!(d.why, Why::NoSpill, "spill off");
    }

    #[test]
    fn crushed_just_under_the_threshold_must_not_cost_more_than_the_preview() {
        let raw = lines(170, 80); // 13.6 KB: spills, ~7 KB preview
        let crushed = lines(150, 80); // 12.0 KB: lands whole
        assert!(raw.len() > P.threshold && crushed.len() <= P.threshold);
        let d = decide("bash", &raw, &crushed, &P, PATH, Tokenizer::V5, true);
        assert_eq!((d.replace, d.why), (false, Why::Costlier));
        let (r, c) = d.history.unwrap();
        assert!(c > r);
        let small = lines(60, 80);
        let d = decide("bash", &raw, &small, &P, PATH, Tokenizer::V5, false);
        assert_eq!((d.replace, d.why), (true, Why::NoCostlier));
    }

    #[test]
    fn failures_in_the_omitted_middle_keep_the_replacement() {
        let mut raw = lines(60, 80);
        for i in 0..14 {
            raw.push_str(&format!("test m::t{i} ... FAILED\n"));
        }
        raw.push_str(&lines(100, 80));
        let mut crushed = lines(60, 80);
        for i in 0..14 {
            crushed.push_str(&format!("test m::t{i} ... FAILED\n"));
        }
        crushed.push_str(&lines(78, 80)[..]);
        assert!(crushed.len() <= P.threshold && raw.len() > P.threshold);
        assert_eq!(failure_lines(&history_view(&raw, &P, PATH)), 0);
        let d = decide("bash", &raw, &crushed, &P, PATH, Tokenizer::V5, true);
        assert_eq!((d.replace, d.why), (true, Why::KeepsFailures));
        // the same words in grep hits are just text: no exception
        let d = decide("grep", &raw, &crushed, &P, PATH, Tokenizer::V5, false);
        assert_eq!((d.replace, d.why), (false, Why::Costlier));
    }

    #[test]
    fn failure_line_shapes() {
        for l in [
            "test a::b ... FAILED",
            "thread 'x' panicked at src/a.rs:1:2:",
            "FAIL src/a.test.js",
            "--- FAIL: TestX (0.00s)",
            "error[E0308]: mismatched types",
            "error: could not compile `x`",
            "Traceback (most recent call last):",
            "  ✕ adds (3 ms)",
        ] {
            assert!(is_failure(l), "{l}");
        }
        for l in ["FAILURES", "errors: 0", "[crush: 3 FAILED lines]", "error[Exx]: no", "ok"] {
            assert!(!is_failure(l), "{l}");
        }
    }
}
