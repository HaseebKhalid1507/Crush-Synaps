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
//! `--synthetic`: time compress() on 256 KiB worst-case shapes (no stdin).
use crush_core::{classify, compress, estimate, Config, Input, Tokenizer};
use std::io::{self, BufRead, BufWriter, Write};
use std::time::Instant;

pub const RAW_REF: &str =
    "/home/user/.synaps-cli/spill/session-1497513-1793340440887-49/toolu_018HPHz1D9QgMpJsQMfa9R3f.txt";

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
    if std::env::args().any(|a| a == "--synthetic") {
        synthetic(&cfg);
        return;
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
        let command = v["input"].get("command").or_else(|| v["input"].get("input")).and_then(|c| c.as_str());
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

/// 256 KiB worst cases: dense grep hits (fires), a source file (CONTENT),
/// an `ls -l` style listing, one minified JSON line, a `\r` progress stream,
/// a log full of repeats, and base64 blobs (all fire or bail).
fn synthetic(cfg: &Config) {
    const N: usize = 256 * 1024;
    fn fill(unit: impl Fn(usize) -> String) -> String {
        let mut s = String::with_capacity(N + 256);
        let mut i = 0;
        loop {
            let u = unit(i);
            if s.len() + u.len() > N {
                break;
            }
            s.push_str(&u);
            i += 1;
        }
        s
    }
    let b64 = "QmFzZTY0IGlzIGZ1bjEyMw9xYZ+/aGVsbG8gd29ybGQh";
    let shapes: Vec<(&str, &str, String)> = vec![
        (
            "grep_hits",
            "grep -rn value crates",
            fill(|i| {
                format!(
                    "crates/agent-engine/src/runtime/m{}.rs:{}:        let value = compute_something(&ctx, {i})?;\n",
                    i / 12,
                    i
                )
            }),
        ),
        (
            "source_file",
            "cat src/lib.rs",
            fill(|i| {
                format!("    pub fn handler_{i}(&self, req: &Request) -> Result<Response, Error> {{ self.inner.call(req, {i}) }}\n")
            }),
        ),
        (
            "ls_listing",
            "ls -la /usr/lib",
            fill(|i| format!("-rwxr-xr-x  1 root root  {:>9} Oct 12 09:41 libexample{i}.so.{}\n", 1000 + i * 7, i % 9)),
        ),
        (
            "minified_json",
            "curl -s https://api.example.test/items",
            format!("{{\"items\":[{}]}}", fill(|i| format!("{{\"id\":{i},\"v\":\"x{i}\"}},")).trim_end_matches(',')),
        ),
        (
            "progress_cr",
            "pip install big-package",
            fill(|i| {
                format!("Downloading part {} {:>3}%\r{}", i / 100, i % 100, if i % 100 == 99 { "\n" } else { "" })
            }),
        ),
        (
            "repeats_log",
            "make 2>&1",
            fill(|i| format!("waiting for the build lock held by another process (attempt group {})\n", i / 50)),
        ),
        ("base64_blobs", "kubectl get secret x -o yaml", fill(|i| format!("  key{i}: {}\n", b64.repeat(8)))),
        // Phase 2A: over-long hit lines (match mid-line, case-folded), and an
        // ls -la with the user's rows plus a root-owned `..`-style exception
        (
            "grep_long_lines",
            "grep -rni 'fetchuser' dist",
            fill(|i| {
                let f: String = "abcdefghij;var k=e.q[3]||7;".chars().cycle().take(1500 + (i % 7) * 400).collect();
                format!("dist/chunk{}.min.js:{}:{f}fetchUser(e,t){f}\n", i / 3, i + 1)
            }),
        ),
        (
            "ls_mixed_owner",
            "ls -la ~/data",
            fill(|i| {
                if i % 40 == 1 {
                    format!("drwxr-xr-x 1 root   root   {:>9} Oct 12 09:41 ..\n", 4096)
                } else {
                    format!("-rw-r--r-- 1 haseeb haseeb {:>9} Oct 12 09:41 data-{i}.parquet\n", 1000 + i * 7)
                }
            }),
        ),
    ];
    // ls_columns' input bound is 64 KiB: its worst case where it still fires
    let mut ls64 = String::new();
    let mut i = 0;
    while ls64.len() < 64 * 1024 - 200 {
        ls64.push_str(&format!("-rw-r--r-- 1 haseeb haseeb {:>9} Oct 12 09:41 data-{i}.parquet\n", 1000 + i * 7));
        i += 1;
    }
    let mut shapes = shapes;
    shapes.push(("ls_64k_fires", "ls -la ~/data", ls64));
    let mut rows = Vec::new();
    for (name, cmd, text) in &shapes {
        let input = Input { tool: "bash", command: Some(cmd), output: text, raw_ref: Some(RAW_REF) };
        let mut ns: Vec<u128> = Vec::new();
        let mut last = None;
        for _ in 0..200 {
            let t = Instant::now();
            let o = compress(&input, cfg);
            ns.push(t.elapsed().as_nanos());
            last = Some((o.why.as_str(), o.transforms.join("+"), o.text.len()));
        }
        ns.sort_unstable();
        let (why, tr, out_len) = last.unwrap();
        rows.push(serde_json::json!({"shape": name, "bytes": text.len(), "why": why, "transforms": tr, "out_bytes": out_len,
            "best_us": ns[0] as f64 / 1e3, "p50_us": ns[100] as f64 / 1e3, "p99_us": ns[197] as f64 / 1e3, "max_us": ns[199] as f64 / 1e3}));
    }
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}
