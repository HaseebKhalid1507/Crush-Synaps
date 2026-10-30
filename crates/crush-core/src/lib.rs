//! crush-core: make tool output cheaper to carry in a model's context without
//! changing what the model can read out of it.
//!
//! One pure function, [`compress`]: no I/O, no global state, no clocks. The
//! host (Synaps, the `crush` CLI, the extension shim) owns everything with a
//! side effect, in particular keeping the raw output when a result is lossy.
//!
//! Every result goes through the same gate ([`compress`] enforces it, the
//! transforms can't opt out):
//! - **CONTENT is byte-identical.** No transform is registered for it, and the
//!   gate re-checks equality anyway.
//! - **Never larger** in estimated tokens, under *both* tokenizer families, with
//!   a margin (wider for lossy results): a marginal gain passes the output through.
//! - **Idempotent**: the result is compressed once more; if that would change it
//!   again, the original passes through. So `compress(compress(x)) ==
//!   compress(x)` holds by construction.
//! - **Deterministic**: no hashing order, no time, no randomness.
//! - **Hands off marker-like input**: output that already carries a crush or
//!   host marker, or looks like a role/tool boundary, passes through.
//! - **Pass-through on doubt**: oversized input, a transform that bails, or a
//!   panic (caught) all return the raw text, borrowed.
//!
//! Lossy results (`lossless == false`) are only produced when the host passes
//! [`Input::raw_ref`], i.e. promises to keep the raw output there. Markers in
//! the text point at that reference.

#![forbid(unsafe_code)]

use std::borrow::Cow;

pub mod classify;
pub mod estimate;
mod guard;
pub mod shell;
pub mod transforms;

pub use classify::{classify, Class, Classification};
pub use estimate::{estimate_tokens, Tokenizer};

/// One tool result, as the host sees it after the tool ran.
#[derive(Debug, Clone, Copy, Default)]
pub struct Input<'a> {
    /// The tool's runtime name (`bash`, `read`, `grep`, `ext__web-tools__fetch`, ...).
    pub tool: &'a str,
    /// The shell command for shell-like tools (`input.command`), else `None`.
    pub command: Option<&'a str>,
    /// The tool's text output.
    pub output: &'a str,
    /// Where the host will keep the raw output if the outcome is lossy (e.g. the
    /// spill path from SynapsCLI #334). `None` disables every lossy transform.
    /// Markers quote it, so it should be stable for a given result.
    pub raw_ref: Option<&'a str>,
}

/// Per-class switches. A disabled class always passes through.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClassSwitches {
    /// Reserved: CONTENT has no transform in this version (byte-identical).
    pub content: bool,
    pub search: bool,
    pub listing: bool,
    pub log: bool,
    pub structured: bool,
    pub vcs: bool,
    pub prose: bool,
    pub unknown: bool,
}

impl Default for ClassSwitches {
    fn default() -> Self {
        Self {
            content: false,
            search: true,
            listing: true,
            log: true,
            structured: true,
            vcs: true,
            // Documents and agent/host messages: left alone for now.
            prose: false,
            unknown: true,
        }
    }
}

impl ClassSwitches {
    pub fn get(&self, c: Class) -> bool {
        match c {
            Class::Content => self.content,
            Class::Search => self.search,
            Class::Listing => self.listing,
            Class::Log => self.log,
            Class::Structured => self.structured,
            Class::Vcs => self.vcs,
            Class::Prose => self.prose,
            Class::Unknown => self.unknown,
        }
    }
}

/// Per-transform switches. Defaults reflect the accuracy gate: a transform
/// that failed parity ships disabled (`search_group`, `ls_columns`, see
/// eval/RESULTS.md).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TransformSwitches {
    /// Grep/rg hits grouped under a full-path heading (SEARCH). Off: on real
    /// grep output it cut ~20% of SEARCH tokens but Sonnet and Haiku attributed
    /// hits to the wrong file / line / count more often (eval/RESULTS.md).
    pub search_group: bool,
    /// `\r` progress rewrites collapsed to the final visible line (lossy).
    pub progress: bool,
    /// Runs of identical lines written once plus a counted note.
    pub repeats: bool,
    /// Long base64 / hex / minified blobs elided (lossy).
    pub blobs: bool,
    /// SEARCH hit lines over 1 KiB cut to the text around the match, with
    /// exact cut counts and a raw pointer (lossy). On: at parity on Haiku and
    /// Sonnet over two rounds (eval/RESULTS.md, Phase 2A).
    pub search_elide: bool,
    /// LISTING `ls -l` owner/group said once in words when (nearly) every row
    /// has the same pair (lossless). Off: failed accuracy parity with both
    /// note wordings (models answered "unknown" for the owner and read the
    /// neighbouring row's date; eval/RESULTS.md, Phase 2A).
    pub ls_columns: bool,
    /// LOG: runs of passing-test lines become a counted note (lossy).
    pub test_pass: bool,
    /// LOG: runs of build-progress lines (`Compiling`, pip `Collecting`, ...)
    /// become a counted note (lossy).
    pub build_progress: bool,
    /// STRUCTURED: pretty-printed JSON re-laid out one top-level element per
    /// line, no other whitespace (lossless in meaning).
    pub json_minify: bool,
}

