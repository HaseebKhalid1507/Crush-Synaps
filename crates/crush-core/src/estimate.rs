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
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                r == LO && len > LONG_WORD,   // letters_long
                r == DI && len > LONG_DIGITS, // digits_long (capped run: >3 still counts)
                r == SP && len == 2,          // space_runs (len >= 2)
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

/// Reference implementation (scalar, one pass): `features` must equal it.
pub fn features_scalar(s: &str) -> Features {
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

// ── fast path: byte flags in blocks, counted with u8 lanes (vectorises) ────

const F_LET: u8 = 1;
const F_UP: u8 = 2;
const F_LO: u8 = 4;
const F_DI: u8 = 8;
const F_SP: u8 = 16;
const F_NL: u8 = 32;
const F_PU: u8 = 64;
const F_HI: u8 = 128;

#[inline(always)]
fn flag(b: u8) -> u8 {
    let lo = b.wrapping_sub(b'a') < 26;
    let up = b.wrapping_sub(b'A') < 26;
    let di = b.wrapping_sub(b'0') < 10;
    let sp = (b == b' ') | (b == b'\t');
    let nl = b == b'\n';
    let hi = b >= 0xC0;
    let pu = (b < 0x80) & !(lo | up | di | sp | nl);
    (((lo | up) as u8) * F_LET)
        | ((up as u8) * F_UP)
        | ((lo as u8) * F_LO)
        | ((di as u8) * F_DI)
        | ((sp as u8) * F_SP)
        | ((nl as u8) * F_NL)
        | ((pu as u8) * F_PU)
        | ((hi as u8) * F_HI)
}

#[inline(always)]
fn sum1(a: &[u8], f: impl Fn(u8) -> bool) -> u32 {
    let mut acc = 0u8;
    for &x in a {
        acc = acc.wrapping_add(f(x) as u8);
    }
    acc as u32
}

#[inline(always)]
fn sum2(a: &[u8], b: &[u8], f: impl Fn(u8, u8) -> bool) -> u32 {
    let mut acc = 0u8;
    for (&x, &y) in a.iter().zip(b) {
        acc = acc.wrapping_add(f(x, y) as u8);
    }
    acc as u32
}

const BLOCK: usize = 255;
const BACK: usize = 7;

/// Shape features of `s`: identical to [`features_scalar`], faster on big text.
pub fn features(s: &str) -> Features {
    let bytes = s.as_bytes();
    if bytes.len() < 64 {
        return features_scalar(s);
    }
    let mut f = [0u32; K];
    f[0] = 1;
    f[1] = bytes.len() as u32;
    let mut buf = [0u8; BACK + BLOCK];
    buf[..BACK].fill(F_NL);
    let mut and_l = [0u8; BLOCK];
    for blk in bytes.chunks(BLOCK) {
        let n = blk.len();
        for (d, &b) in buf[BACK..BACK + n].iter_mut().zip(blk) {
            *d = flag(b);
        }
        let p = |k: usize| &buf[BACK - k..BACK - k + n];
        let c = p(0);
        let p1 = p(1);
        f[2] += sum2(c, p1, |x, y| (x & !y) & F_LET != 0);
        f[4] += sum2(c, p1, |x, y| (x & F_UP != 0) & (y & F_LO != 0));
        f[5] += sum1(c, |x| x & F_UP != 0);
        f[6] += sum2(c, p1, |x, y| (x & !y) & F_DI != 0);
        f[8] += sum1(c, |x| x & F_PU != 0);
        f[9] += sum2(c, p1, |x, y| (x & !y) & F_PU != 0);
        f[11] += sum2(c, p1, |x, y| (x & y) & F_SP != 0);
        f[12] += sum1(c, |x| x & F_NL != 0);
        f[13] += sum1(c, |x| x & F_HI != 0);
        f[14] += sum2(c, p1, |x, y| (x & F_DI != 0) & (y & F_LET != 0));
        f[15] += sum2(c, p1, |x, y| (x & F_LET != 0) & (y & F_DI != 0));
        // space runs of >= 2 start where two spaces follow a non-space
        let a = &mut and_l[..n];
        for ((d, &x), &y) in a.iter_mut().zip(c).zip(p1) {
            *d = x & y;
        }
        f[10] += sum2(a, p(2), |xy, z| (xy & !z) & F_SP != 0);
        // digits: 4 in a row; letters: 7 in a row
        for ((d, &y), &z) in a.iter_mut().zip(p(2)).zip(p(3)) {
            *d &= y & z;
        }
        f[7] += sum1(a, |x| x & F_DI != 0);
        for (((d, &y), &z), &w) in a.iter_mut().zip(p(4)).zip(p(5)).zip(p(6)) {
            *d &= y & z & w;
        }
        f[3] += sum1(a, |x| x & F_LET != 0);
        buf.copy_within(n..n + BACK, 0);
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
    fn fast_equals_scalar() {
        let samples = [
            "fooBar  12345\n\tx: ü".repeat(40),
            "src/foo/bar.rs:12:    let x = y;\n".repeat(30),
            "aaaaaaaaaaaa 123456789 a1b2c3 ABCdef\r\n  \t  ".repeat(25),
            (0..3000).map(|i| char::from_u32(32 + (i * 7919) % 5000).unwrap_or('x')).collect::<String>(),
        ];
        for s in &samples {
            for cut in [s.len(), s.len() / 3, 300, 256, 255, 254, 70] {
                let mut c = cut.min(s.len());
                while !s.is_char_boundary(c) {
                    c -= 1;
                }
                assert_eq!(features(&s[..c]), features_scalar(&s[..c]), "len {c}");
            }
        }
    }

    #[test]
    fn monotone_in_repetition() {
        let a = "src/foo/bar.rs:12:    let x = y;\n";
        assert!(estimate_tokens(&a.repeat(10), Tokenizer::V5) > estimate_tokens(&a.repeat(5), Tokenizer::V5));
    }
}
