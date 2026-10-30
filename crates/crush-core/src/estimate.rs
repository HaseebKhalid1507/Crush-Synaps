//! Local token estimator.
//!
//! crush can't call `count_tokens` at runtime, so it estimates: one pass over
//! the bytes counts a few shape features (word pieces, long words, camel
//! humps, digit runs, punctuation, indentation, newlines, non-ASCII, mixed
//! alphanumeric runs like hashes), and a per-tokenizer linear model turns them
//! into tokens. The coefficients are fitted against real `count_tokens`
//! numbers (see `eval/estimator/`), and the fit's error is reported there.
//!
//! The never-larger gate compares estimates under both families with a margin,
//! so a small bias cancels and a marginal gain passes through.

/// Tokenizer families (measured 2026-10-30): 4.x (sonnet-4-6, haiku-4-5) and
/// 5.x (opus-5-5, sonnet-5-5). 5.x makes ~1.26x the tokens of 4.x.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Tokenizer {
    #[serde(rename = "4.x", alias = "v4")]
    V4,
    #[default]
    #[serde(rename = "5.x", alias = "v5")]
    V5,
}

pub const K: usize = 16;
pub type Features = [u32; K];

pub const FEATURE_NAMES: [&str; K] = [
    "one",
    "bytes",
    "letter_runs",
    "letters_long",
    "camel",
    "upper",
    "digit_runs",
    "digits_long",
    "punct",
    "punct_runs",
    "space_runs",
    "space_pairs",
    "newlines",
    "nonascii",
    "letter_digit",
    "digit_letter",
];

// byte classes
const LO: u8 = 0; // lowercase ASCII letter
const UP: u8 = 1; // uppercase ASCII letter
const DI: u8 = 2; // digit
const SP: u8 = 3; // space / tab
const NL: u8 = 4; // newline
const PU: u8 = 5; // ASCII punctuation / control
const HI: u8 = 6; // non-ASCII lead byte
const CO: u8 = 7; // UTF-8 continuation byte

const fn class_table() -> [u8; 256] {
    let mut t = [PU; 256];
    let mut i = 0;
    while i < 256 {
        let c = i as u8;
        t[i] = if c.is_ascii_lowercase() {
            LO
        } else if c.is_ascii_uppercase() {
            UP
        } else if c.is_ascii_digit() {
            DI
        } else if c == b' ' || c == b'\t' {
            SP
        } else if c == b'\n' {
            NL
        } else if c >= 0xC0 {
            HI
        } else if c >= 0x80 {
            CO
        } else {
            PU
        };
        i += 1;
    }
    t
}
static CLASS: [u8; 256] = class_table();

/// Every feature but the first two is a count of events that depend on the
/// (previous class, class) pair, or on (run class, run length so far). Each
/// event table entry packs its increments as 8-bit lanes of a u128, so one
/// wide add per table per byte counts all of them; lanes are flushed every 255
/// bytes. LANES[i] is the feature lane i counts.
const LANES: [usize; 14] = [2, 4, 5, 6, 8, 9, 11, 12, 13, 14, 15, 3, 7, 10];

const fn is_letter(c: u8) -> bool {
    c == LO || c == UP
}

/// Letters past this length in one run cost extra (long identifiers split).
const LONG_WORD: usize = 6;
/// Digits past this length in one run cost extra (numbers chunk by ~3).
const LONG_DIGITS: usize = 3;
/// Run lengths are tracked up to this (enough for LONG_WORD + 1).
const RUN_CAP: usize = 7;

const fn lanes(ev: &[bool]) -> u128 {
    let mut x = 0u128;
    let mut k = 0;
    while k < ev.len() {
        if ev[k] {
            x |= 1u128 << (8 * k);
        }
        k += 1;
    }
    x
}

const fn pair_table() -> [u128; 64] {
    let mut t = [0u128; 64];
    let mut p = 0;
    while p < 8 {
        let mut c = 0;
        while c < 8 {
            let (pu, cu) = (p as u8, c as u8);
            t[p * 8 + c] = lanes(&[
                is_letter(cu) && !is_letter(pu), // letter_runs
                cu == UP && pu == LO,            // camel
                cu == UP,                        // upper
                cu == DI && pu != DI,            // digit_runs
                cu == PU,                        // punct
                cu == PU && pu != PU,            // punct_runs
                cu == SP && pu == SP,            // space_pairs
                cu == NL,                        // newlines
                cu == HI,                        // nonascii
                cu == DI && is_letter(pu),       // letter_digit
                is_letter(cu) && pu == DI,       // digit_letter
            ]);
            c += 1;
        }
        p += 1;
    }
    t
}