impl Default for TransformSwitches {
    fn default() -> Self {
        Self {
            search_group: false,
            progress: true,
            repeats: true,
            blobs: true,
            search_elide: true,
            ls_columns: false,
            test_pass: true,
            build_progress: true,
            json_minify: true,
        }
    }
}

/// Compression settings; deserialisable from the host's config.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Master switch.
    pub enabled: bool,
    pub classes: ClassSwitches,
    pub transforms: TransformSwitches,
    /// Tokenizer family `tokens_before`/`tokens_after` are reported in. The
    /// never-larger gate always checks both families.
    pub tokenizer: Tokenizer,
    /// Outputs shorter than this pass through (nothing worth the risk).
    pub min_bytes: usize,
    /// Outputs longer than this pass through (bounded time).
    pub max_bytes: usize,
    /// A result must save at least this many estimated tokens...
    pub min_gain_tokens: u32,
    /// ...and at least this fraction of the estimated input tokens.
    pub min_gain_ratio: f32,
    /// A lossy result costs the host a kept raw and adds a pointer marker
    /// (a token-dense path), so it must save at least this many tokens...
    pub min_gain_tokens_lossy: u32,
    /// ...and this fraction.
    pub min_gain_ratio_lossy: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            classes: ClassSwitches::default(),
            transforms: TransformSwitches::default(),
            tokenizer: Tokenizer::default(),
            min_bytes: 256,
            max_bytes: 512 * 1024,
            min_gain_tokens: 8,
            min_gain_ratio: 0.02,
            min_gain_tokens_lossy: 64,
            min_gain_ratio_lossy: 0.10,
        }
    }
}

/// Why a result came out the way it did (telemetry / `--explain`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// At least one transform applied and the gate accepted it.
    Compressed,
    Disabled,
    ClassDisabled,
    TooSmall,
    TooLarge,
    /// CONTENT (or another byte-identical class): never changed.
    ByteIdentical,
    /// The output carries a crush/host marker or a role/tool boundary.
    MarkerLike,
    /// No transform found anything to do.
    NothingToDo,
    /// Transforms ran but the estimated gain was below the margin.
    NoGain,
    /// The result would compress again (idempotence check failed).
    NotIdempotent,
    /// A transform panicked; caught.
    InternalError,
}

impl Why {
    pub fn as_str(self) -> &'static str {
        match self {
            Why::Compressed => "compressed",
            Why::Disabled => "disabled",
            Why::ClassDisabled => "class-disabled",
            Why::TooSmall => "too-small",
            Why::TooLarge => "too-large",
            Why::ByteIdentical => "byte-identical-class",
            Why::MarkerLike => "marker-like-input",
            Why::NothingToDo => "nothing-to-do",
            Why::NoGain => "no-gain",
            Why::NotIdempotent => "not-idempotent",
            Why::InternalError => "internal-error",
        }
    }
}

/// The result of [`compress`].
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome<'a> {
    /// What the model should see. Borrowed (zero-copy) on pass-through.
    pub text: Cow<'a, str>,
    pub class: Class,
    /// Transforms that applied, in order. Empty on pass-through.
    pub transforms: Vec<&'static str>,
    /// `false` means the raw output can't be rebuilt from `text`: the host
    /// **must** keep it at the `raw_ref` it passed in.
    pub lossless: bool,
    /// Estimated tokens of the input and of `text`, in `Config::tokenizer`.
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub why: Why,
}

impl Outcome<'_> {
    /// True when `text` differs from the input.
    pub fn changed(&self) -> bool {
        matches!(self.text, Cow::Owned(_))
    }
}

