//! crush-ext: the Synaps process-extension shim (protocol v1, Content-Length
//! framed JSON-RPC over stdio). `after_tool_call` → crush-core → `replace` or
//! `continue`. `/crush` reports what this session saved, in estimated tokens.
//!
//! Lossy transforms need somewhere to keep the raw output. Pass
//! `--keep-raw-dir DIR` to enable them: the raw goes to `DIR/<hash>.txt` (0600)
//! before the lossy text is returned, and the markers point there. Without it
//! only lossless transforms run. Natively (Phase 3) Synaps does this itself
//! with its spill files.

mod protocol;

use crush_core::{compress, Config, Input};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};

const PROTOCOL_VERSION: u64 = 1;

fn log(msg: &str) {
    let _ = writeln!(io::stderr(), "[crush] {msg}");
}

#[derive(Default)]
struct Stats {
    /// class/transforms -> (calls, tokens before, tokens after)
    rows: BTreeMap<String, (u64, u64, u64)>,
}

impl Stats {
    fn record(&mut self, key: String, before: u32, after: u32) {
        let r = self.rows.entry(key).or_default();
        r.0 += 1;
        r.1 += before as u64;
        r.2 += after as u64;
    }

    fn render(&self) -> String {
        if self.rows.is_empty() {
            return "crush: no compressions yet this session.".into();
        }
        let (calls, b, a) = self.rows.values().fold((0, 0, 0), |t, r| (t.0 + r.0, t.1 + r.1, t.2 + r.2));
        let mut out = vec![format!(
            "crush: ~{} tokens saved across {calls} tool result{} ({:.0}% of those results; estimated)",
            b - a,
            if calls == 1 { "" } else { "s" },
            100.0 * (b - a) as f64 / b.max(1) as f64
        )];
        let mut rows: Vec<_> = self.rows.iter().collect();
        rows.sort_by(|x, y| (y.1 .1 - y.1 .2).cmp(&(x.1 .1 - x.1 .2)).then(x.0.cmp(y.0)));
        for (k, (n, b, a)) in rows {
            out.push(format!("  {k:<28} {n:>5} × ~{} tokens saved", b - a));
        }
        out.join("\n")
    }
}

struct Shim {
    cfg: Config,
    keep_raw_dir: Option<PathBuf>,
    stats: Stats,
}

/// FNV-1a 64: a stable file name for a raw output (not a security boundary).
fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in s.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn write_private(path: &Path, data: &str) -> io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(data.as_bytes())
}

impl Shim {
    fn hook(&mut self, params: Option<&Value>) -> Value {
        let cont = json!({ "action": "continue" });
        let Some(p) = params else { return cont };
        if p.get("kind").and_then(Value::as_str) != Some("after_tool_call") {
            return cont;
        }
        let Some(output) = p.get("tool_output").and_then(Value::as_str) else { return cont };
        let tool = p.get("tool_name").and_then(Value::as_str).unwrap_or("");
        let inp = p.get("tool_input");
        let command = inp.and_then(|i| i.get("command").or_else(|| i.get("input"))).and_then(Value::as_str);
        let raw_path = self.keep_raw_dir.as_ref().map(|d| d.join(format!("{:016x}.txt", fnv64(output))));
        let raw_ref = raw_path.as_ref().map(|p| p.to_string_lossy().into_owned());
        let input = Input { tool, command, output, raw_ref: raw_ref.as_deref() };
        let o = compress(&input, &self.cfg);
        if !o.changed() {
            return cont;
        }
        if !o.lossless {
            let ok = raw_path.as_ref().is_some_and(|path| {
                std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).is_ok()
                    && write_private(path, output).is_ok()
            });
            if !ok {
                log("could not keep the raw output; passing through");
                return cont;
            }
        }
        self.stats.record(format!("{}/{}", o.class, o.transforms.join("+")), o.tokens_before, o.tokens_after);
        json!({ "action": "replace", "output": o.text })
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut shim = Shim { cfg: Config::default(), keep_raw_dir: None, stats: Stats::default() };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--keep-raw-dir" => shim.keep_raw_dir = args.next().map(PathBuf::from),
            "--config" => {
                if let Some(p) = args.next() {
                    match std::fs::read_to_string(&p)
                        .map_err(|e| e.to_string())
                        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                    {
                        Ok(c) => shim.cfg = c,
                        Err(e) => log(&format!("--config {p}: {e}; using defaults")),
                    }
                }
            }
            _ => log(&format!("ignoring argument {a}")),
        }
    }
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut w = stdout.lock();
    loop {
        let msg = match protocol::read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                log(&format!("frame error: {e}; resyncing"));
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        match msg.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => respond(&mut w, &id, json!({ "protocol_version": PROTOCOL_VERSION, "capabilities": {} })),
            "hook.handle" => {
                // crush-core already catches panics; this is the second net.
                let params = msg.get("params");
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shim.hook(params)))
                    .unwrap_or_else(|_| json!({ "action": "continue" }));
                respond(&mut w, &id, r);
            }
            "command.invoke" => {
                let p = msg.get("params").cloned().unwrap_or(Value::Null);
                let rid = p.get("request_id").and_then(Value::as_str).unwrap_or("");
                let body = match p.get("command").and_then(Value::as_str).unwrap_or("") {
                    "crush" | "/crush" | "" => shim.stats.render(),
                    other => format!("crush: unknown command '{other}'"),
                };
                notify(&mut w, rid, json!({ "kind": "system", "content": body }));
                notify(&mut w, rid, json!({ "kind": "done" }));
                respond(&mut w, &id, json!({ "ok": true }));
            }
            "shutdown" => {
                respond(&mut w, &id, json!({}));
                break;
            }
            _ => respond(&mut w, &id, json!({})),
        }
    }
}

