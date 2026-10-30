use super::*;
use std::io::Cursor;

fn tmp_shim(dir: &Path) -> Shim {
    Shim::new(ExtConfig::default(), Options { state_dir: Some(dir.to_path_buf()), ..Options::default() })
}

fn mem_shim() -> Shim {
    Shim::new(ExtConfig::default(), Options { no_fs: true, ..Options::default() })
}

fn ev(tool: &str, cmd: &str, out: &str) -> Value {
    json!({"kind": "after_tool_call", "tool_name": tool, "tool_input": {"command": cmd}, "tool_output": out,
           "session_id": "sess-1"})
}

/// A poll loop: identical lines (the lossless `repeats` transform).
fn poll_log() -> String {
    let mut s = String::from("waiting for the build to finish\n");
    for _ in 0..60 {
        s.push_str("pending (build 4711 queued behind 3 jobs)\n");
    }
    s.push_str("done\n");
    s
}

/// A base64 blob (the lossy `blobs` transform).
fn blob_out() -> String {
    let blob: String = (0..2000).map(|i| b"QmFzZTY0IGlzIGZ1bjEyMw9xYZ+/"[i % 28] as char).collect();
    format!("token: {blob}\n")
}

#[test]
fn passes_through_non_hooks_and_small_outputs() {
    let mut s = mem_shim();
    assert_eq!(s.hook(Some(&json!({"kind": "before_message"})))["action"], "continue");
    assert_eq!(s.hook(Some(&json!({"kind": "after_tool_call"})))["action"], "continue");
    assert_eq!(s.hook(None)["action"], "continue");
    assert_eq!(s.hook(Some(&json!("a string")))["action"], "continue");
    assert_eq!(s.hook(Some(&ev("bash", "echo hi", "hi\n")))["action"], "continue");
    let r = s.hook(Some(&ev("bash", "./poll.sh", &poll_log())));
    assert_eq!(r["action"], "replace");
    assert!(r["output"].as_str().unwrap().contains("[crush: previous line repeated"));
}

#[test]
fn host_truncated_output_passes_through_untouched() {
    // what Synaps sends for a result over 256 KiB: a prefix + its marker
    let mut out = poll_log();
    out.push_str("…[truncated, 812345 total bytes]");
    assert!(host_truncated(&out));
    let mut s = mem_shim();
    assert_eq!(s.hook(Some(&ev("bash", "./poll.sh", &out)))["action"], "continue");
    s.flush();
    assert_eq!(s.view.total.passed.get("host-truncated"), Some(&1));
    // the same text without the marker would have been crushed
    assert_eq!(s.hook(Some(&ev("bash", "./poll.sh", &poll_log())))["action"], "replace");
    for not in ["…[truncated, 12 total bytes] trailing", "…[truncated,  total bytes]", "[truncated, 12 total bytes]"] {
        assert!(!host_truncated(not), "{not}");
    }
    assert!(host_truncated("x…[truncated, 262145 total bytes]"));
}

#[test]
fn the_runtime_name_wins_over_the_api_safe_name() {
    // MCP-style: the API-safe wire name and the runtime name differ; the
    // runtime one says it's a web fetch (PROSE: left alone by default)
    let out = poll_log();
    let mut p = json!({"kind": "after_tool_call", "tool_name": "ext__fs__x", "tool_runtime_name": "ext__web-tools__fetch",
                       "tool_input": {"url": "https://example.com"}, "tool_output": out});
    assert_eq!(parse_hook(Some(&p)).unwrap().tool, "ext__web-tools__fetch");
    assert_eq!(mem_shim().hook(Some(&p))["action"], "continue");
    // no runtime name (or an empty one): fall back to tool_name
    for rt in [Value::Null, json!("")] {
        p["tool_runtime_name"] = rt;
        assert_eq!(parse_hook(Some(&p)).unwrap().tool, "ext__fs__x");
        assert_eq!(mem_shim().hook(Some(&p))["action"], "replace");
    }
    p.as_object_mut().unwrap().remove("tool_runtime_name");
    assert_eq!(parse_hook(Some(&p)).unwrap().tool, "ext__fs__x");
}