/// Run classes: letters (either case) are one run class.
const fn run_class(c: u8) -> u8 {
    if c == UP {
        LO
    } else {
        c
    }
}

const fn run_table() -> [u128; 64] {
    let mut t = [0u128; 64];
    let mut rc = 0;
    while rc < 8 {
        let mut len = 1;
        while len <= RUN_CAP {
            let r = rc as u8;
            t[rc * 8 + len] = lanes(&[
                false, false, false, false, false, false, false, false, false, false, false,
                r == LO && len > LONG_WORD,    // letters_long
                r == DI && len > LONG_DIGITS,  // digits_long (capped run: >3 still counts)
                r == SP && len == 2,           // space_runs (len >= 2)
            ]);
            len += 1;
        }
        rc += 1;
    }
    t
}

static PAIR: [u128; 64] = pair_table();
static RUN: [u128; 64] = run_table();

fn flush(f: &mut Features, a: u128) {
    for (i, &k) in LANES.iter().enumerate() {
        f[k] += ((a >> (8 * i)) & 0xff) as u32;
    }
}

/// Shape features of `s`, one branch-free pass.
pub fn features(s: &str) -> Features {
    let mut f = [0u32; K];
    let bytes = s.as_bytes();
    f[0] = 1;
    f[1] = bytes.len() as u32;
    let mut prev = NL as usize;
    let mut prc = NL as usize;
    let mut run = 0usize;
    for chunk in bytes.chunks(255) {
        let mut a = 0u128;
        for &byte in chunk {
            let c = CLASS[byte as usize] as usize;
            let rc = run_class(c as u8) as usize;
            run = if rc == prc { (run + 1).min(RUN_CAP) } else { 1 };
            a += PAIR[(prev << 3) | c] + RUN[(rc << 3) | run];
            prev = c;
            prc = rc;
        }
        flush(&mut f, a);
    }
    f
}

// Fitted by eval/estimator/fit.py on real count_tokens (see the report there).
include!("estimate_coef.rs");

/// Estimated tokens from precomputed features.
pub fn from_features(f: &Features, tk: Tokenizer) -> u32 {
    let w = match tk {
        Tokenizer::V4 => &COEF_V4,
        Tokenizer::V5 => &COEF_V5,
    };
    if f[1] == 0 {
        return 0;
    }
    let mut t = 0.0f64;
    for i in 0..K {
        t += w[i] * f[i] as f64;
    }
    t.max(1.0).round() as u32
}

/// Estimated tokens of `s` in the given tokenizer family.
pub fn estimate_tokens(s: &str, tk: Tokenizer) -> u32 {
    from_features(&features(s), tk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_basic() {
        let f = features("fooBar  12345\n\tx: ü");
        assert_eq!(f[1], 20);
        assert_eq!(f[2], 2); // fooBar, x (ü is not ASCII)
        assert_eq!(f[4], 1); // camel hump
        assert_eq!(f[5], 1);
        assert_eq!(f[6], 1);
        assert_eq!(f[7], 2); // 5 digits, 3 free
        assert_eq!(f[10], 1); // the double space
        assert_eq!(f[11], 1);
        assert_eq!(f[12], 1);
        assert_eq!(f[13], 1);
        let g = features("ab12cd");
        assert_eq!((g[14], g[15]), (1, 1));
        let long = features(&"x".repeat(1000));
        assert_eq!(long[3], 994);
        assert_eq!(long[2], 1);
        assert_eq!(estimate_tokens("", Tokenizer::V5), 0);
    }

    #[test]
    fn monotone_in_repetition() {
        let a = "src/foo/bar.rs:12:    let x = y;\n";
        assert!(estimate_tokens(&a.repeat(10), Tokenizer::V5) > estimate_tokens(&a.repeat(5), Tokenizer::V5));
    }
}
