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
    "spaces_in_runs",
    "newlines",
    "nonascii",
    "mixed_runs",
    "mixed_chars",
];

// byte classes
const L: u8 = 0; // lowercase ASCII letter
const U: u8 = 1; // uppercase ASCII letter
const D: u8 = 2; // digit
const S: u8 = 3; // space / tab
const N: u8 = 4; // newline
const P: u8 = 5; // ASCII punctuation / other control
const H: u8 = 6; // non-ASCII lead byte
const C: u8 = 7; // UTF-8 continuation byte

const fn table() -> [u8; 256] {
    let mut t = [P; 256];
    let mut i = 0;
    while i < 256 {
        let c = i as u8;
        t[i] = if c.is_ascii_lowercase() {
            L
        } else if c.is_ascii_uppercase() {
            U
        } else if c.is_ascii_digit() {
            D
        } else if c == b' ' || c == b'\t' {
            S
        } else if c == b'\n' {
            N
        } else if c >= 0xC0 {
            H
        } else if c >= 0x80 {
            C
        } else {
            P
        };
        i += 1;
    }
    t
}
static TABLE: [u8; 256] = table();

/// Letters past this length in one run cost extra (long identifiers split).
const LONG_WORD: u32 = 6;
/// Digits past this length in one run cost extra (numbers chunk by ~3).
const LONG_DIGITS: u32 = 3;

/// Shape features of `s`, one pass.
pub fn features(s: &str) -> Features {
    let mut f = [0u32; K];
    let b = s.as_bytes();
    f[0] = 1;
    f[1] = b.len() as u32;
    let mut letter_run = 0u32;
    let mut digit_run = 0u32;
    let mut space_run = 0u32;
    let mut punct_run = 0u32;
    let mut alnum_run = 0u32;
    let mut alnum_letters = false;
    let mut alnum_digits = false;
    let mut prev = N;
    for &byte in b {
        let k = TABLE[byte as usize];
        // close runs that end here
        if letter_run > 0 && k != L && k != U {
            f[3] += letter_run.saturating_sub(LONG_WORD);
            letter_run = 0;
        }
        if digit_run > 0 && k != D {
            f[7] += digit_run.saturating_sub(LONG_DIGITS);
            digit_run = 0;
        }
        if space_run > 0 && k != S {
            if space_run > 1 {
                f[10] += 1;
                f[11] += space_run;
            }
            space_run = 0;
        }
        if punct_run > 0 && k != P {
            punct_run = 0;
        }
        if alnum_run > 0 && !matches!(k, L | U | D) {
            if alnum_letters && alnum_digits {
                f[14] += 1;
                f[15] += alnum_run;
            }
            alnum_run = 0;
            alnum_letters = false;
            alnum_digits = false;
        }
        match k {
            L | U => {
                if letter_run == 0 {
                    f[2] += 1;
                } else if k == U && prev == L {
                    f[4] += 1;
                }
                if k == U {
                    f[5] += 1;
                }
                letter_run += 1;
                alnum_run += 1;
                alnum_letters = true;
            }
            D => {
                if digit_run == 0 {
                    f[6] += 1;
                }
                digit_run += 1;
                alnum_run += 1;
                alnum_digits = true;
            }
            S => space_run += 1,
            N => f[12] += 1,
            P => {
                f[8] += 1;
                if punct_run == 0 {
                    f[9] += 1;
                }
                punct_run += 1;
            }
            H => f[13] += 1,
            _ => {}
        }
        prev = k;
    }
    // close trailing runs
    f[3] += letter_run.saturating_sub(LONG_WORD);
    f[7] += digit_run.saturating_sub(LONG_DIGITS);
    if space_run > 1 {
        f[10] += 1;
        f[11] += space_run;
    }
    if alnum_run > 0 && alnum_letters && alnum_digits {
        f[14] += 1;
        f[15] += alnum_run;
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
        assert_eq!(f[6], 1);
        assert_eq!(f[7], 2); // 5 digits, 3 free
        assert_eq!(f[10], 1); // the double space
        assert_eq!(f[12], 1);
        assert_eq!(f[13], 1);
        assert_eq!(estimate_tokens("", Tokenizer::V5), 0);
    }

    #[test]
    fn monotone_in_repetition() {
        let a = "src/foo/bar.rs:12:    let x = y;\n";
        assert!(estimate_tokens(&a.repeat(10), Tokenizer::V5) > estimate_tokens(&a.repeat(5), Tokenizer::V5));
    }
}