#[test]
fn lossy_results_keep_the_raw_first_and_point_at_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = tmp_shim(tmp.path());
    let out = blob_out();
    let r = s.hook(Some(&ev("bash", "curl -s x", &out)));
    assert_eq!(r["action"], "replace");
    let text = r["output"].as_str().unwrap();
    assert!(text.contains("base64 blob elided"), "{text}");
    let path = s.store().unwrap().path_for(Some("sess-1"), &out);
    assert!(path.is_absolute());
    assert!(path.starts_with(tmp.path().join("raw").join("sess-1")));
    assert!(text.contains(path.to_str().unwrap()), "the marker quotes the absolute path: {text}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), out);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }
    // deterministic: the same result again gives the same text, one file
    let r2 = s.hook(Some(&ev("bash", "curl -s x", &out)));
    assert_eq!(r2["output"], r["output"]);
    assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn no_session_id_goes_to_nosession() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = tmp_shim(tmp.path());
    let out = blob_out();
    let p = json!({"kind": "after_tool_call", "tool_name": "bash", "tool_input": {"command": "curl x"},
                   "tool_output": out, "session_id": null});
    assert_eq!(s.hook(Some(&p))["action"], "replace");
    assert!(tmp.path().join("raw").join("nosession").is_dir());
}

#[cfg(unix)]
#[test]
fn a_raw_that_cannot_be_kept_means_pass_through() {
    let tmp = tempfile::tempdir().unwrap();
    // a file where the session directory should be
    std::fs::create_dir_all(tmp.path().join("raw")).unwrap();
    std::fs::write(tmp.path().join("raw").join("sess-1"), "x").unwrap();
    let mut s = tmp_shim(tmp.path());
    assert_eq!(s.hook(Some(&ev("bash", "curl -s x", &blob_out())))["action"], "continue");
    // lossless results don't need the store
    assert_eq!(s.hook(Some(&ev("bash", "./poll.sh", &poll_log())))["action"], "replace");
    s.flush();
    assert_eq!(s.view.total.passed.get("raw-store-failed"), Some(&1));
}

#[test]
fn without_a_store_lossy_transforms_are_off() {
    let mut s = Shim::new(ExtConfig::default(), Options::default());
    assert!(s.store().is_none());
    assert_eq!(s.hook(Some(&ev("bash", "curl -s x", &blob_out())))["action"], "continue");
}

/// Unique lines (no transform) around a run of identical lines.
fn just_over(before: usize, run: usize, after: usize) -> String {
    let mut s = String::new();
    for i in 0..before {
        s.push_str(&format!("{i:04} item-{i} state=ready owner=svc-{}\n", i % 7));
    }
    for _ in 0..run {
        s.push_str("retrying connection to db-primary:5432 (connection refused)\n");
    }
    for i in 0..after {
        s.push_str(&format!("{i:04} tail-{i} state=done owner=svc-{}\n", i % 5));
    }
    s
}

#[test]
fn spill_aware_crushing_under_the_threshold_must_not_cost_more() {
    // ~13.5 KB raw: Synaps would keep a ~7 KB preview. Crushed it is ~10.6 KB:
    // under the threshold, so all of it would land in history. Costlier.
    let raw = just_over(150, 50, 135);
    assert!(raw.len() > 12_288, "{}", raw.len());
    let mut s = mem_shim();
    let r = s.hook(Some(&ev("bash", "./poll.sh", &raw)));
    assert_eq!(r["action"], "continue");
    s.flush();
    assert_eq!(s.view.total.passed.get("spill-costlier"), Some(&1));
    // the same output with spill off (or a tool Synaps never spills): crushed
    let mut s = mem_shim();
    s.live.current.spill.threshold = 0;
    assert_eq!(s.hook(Some(&ev("bash", "./poll.sh", &raw)))["action"], "replace");
    // and a crush that lands far under the preview: replaced
    let raw = just_over(40, 400, 40);
    assert!(raw.len() > 12_288);
    let r = mem_shim().hook(Some(&ev("bash", "./poll.sh", &raw)));
    assert_eq!(r["action"], "replace");
}

#[test]
fn spill_aware_both_over_the_threshold_spills_the_crushed_text() {
    // both raw and crushed spill: same preview budget, the crushed one wins
    let raw = just_over(250, 300, 150);
    let mut s = mem_shim();
    let r = s.hook(Some(&ev("bash", "./poll.sh", &raw)));
    assert_eq!(r["action"], "replace");
    assert!(r["output"].as_str().unwrap().len() > 12_288);
}

