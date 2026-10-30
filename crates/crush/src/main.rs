//! crush CLI.
//!
//! ```text
//! cmd | crush [--tool bash] [--command 'cmd'] [--keep-raw PATH] [--config FILE]
//! crush --explain [same flags]          what would happen, and why (stderr report + stdout text)
//! crush classify [--tool T] [--command C] < output
//! crush measure [--tool T] [--command C] FILE...
//! ```
//!
//! Lossy transforms only run with `--keep-raw PATH`: crush then writes the raw
//! input to PATH (only when the result is lossy) and the markers point at it.

use crush_core::{classify, compress, estimate_tokens, Config, Input, Tokenizer};
use std::io::{Read, Write};

const USAGE: &str = "usage:
  crush [--tool T] [--command CMD] [--keep-raw PATH] [--config FILE] [--explain]   (stdin -> stdout)
  crush classify [--tool T] [--command CMD]                                       (stdin)
  crush measure [--tool T] [--command CMD] [--config FILE] FILE...
options:
  --tool T          tool name the output came from (default: bash)
  --command CMD     the shell command that produced it
  --keep-raw PATH   allow lossy transforms; the raw input is written to PATH if used
  --config FILE     JSON crush-core config (see Config)
  --explain         print class, transforms, token estimates and the reason to stderr";

struct Args {
    sub: Option<String>,
    tool: String,
    command: Option<String>,
    keep_raw: Option<String>,
    config: Config,
    explain: bool,
    files: Vec<String>,
}

fn die(msg: &str) -> ! {
    let _ = writeln!(std::io::stderr(), "crush: {msg}");
    std::process::exit(2);
}

fn parse_args() -> Args {
    let mut a = Args {
        sub: None,
        tool: "bash".into(),
        command: None,
        keep_raw: None,
        config: Config::default(),
        explain: false,
        files: vec![],
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut val = |name: &str| it.next().unwrap_or_else(|| die(&format!("{name} needs a value")));
        match x.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("crush {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--tool" => a.tool = val("--tool"),
            "--command" => a.command = Some(val("--command")),
            "--keep-raw" => a.keep_raw = Some(val("--keep-raw")),
            "--config" => {
                let p = val("--config");
                let s = std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("{p}: {e}")));
                a.config = serde_json::from_str(&s).unwrap_or_else(|e| die(&format!("{p}: {e}")));
            }
            "--explain" => a.explain = true,
            "classify" | "measure" if a.sub.is_none() && a.files.is_empty() => a.sub = Some(x),
            _ if x.starts_with('-') => die(&format!("unknown option {x}\n{USAGE}")),
            _ => a.files.push(x),
        }
    }
    a
}

fn read_stdin() -> String {
    let mut buf = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
        die(&format!("stdin: {e}"));
    }
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn main() {
    let a = parse_args();
    match a.sub.as_deref() {
        Some("classify") => {
            let out = read_stdin();
            let c = classify(&a.tool, a.command.as_deref(), &out);
            println!("{}\t{}", c.class, c.reason);
        }
        Some("measure") => measure(&a),
        _ => run(&a),
    }
}

fn run(a: &Args) {
    if !a.files.is_empty() {
        die(&format!("unexpected argument {}\n{USAGE}", a.files[0]));
    }
    let raw = read_stdin();
    let input = Input { tool: &a.tool, command: a.command.as_deref(), output: &raw, raw_ref: a.keep_raw.as_deref() };
    let c = classify(&a.tool, a.command.as_deref(), &raw);
    let o = compress(&input, &a.config);
    if !o.lossless {
        if let Some(p) = &a.keep_raw {
            if let Err(e) = std::fs::write(p, raw.as_bytes()) {
                // can't keep the raw: hand back the raw instead of a lossy result
                let _ = writeln!(std::io::stderr(), "crush: --keep-raw {p}: {e}; passing through");
                let _ = std::io::stdout().write_all(raw.as_bytes());
                return;
            }
        }
    }
    if a.explain {
        let mut e = std::io::stderr();
        let _ = writeln!(e, "class:      {} ({})", c.class, c.reason);
        let _ = writeln!(e, "result:     {}", o.why.as_str());
        let _ =
            writeln!(e, "transforms: {}", if o.transforms.is_empty() { "-".into() } else { o.transforms.join(", ") });
        let _ = writeln!(e, "lossless:   {}", o.lossless);
        let _ = writeln!(e, "bytes:      {} -> {}", raw.len(), o.text.len());
        for tk in [Tokenizer::V5, Tokenizer::V4] {
            let (b, f) = (estimate_tokens(&raw, tk), estimate_tokens(&o.text, tk));
            let _ =
                writeln!(e, "tokens {}: {} -> {} (estimated)", if tk == Tokenizer::V5 { "5.x" } else { "4.x" }, b, f);
        }
    }
    let _ = std::io::stdout().write_all(o.text.as_bytes());
}

fn measure(a: &Args) {
    if a.files.is_empty() {
        die("measure needs at least one FILE");
    }
    println!("file\tclass\tresult\ttransforms\tbytes_in\tbytes_out\ttokens_in\ttokens_out\tsaved");
    let (mut ti, mut to) = (0u64, 0u64);
    for f in &a.files {
        let raw = match std::fs::read(f) {
            Ok(b) => String::from_utf8(b).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "crush: {f}: {e}");
                continue;
            }
        };
        let input = Input { tool: &a.tool, command: a.command.as_deref(), output: &raw, raw_ref: Some("<raw>") };
        let o = compress(&input, &a.config);
        ti += o.tokens_before as u64;
        to += o.tokens_after as u64;
        let saved =
            if o.tokens_before > 0 { 100.0 * (1.0 - o.tokens_after as f64 / o.tokens_before as f64) } else { 0.0 };
        println!(
            "{f}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{saved:.1}%",
            o.class,
            o.why.as_str(),
            if o.transforms.is_empty() { "-".into() } else { o.transforms.join(",") },
            raw.len(),
            o.text.len(),
            o.tokens_before,
            o.tokens_after
        );
    }
    if a.files.len() > 1 {
        let saved = if ti > 0 { 100.0 * (1.0 - to as f64 / ti as f64) } else { 0.0 };
        println!("total\t\t\t\t\t\t{ti}\t{to}\t{saved:.1}%");
    }
}
