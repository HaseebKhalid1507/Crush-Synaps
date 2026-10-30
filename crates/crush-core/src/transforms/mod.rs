//! Transforms and the per-class pipeline.
//!
//! A transform takes text and returns `Some(new_text)` only when it changed
//! something; it never decides whether the change is worth it (the gate in
//! `lib.rs` does, in tokens, for the whole pipeline).

use crate::{Class, Config};

pub mod blobs;
pub mod progress;
pub mod repeats;
pub mod search;

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
        // hit text must stay byte-exact: grouping only
        Class::Search => &[search::NAME],
        Class::Log | Class::Listing | Class::Vcs | Class::Structured | Class::Unknown | Class::Prose => {
            &[progress::NAME, repeats::NAME, blobs::NAME]
        }
    }
}

/// Run the class pipeline. `None` when nothing applied.
pub fn run(class: Class, text: &str, raw_ref: Option<&str>, cfg: &Config) -> Option<Applied> {
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
            _ => None,
        };
        if let Some(n) = next {
            if n != src {
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
