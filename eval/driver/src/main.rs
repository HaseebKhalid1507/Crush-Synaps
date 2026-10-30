//! crush-eval-driver: JSONL samples on stdin (`{"id","tool","input","output"}`),
//! crush-core in-process, one JSONL result per sample on stdout.
//!
//! Default mode: `crush_core::compress` exactly as a host calls it (with a
//! spill-path-like `raw_ref`, so lossy transforms run and their markers cost
//! what they will cost in Synaps). Result fields: id, class, reason, fired,
//! why, transforms, lossless, in_bytes, out_bytes, est_{v5,v4}_{in,out}, ns
//! (best of `--reps`), panicked, idempotent, output (only when changed).
//!
//! `--features`: the estimator's feature vector of each sample's `output`
//! (and of `alt` if present) instead; used to fit/check the estimator.
//! `--style blank,colon,inline`: SEARCH layout variants (measurement only).
use crush_core::{classify, compress, estimate, Config, Input, Tokenizer};
use std::io::{self, BufRead, BufWriter, Write};
use std::time::Instant;

pub const RAW_REF: &str = "/home/user/.synaps-cli/spill/session-1497513-1793340440887-49/toolu_018HPHz1D9QgMpJsQMfa9R3f.txt";

fn arg(name: &str) -> Option<String> {
    std::env::args().skip_while(|a| a != name).nth(1)
}

fn main() {
    let reps: usize = arg("--reps").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let features = std::env::args().any(|a| a == "--features");
    let style = arg("--style").map(|s| crush_core::transforms::search::Style {
        blank_between: s.contains("blank"),
        colon: s.contains("colon"),
        group_singles: !s.contains("inline"),
    });
    let mut cfg = Config::default();
    if let Some(c) = arg("--config") {
        cfg = serde_json::from_str(&std::fs::read_to_string(c).expect("config")).expect("config json");
    }
    std::panic::set_hook(Box::new(|_| {}));
    let stdin = io::stdin();
    let mut out = BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bad input line: {e}");
                continue;
            }
        };
        let id = v["id"].clone();
        let tool = v["tool"].as_str().unwrap_or("");
        let output = v["output"].as_str().unwrap_or("");
        let command = v["input"]
            .get("command")
            .or_else(|| v["input"].get("input"))
            .and_then(|c| c.as_str());
        if features {
            let mut o = serde_json::json!({"id": id, "f": estimate::features(output).to_vec()});
            if let Some(alt) = v.get("alt").and_then(|a| a.as_str()) {
                o["f_alt"] = estimate::features(alt).to_vec().into();
            }
            let _ = writeln!(out, "{o}");
            continue;
        }
        if let Some(st) = style {
            let g = crush_core::transforms::search::group_with(output, st);
            let o = serde_json::json!({"id": id, "output": g});
            let _ = writeln!(out, "{o}");
            continue;
        }
        let input = Input { tool, command, output, raw_ref: Some(RAW_REF) };
        let mut best = u128::MAX;
        let mut res = None;
        let mut panicked = false;
        for _ in 0..reps {
            let t = Instant::now();
            let r = std::panic::catch_unwind(|| compress(&input, &cfg));
            best = best.min(t.elapsed().as_nanos());
            match r {
                Ok(o) => res = Some(o),
                Err(_) => {
                    panicked = true;
                    break;
                }
            }
        }
        let cls = classify(tool, command, output);
        let mut o = serde_json::json!({
            "id": id, "class": cls.class.as_str(), "reason": cls.reason, "panicked": panicked,
            "in_bytes": output.len(), "ns": best as u64,
            "est_v5_in": estimate::estimate_tokens(output, Tokenizer::V5),
            "est_v4_in": estimate::estimate_tokens(output, Tokenizer::V4),
        });
        if let Some(r) = res {
            let fired = r.changed();
            o["fired"] = fired.into();
            o["why"] = r.why.as_str().into();
            o["transforms"] = r.transforms.clone().into();
            o["lossless"] = r.lossless.into();
            o["out_bytes"] = r.text.len().into();
            o["est_v5_out"] = estimate::estimate_tokens(&r.text, Tokenizer::V5).into();
            o["est_v4_out"] = estimate::estimate_tokens(&r.text, Tokenizer::V4).into();
            if fired {
                // idempotence, checked from the outside too
                let again = compress(&Input { output: &r.text, ..input }, &cfg);
                o["idempotent"] = (again.text == r.text).into();
                o["output"] = r.text.into_owned().into();
            }
        } else {
            o["fired"] = false.into();
            o["out_bytes"] = output.len().into();
        }
        let _ = writeln!(out, "{o}");
    }
}
