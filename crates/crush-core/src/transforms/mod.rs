//! Transforms and the per-class pipeline.
//!
//! A transform takes text and returns `Some(new_text)` only when it changed
//! something; it never decides whether the change is worth it (the gate in
//! `lib.rs` does, in tokens, for the whole pipeline).

use crate::{Class, Config};

pub mod blobs;
pub mod json;
pub mod listing;
pub mod log;
pub mod progress;
pub mod repeats;
pub mod search;
pub mod search_elide;

/// What the pipeline did.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub text: String,
    pub names: Vec<&'static str>,
    pub lossless: bool,
}

/// Ordered pipeline for a class. CONTENT has none, by design.
fn pipeline(class: Class) -> &'static [&'static str] {
    match class {
        Class::Content => &[],
        // hit text must stay byte-exact: lines are cut (with exact counts) or
        // grouped, never rewritten
        Class::Search => &[search_elide::NAME, search::NAME],
        // `ls -l` factoring last: its round trip is checked on its own input
        Class::Listing => &[progress::NAME, repeats::NAME, blobs::NAME, listing::NAME],
        // test/build collapse before the generic text chain
        Class::Log => &[progress::NAME, log::BUILD, log::TESTS, log::GROUP, repeats::NAME, blobs::NAME],
        // minify first: it parses the raw JSON, and blobs never touch JSON lines
        Class::Structured => &[json::NAME, progress::NAME, repeats::NAME, blobs::NAME],
        Class::Vcs | Class::Unknown | Class::Prose => &[progress::NAME, repeats::NAME, blobs::NAME],
    }
}

/// Is `next` exactly what `name` may make of `src`? Checked after every step
/// that has a checker, against that step's own input; a failure drops the
/// whole result (pass-through on doubt).
fn verify_step(name: &str, src: &str, next: &str) -> bool {
    match name {
        search::NAME => search::rebuilds(src, next),
        search_elide::NAME => search_elide::verify(src, next),
        listing::NAME => listing::verify(src, next),
        log::GROUP => log::expand_test_groups(next) == src,
        _ => true,
    }
}

/// Run the class pipeline. `None` when nothing applied. `command` is the
/// shell command for shell-like tools (some transforms read flags or the
/// grep pattern from it).
pub fn run(class: Class, text: &str, command: Option<&str>, raw_ref: Option<&str>, cfg: &Config) -> Option<Applied> {
    let mut cur: Option<String> = None;
    let mut names = Vec::new();
    let mut lossless = true;
    for &name in pipeline(class) {
        let src = cur.as_deref().unwrap_or(text);
        let (on, lossy) = match name {
            search::NAME => (cfg.transforms.search_group, false),
            progress::NAME => (cfg.transforms.progress, true),
            repeats::NAME => (cfg.transforms.repeats, false),
            blobs::NAME => (cfg.transforms.blobs, true),
            search_elide::NAME => (cfg.transforms.search_elide, true),
            listing::NAME => (cfg.transforms.ls_columns, false),
            log::TESTS => (cfg.transforms.test_pass, true),
            log::BUILD => (cfg.transforms.build_progress, true),
            log::GROUP => (cfg.transforms.test_group, false),
            json::NAME => (cfg.transforms.json_minify, false),
            _ => (false, false),
        };
        if !on || (lossy && raw_ref.is_none()) {
            continue;
        }
        let next = match name {
            search::NAME => search::group(src),
            progress::NAME => progress::collapse(src, raw_ref.unwrap_or("")),
            repeats::NAME => repeats::collapse(src),
            blobs::NAME => blobs::elide(src, raw_ref.unwrap_or("")),
            search_elide::NAME => search_elide::elide(src, command, raw_ref.unwrap_or("")),
            listing::NAME => listing::factor(src, command),
            log::TESTS => log::collapse_tests(src, raw_ref.unwrap_or("")),
            log::BUILD => log::collapse_build(src, raw_ref.unwrap_or("")),
            log::GROUP => log::group_tests(src),
            json::NAME => json::minify(src),
            _ => None,
        };
        if let Some(n) = next {
            if n != src {
                if !verify_step(name, src, &n) {
                    return None;
                }
                cur = Some(n);
                names.push(name);
                lossless &= !lossy;
            }
        }
    }
    cur.map(|text| Applied { text, names, lossless })
}

/// Split into lines, keeping whether the text ended with `\n`.
pub(crate) fn lines(s: &str) -> (Vec<&str>, bool) {
    let trailing = s.ends_with('\n');
    let body = if trailing { &s[..s.len() - 1] } else { s };
    (body.split('\n').collect(), trailing)
}

/// Thousands separators for counts in markers (`12,345`).
pub(crate) fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