fn respond<W: Write>(w: &mut W, id: &Value, result: Value) {
    if let Err(e) = protocol::write_response(w, id, result) {
        log(&format!("write error: {e}"));
    }
}

fn notify<W: Write>(w: &mut W, request_id: &str, event: Value) {
    if let Err(e) =
        protocol::write_notification(w, "command.output", json!({ "request_id": request_id, "event": event }))
    {
        log(&format!("notification write error: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shim() -> Shim {
        Shim { cfg: Config::default(), keep_raw_dir: None, stats: Stats::default() }
    }

    #[test]
    fn passes_through_non_hooks_and_small_outputs() {
        let mut s = shim();
        assert_eq!(s.hook(Some(&json!({"kind": "before_message"})))["action"], "continue");
        assert_eq!(s.hook(Some(&json!({"kind": "after_tool_call"})))["action"], "continue");
        let p = json!({"kind": "after_tool_call", "tool_name": "bash", "tool_input": {"command": "echo hi"}, "tool_output": "hi\n"});
        assert_eq!(s.hook(Some(&p))["action"], "continue");
    }

    #[test]
    fn groups_grep_hits() {
        let mut s = shim();
        let out: String = (1..40)
            .map(|i| format!("crates/agent-engine/src/runtime/stream.rs:{i}:    let value_{i} = compute();\n"))
            .collect();
        let p = json!({"kind": "after_tool_call", "tool_name": "bash", "tool_input": {"command": "grep -rn value crates"}, "tool_output": out});
        let r = s.hook(Some(&p));
        assert_eq!(r["action"], "replace");
        assert!(r["output"]
            .as_str()
            .unwrap()
            .starts_with("crates/agent-engine/src/runtime/stream.rs\n1:    let value_1"));
        assert!(s.stats.render().contains("SEARCH/search_group"));
    }

    #[test]
    fn lossy_needs_a_raw_dir() {
        let blob: String = (0..2000).map(|i| b"QmFzZTY0IGlzIGZ1bjEyMw9xYZ+/"[i % 28] as char).collect();
        let p = json!({"kind": "after_tool_call", "tool_name": "bash", "tool_input": {"command": "curl -s x"}, "tool_output": format!("token: {blob}\n")});
        assert_eq!(shim().hook(Some(&p))["action"], "continue");
        let dir = std::env::temp_dir().join(format!("crush-ext-test-{}", std::process::id()));
        let mut s = Shim { keep_raw_dir: Some(dir.clone()), ..shim() };
        let r = s.hook(Some(&p));
        assert_eq!(r["action"], "replace");
        assert!(r["output"].as_str().unwrap().contains("base64 blob elided"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
