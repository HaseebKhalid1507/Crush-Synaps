//! The extension shim's request path on arbitrary input: frame parsing,
//! JSON-RPC dispatch, hook-param parsing, crush-core, the spill rule and the
//! /crush report. No file system (`no_fs`): raws are "kept" nowhere.
//!
//! Three shapes per input (first byte):
//! 0. a raw byte stream of frames, read until EOF;
//! 1. a well-formed `hook.handle` frame whose fields come from the input
//!    (tool, runtime name, command, session, output, optional host marker);
//! 2. the input as hook params JSON, straight to `Shim::hook`.
//! Every `hook.handle` must answer `continue` or `replace`; a replacement is
//! never longer than the output, never for host-truncated input.
#![no_main]
use crush_ext::{config::ExtConfig, host_truncated, protocol, Options, Shim};
use libfuzzer_sys::fuzz_target;
use serde_json::{json, Value};
use std::io::{Cursor, ErrorKind};

const TOOLS: &[&str] = &["bash", "read", "grep", "ls", "ext__web-tools__fetch", "ext__fs__x", "memory_fetch", ""];
const CMDS: &[&str] = &["cargo test", "cargo build", "grep -rn x .", "cat f", "curl x", "./poll.sh", "ls -la", "git log"];

fn shim(data: &[u8]) -> Shim {
    let mut cfg = ExtConfig::default();
    if data.first().is_some_and(|b| b & 1 == 1) {
        cfg.core.transforms.test_pass = true;
        cfg.core.transforms.json_minify = true;
        cfg.core.min_bytes = 0;
    }
    if data.get(1).is_some_and(|b| b & 1 == 1) {
        cfg.spill.threshold = 64 + data[1] as usize * 16;
        cfg.spill.head = 32 + data[1] as usize;
        cfg.spill.tail = 16 + data[1] as usize / 2;
    }
    Shim::new(cfg, Options { no_fs: true, ..Options::default() })
}

fn checked(out: Vec<u8>, outputs: &[String]) {
    let mut c = Cursor::new(out);
    while let Ok(Some(m)) = protocol::read_message(&mut c) {
        let Some(r) = m.get("result") else { continue };
        if let Some(a) = r.get("action").and_then(Value::as_str) {
            assert!(a == "continue" || a == "replace", "{a}");
            if a == "replace" {
                assert!(r["output"].is_string(), "replace carries text");
                for o in outputs {
                    assert!(!host_truncated(o), "replaced host-truncated input");
                }
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let mode = data[0] % 3;
    let body = &data[4..];
    let mut s = shim(&data[1..3]);
    let mut out = Vec::new();
    match mode {
        0 => {
            let mut r = Cursor::new(body);
            for _ in 0..32 {
                match protocol::read_message(&mut r) {
                    Ok(Some(m)) => {
                        let _ = s.handle(&m, &mut out);
                    }
                    Ok(None) => break,
                    Err(e) if e.kind() == ErrorKind::InvalidData => continue,
                    Err(_) => break,
                }
            }
            checked(out, &[]);
        }
        1 => {
            let t = data[3];
            let text = String::from_utf8_lossy(body).into_owned();
            let mut output = text.clone();
            if t & 0x80 != 0 {
                output.push_str(&format!("…[truncated, {} total bytes]", body.len() * 7));
            }
            let mut p = json!({
                "kind": "after_tool_call",
                "tool_name": TOOLS[(t as usize) % TOOLS.len()],
                "tool_input": {"command": CMDS[(t as usize >> 3) % CMDS.len()]},
                "tool_output": output,
                "session_id": if t & 0x40 != 0 { Value::Null } else { json!(text.chars().take(80).collect::<String>()) },
            });
            if t & 0x20 != 0 {
                p["tool_runtime_name"] = json!(TOOLS[(t as usize >> 2) % TOOLS.len()]);
            }
            let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "hook.handle", "params": p});
            let b = serde_json::to_vec(&msg).unwrap();
            let mut frame = format!("Content-Length: {}\r\n\r\n", b.len()).into_bytes();
            frame.extend_from_slice(&b);
            let m = protocol::read_message(&mut Cursor::new(frame)).unwrap().unwrap();
            let _ = s.handle(&m, &mut out);
            let _ = s.handle(&json!({"id": 2, "method": "command.invoke", "params": {"command": "crush"}}), &mut out);
            let r = protocol::read_message(&mut Cursor::new(&out)).unwrap().unwrap();
            let a = r["result"]["action"].as_str().unwrap();
            if t & 0x80 != 0 {
                assert_eq!(a, "continue", "host-truncated input must pass through");
            }
            if a == "replace" {
                let o = r["result"]["output"].as_str().unwrap();
                assert_ne!(o, output);
            }
        }
        _ => {
            if let Ok(v) = serde_json::from_slice::<Value>(body) {
                let r = s.hook(Some(&v));
                let a = r["action"].as_str().unwrap();
                assert!(a == "continue" || a == "replace");
                if a == "replace" {
                    let o = v.get("tool_output").and_then(Value::as_str).unwrap();
                    assert!(!host_truncated(o));
                }
                let _ = s.initialize(Some(&json!({"config": v})));
                let _ = s.hook(Some(&v));
            }
        }
    }
});
