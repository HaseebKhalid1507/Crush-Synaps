//! crush-ext: the Synaps process-extension binary (protocol v1, Content-Length
//! framed JSON-RPC over stdio). See the library docs (`lib.rs`) for what it
//! guarantees.
//!
//! ```text
//! crush-ext                     run as an extension (Synaps starts it)
//! crush-ext --report            print the /crush report from the stats file
//! crush-ext --sweep             run the raw-store retention sweep now
//!
//! --state-dir DIR               stats + raw store root (default ~/.cache/crush)
//! --keep-raw-dir DIR            keep raws directly in DIR (eval)
//! --config FILE                 crush-core Config JSON (eval)
//! --set KEY=VALUE               any manifest config key (repeatable)
//! --timing                      log each request's in-process time to stderr (eval)
//! ```

use crush_ext::{config::ExtConfig, log, protocol, Flow, Options, Shim};
use serde_json::Value;
use std::io::{self, BufReader};
use std::path::PathBuf;

fn main() {
    let mut cfg = ExtConfig::default();
    let mut opts = Options { state_dir: crush_ext::store::default_state_dir(), ..Options::default() };
    let mut mode = "serve";
    let mut timing = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--report" => mode = "report",
            "--sweep" => mode = "sweep",
            "--timing" => timing = true,
            "--state-dir" => opts.state_dir = args.next().map(PathBuf::from),
            "--keep-raw-dir" => opts.keep_raw_dir = args.next().map(PathBuf::from),
            "--config" => {
                if let Some(p) = args.next() {
                    match std::fs::read_to_string(&p)
                        .map_err(|e| e.to_string())
                        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                    {
                        Ok(c) => cfg.core = c,
                        Err(e) => log(&format!("--config {p}: {e}; using defaults")),
                    }
                }
            }
            "--set" => {
                if let Some((k, v)) = args.next().as_deref().and_then(|kv| kv.split_once('=')) {
                    if let Err(e) = cfg.set(k.trim(), &Value::String(v.trim().to_string())) {
                        log(&format!("--set: {e}"));
                    }
                }
            }
            _ => log(&format!("ignoring argument {a}")),
        }
    }
    let mut shim = Shim::new(cfg, opts);
    match mode {
        "report" => {
            println!("{}", shim.report());
            return;
        }
        "sweep" => {
            match shim.store() {
                Some(s) => {
                    let r = crush_ext::sweep_now(s.root(), shim.cfg());
                    println!(
                        "crush: swept {}: deleted {} files ({} bytes); {} files, {} bytes left",
                        s.root().display(),
                        r.deleted,
                        r.freed,
                        r.left.files,
                        r.left.bytes
                    );
                }
                None => println!("crush: no raw store configured"),
            }
            return;
        }
        _ => {}
    }
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut w = stdout.lock();
    loop {
        let msg = match protocol::read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                log(&format!("frame error: {e}; resyncing"));
                continue;
            }
            Err(e) => {
                log(&format!("read error: {e}; exiting"));
                break;
            }
        };
        let t = std::time::Instant::now();
        let flow = shim.handle(&msg, &mut w);
        if timing {
            let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
            log(&format!("timing {method} {}", t.elapsed().as_nanos()));
        }
        shim.maintain();
        if let Flow::Stop = flow {
            break;
        }
    }
    shim.flush();
}
