//! crush-eval-driver: read JSONL samples on stdin
//! (`{"id","tool","input","output"}`), run crush v0.1's real pipeline
//! (`compress::compress_labeled`, the function `handle_hook` calls) in-process,
//! and write one JSONL result per sample on stdout:
//! `{"id","fired","label","in_bytes","out_bytes","ns","panicked","output"?}`.
//! `output` is only present when crush replaced the text. `ns` is wall time of
//! the compress call alone (best of `--reps N`, default 1).
use std::io::{self, BufRead, BufWriter, Write};
use std::time::Instant;

fn main() {
    let reps: usize = std::env::args()
        .skip_while(|a| a != "--reps")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    // Silence the default panic hook: panics are counted, not printed.
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
        let tool = v["tool"].as_str().unwrap_or("").to_string();
        let input = v.get("input").cloned().unwrap_or(serde_json::Value::Null);
        let output = v["output"].as_str().unwrap_or("").to_string();
        let mut best = u128::MAX;
        let mut result = None;
        let mut panicked = false;
        for _ in 0..reps {
            let t = Instant::now();
            let r = std::panic::catch_unwind(|| {
                crush::compress::compress_labeled(&tool, &input, &output)
            });
            let ns = t.elapsed().as_nanos();
            best = best.min(ns);
            match r {
                Ok(x) => result = x,
                Err(_) => {
                    panicked = true;
                    result = None;
                    break;
                }
            }
        }
        let mut o = serde_json::json!({
            "id": id,
            "fired": result.is_some(),
            "label": result.as_ref().map(|(_, l)| *l),
            "in_bytes": output.len(),
            "out_bytes": result.as_ref().map_or(output.len(), |(s, _)| s.len()),
            "ns": best as u64,
            "panicked": panicked,
        });
        if let Some((s, _)) = result {
            o["output"] = serde_json::Value::String(s);
        }
        let _ = writeln!(out, "{o}");
    }
}