#[test]
fn initialize_config_and_the_report() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin = tmp.path().join("base").join("plugins").join("crush");
    std::fs::create_dir_all(&plugin).unwrap();
    let mut s = tmp_shim(&tmp.path().join("state"));
    let mut out = Vec::new();
    let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                      "params": {"plugin_id": "crush", "plugin_root": plugin, "config": {"class_unknown": false}}});
    s.handle(&init, &mut out);
    let resp = protocol::read_message(&mut Cursor::new(out)).unwrap().unwrap();
    assert_eq!(resp["result"]["protocol_version"], 1);
    assert_eq!(s.synaps_base.as_deref(), Some(tmp.path().join("base").as_path()));
    assert!(!s.cfg().core.classes.unknown);
    assert_eq!(s.hook(Some(&ev("bash", "./poll.sh", &poll_log())))["action"], "continue");
    // /settings writes the plugin config file; the shim picks it up
    std::fs::write(plugin.join("config"), "class_unknown = true\n").unwrap();
    assert!(s.live.reload());
    assert_eq!(s.hook(Some(&ev("bash", "./poll.sh", &poll_log())))["action"], "replace");
    std::fs::write(plugin.join("config"), "class_unknown = true\nenabled = false\n").unwrap();
    assert!(s.live.reload());
    assert_eq!(s.hook(Some(&ev("bash", "./poll.sh", &poll_log())))["action"], "continue");
    // /crush
    let mut out = Vec::new();
    s.handle(&json!({"id": 5, "method": "command.invoke", "params": {"command": "crush", "request_id": "r1"}}), &mut out);
    let mut c = Cursor::new(out);
    let first = protocol::read_message(&mut c).unwrap().unwrap();
    assert_eq!(first["method"], "command.output");
    let text = first["params"]["event"]["content"].as_str().unwrap().to_string();
    assert!(text.contains("latest session (sess-1): 1 of 3 results crushed"), "{text}");
    assert!(text.contains("UNKNOWN repeats"), "{text}");
    assert!(text.contains("class-disabled 1") && text.contains("disabled 1"), "{text}");
    assert!(text.contains("raw store: 0 files"), "{text}");
    assert_eq!(protocol::read_message(&mut c).unwrap().unwrap()["params"]["event"]["kind"], "done");
    assert_eq!(protocol::read_message(&mut c).unwrap().unwrap()["result"]["ok"], true);
    // persisted
    let saved = stats::load(&tmp.path().join("state").join("stats.json"));
    assert_eq!(saved.total.results, 3);
}

#[test]
fn shim_equals_the_library_when_spill_is_off() {
    // the eval's crush-ext == library check runs with spill_threshold_bytes=0
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Shim::new(
        ExtConfig::default(),
        Options { keep_raw_dir: Some(tmp.path().to_path_buf()), no_fs: false, state_dir: None },
    );
    s.live.current.spill.threshold = 0;
    for (cmd, out) in [("./poll.sh", poll_log()), ("curl -s x", blob_out()), ("./x", just_over(150, 50, 135))] {
        let path = tmp.path().join(format!("{:016x}.txt", store::hash64(out.as_bytes())));
        let raw_ref = path.to_string_lossy().into_owned();
        let lib = crush_core::compress(
            &Input { tool: "bash", command: Some(cmd), output: &out, raw_ref: Some(&raw_ref) },
            &crush_core::Config::default(),
        );
        let r = s.hook(Some(&ev("bash", cmd, &out)));
        assert_eq!(r["action"] == "replace", lib.changed(), "{cmd}");
        if lib.changed() {
            assert_eq!(r["output"].as_str().unwrap(), lib.text);
        }
    }
}

#[test]
fn messages_without_id_get_no_answer_and_shutdown_stops() {
    let mut s = mem_shim();
    let mut out = Vec::new();
    assert!(matches!(s.handle(&json!({"method": "something.notify"}), &mut out), Flow::Continue));
    assert!(out.is_empty());
    assert!(matches!(s.handle(&json!({"id": 9, "method": "shutdown"}), &mut out), Flow::Stop));
    let r = protocol::read_message(&mut Cursor::new(out)).unwrap().unwrap();
    assert_eq!(r["id"], 9);
}