/// Compress one tool result. Never panics; never returns something larger (in
/// estimated tokens) than the input; CONTENT always comes back borrowed.
pub fn compress<'a>(input: &Input<'a>, cfg: &Config) -> Outcome<'a> {
    let cls = classify(input.tool, input.command, input.output);
    compress_classified(input, cfg, cls.class)
}

/// [`compress`] with the class already decided (the host may have classified
/// for telemetry already; the gate is identical).
pub fn compress_classified<'a>(input: &Input<'a>, cfg: &Config, class: Class) -> Outcome<'a> {
    let out = input.output;
    let pass = |why: Why| Outcome {
        text: Cow::Borrowed(out),
        class,
        transforms: Vec::new(),
        lossless: true,
        tokens_before: 0,
        tokens_after: 0,
        why,
    };
    let early = if !cfg.enabled {
        Some(Why::Disabled)
    } else if class.byte_identical() {
        Some(Why::ByteIdentical)
    } else if !cfg.classes.get(class) {
        Some(Why::ClassDisabled)
    } else if out.len() < cfg.min_bytes {
        Some(Why::TooSmall)
    } else if out.len() > cfg.max_bytes {
        Some(Why::TooLarge)
    } else {
        None
    };
    if let Some(why) = early {
        let t = estimate_tokens(out, cfg.tokenizer);
        return Outcome { tokens_before: t, tokens_after: t, ..pass(why) };
    }
    if guard::marker_like(out) {
        let t = estimate_tokens(out, cfg.tokenizer);
        return Outcome { tokens_before: t, tokens_after: t, ..pass(Why::MarkerLike) };
    }
    let run = std::panic::catch_unwind(|| gate(input, cfg, class));
    match run {
        Ok(Ok(o)) => o,
        Ok(Err(why)) => {
            let t = estimate_tokens(out, cfg.tokenizer);
            Outcome { tokens_before: t, tokens_after: t, ..pass(why) }
        }
        Err(_) => pass(Why::InternalError),
    }
}

/// Run the class pipeline, then the invariant checks.
fn gate<'a>(input: &Input<'a>, cfg: &Config, class: Class) -> Result<Outcome<'a>, Why> {
    let out = input.output;
    let Some(applied) = transforms::run(class, out, input.command, input.raw_ref, cfg) else {
        return Err(Why::NothingToDo);
    };
    // Belt and braces: byte-identical classes never reach here.
    if class.byte_identical() || applied.text == out {
        return Err(Why::NothingToDo);
    }
    // SEARCH: every line must rebuild byte for byte (`heading` + sep + line).
    // (`transforms::run` checks every step against its own input; this is the
    // end-to-end check when grouping ran alone.)
    if applied.names == [transforms::search::NAME] && !transforms::search::rebuilds(out, &applied.text) {
        return Err(Why::InternalError);
    }
    // Never larger, under both tokenizer families, with a margin.
    let f_in = estimate::features(out);
    let f_out = estimate::features(&applied.text);
    let mut ok = true;
    for tk in [Tokenizer::V5, Tokenizer::V4] {
        let before = estimate::from_features(&f_in, tk);
        let after = estimate::from_features(&f_out, tk);
        let mut need = (cfg.min_gain_tokens as f64).max(before as f64 * cfg.min_gain_ratio as f64).max(1.0);
        if !applied.lossless {
            need = need.max(cfg.min_gain_tokens_lossy as f64).max(before as f64 * cfg.min_gain_ratio_lossy as f64);
        }
        if (after as f64) + need > before as f64 {
            ok = false;
        }
    }
    if !ok {
        return Err(Why::NoGain);
    }
    // Idempotence: the result must be a fixed point of `compress` itself.
    // Re-classify (sniffing sees the new text) and mirror every pass-through
    // rule; if the pipeline would still change it, give up on this result.
    let y = applied.text.as_str();
    let again = classify(input.tool, input.command, y).class;
    let would_change = cfg.enabled
        && !again.byte_identical()
        && cfg.classes.get(again)
        && y.len() >= cfg.min_bytes
        && y.len() <= cfg.max_bytes
        && !guard::marker_like(y)
        && transforms::run(again, y, input.command, input.raw_ref, cfg).is_some_and(|t| t.text != y);
    if would_change {
        return Err(Why::NotIdempotent);
    }
    Ok(Outcome {
        tokens_before: estimate::from_features(&f_in, cfg.tokenizer),
        tokens_after: estimate::from_features(&f_out, cfg.tokenizer),
        text: Cow::Owned(applied.text),
        class,
        transforms: applied.names,
        lossless: applied.lossless,
        why: Why::Compressed,
    })
}
