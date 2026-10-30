//! crush-ext: crush 2 as a Synaps process extension.
//!
//! Synaps calls the `after_tool_call` hook with every tool result; the shim
//! runs crush-core on it and answers `replace` (with the crushed text) or
//! `continue`. Synaps then applies its own spill step (#334) to whatever the
//! hook returned. Everything here exists so a `replace` is never worse than
//! `continue`:
//!
//! - **Host-truncated input passes through.** Synaps hands hooks at most
//!   256 KiB plus `…[truncated, N total bytes]`; replacing that would drop the
//!   tail Synaps still has.
//! - **Lossy results keep the raw first.** The raw goes to the shim's own
//!   store ([`store`]) before the `replace` leaves; if it can't be written,
//!   the result passes through. Markers quote the file's absolute path.
//! - **Spill-aware.** [`spill::decide`] keeps a replacement only when it
//!   doesn't make the post-spill history cost more.
//! - **Pass-through on doubt**: any parse problem, panic or I/O error → `continue`.
//!
//! The tool's runtime name (`tool_runtime_name`) is what crush classifies
//! on; the API-safe `tool_name` is the fallback.

pub mod config;
pub mod protocol;
pub mod spill;
pub mod stats;
pub mod store;

use config::{ExtConfig, Live};
use crush_core::{classify, compress_classified, Class, Input};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

pub const PROTOCOL_VERSION: u64 = 1;
/// The marker Synaps appends when it cut `tool_output` for the hook
/// (`HookEvent::after_tool_call`, `MAX_HOOK_OUTPUT` = 256 KiB).
const TRUNC_HEAD: &str = "…[truncated, ";
const TRUNC_TAIL: &str = " total bytes]";
/// Stats are written at most this often (and on `/crush`, shutdown, EOF).
const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// The raw store is swept at startup and at most this often.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);
/// A realistic Synaps spill path tail, for the preview note's token cost.
const SPILL_STAND_IN: &str = "spill/session-1497513-1793340440887-49/toolu_018HPHz1D9QgMpJsQMfa9R3f.txt";

pub fn log(msg: &str) {
    let _ = writeln!(std::io::stderr(), "[crush] {msg}");
}

/// Does `output` end with Synaps' hook truncation marker?
pub fn host_truncated(output: &str) -> bool {
    let Some(rest) = output.strip_suffix(TRUNC_TAIL) else { return false };
    let Some(at) = rest.rfind(TRUNC_HEAD) else { return false };
    let digits = &rest[at + TRUNC_HEAD.len()..];
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// One `after_tool_call`, as parsed from the hook params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookCall<'a> {
    /// `tool_runtime_name` when present and non-empty, else `tool_name`.
    pub tool: &'a str,
    pub command: Option<&'a str>,
    pub output: &'a str,
    pub session: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Not an `after_tool_call` (another hook kind, or no params): not counted.
    NotOurs,
    /// An `after_tool_call` we pass through before crush-core sees it.
    Pass(&'static str),
}

fn nonempty(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub fn parse_hook(params: Option<&Value>) -> Result<HookCall<'_>, (Skip, Option<&str>)> {
    let Some(p) = params.filter(|p| p.is_object()) else { return Err((Skip::NotOurs, None)) };
    if p.get("kind").and_then(Value::as_str) != Some("after_tool_call") {
        return Err((Skip::NotOurs, None));
    }
    let session = nonempty(p.get("session_id"));
    let Some(output) = p.get("tool_output").and_then(Value::as_str) else {
        return Err((Skip::Pass("no-text-output"), session));
    };
    if host_truncated(output) {
        return Err((Skip::Pass("host-truncated"), session));
    }
    let tool = nonempty(p.get("tool_runtime_name")).or_else(|| nonempty(p.get("tool_name"))).unwrap_or("");
    let inp = p.get("tool_input");
    let command = inp.and_then(|i| i.get("command").or_else(|| i.get("input"))).and_then(Value::as_str);
    Ok(HookCall { tool, command, output, session })
}

/// What to answer for one call.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Continue(&'static str),
    Replace { text: String, key: String, row: stats::Row, lossless: bool, spill: spill::Why },
}

/// crush-core + the spill rule + the raw-first rule, for one call.
///
/// `raw_path` is asked for only when a lossy transform could run; `keep`
/// must have written the raw to that path when it returns `true`.
pub fn process(
    call: &HookCall,
    cfg: &ExtConfig,
    raw_path: impl FnOnce() -> Option<String>,
    spill_path: &str,
    keep: impl FnOnce(&str) -> bool,
) -> Verdict {
    let c = &cfg.core;
    if !c.enabled {
        return Verdict::Continue("disabled");
    }
    let class = classify(call.tool, call.command, call.output).class;
    let out = call.output;
    // crush-core passes these through before any transform: no raw needed
    let early = class.byte_identical() || !c.classes.get(class) || out.len() < c.min_bytes || out.len() > c.max_bytes;
    let raw_ref = if early { None } else { raw_path() };
    let input = Input { tool: call.tool, command: call.command, output: out, raw_ref: raw_ref.as_deref() };
    let o = compress_classified(&input, c, class);
    if !o.changed() {
        return Verdict::Continue(o.why.as_str());
    }
    let d = spill::decide(call.tool, out, &o.text, &cfg.spill, spill_path, c.tokenizer, class == Class::Log);
    if !d.replace {
        return Verdict::Continue(d.why.as_str());
    }
    if !o.lossless && !raw_ref.as_deref().is_some_and(keep) {
        return Verdict::Continue("raw-store-failed");
    }
    let (hb, ha) = d.history.unwrap_or((o.tokens_before, o.tokens_after));
    Verdict::Replace {
        key: format!("{} {}", o.class, o.transforms.join("+")),
        row: stats::Row {
            fires: 1,
            before: o.tokens_before.into(),
            after: o.tokens_after.into(),
            hist_before: hb.into(),
            hist_after: ha.into(),
        },
        lossless: o.lossless,
        spill: d.why,
        text: o.text.into_owned(),
    }
}

/// Where things live, from the command line.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Stats file and default raw store root (`<dir>/stats.json`, `<dir>/raw`).
    pub state_dir: Option<PathBuf>,
    /// Legacy/eval: keep raws directly in this dir (no session dirs).
    pub keep_raw_dir: Option<PathBuf>,
    /// No file system at all (fuzzing): raws are "kept" nowhere, stats stay
    /// in memory, the plugin config file is never read.
    pub no_fs: bool,
}

pub struct Shim {
    pub live: Live,
    opts: Options,
    synaps_base: Option<PathBuf>,
    delta: stats::Stats,
    view: stats::Stats,
    last_flush: Option<Instant>,
    last_sweep: Option<Instant>,
    sweeping: Arc<AtomicBool>,
    warned_store: bool,
}

pub enum Flow {
    Continue,
    Stop,
}

impl Shim {
    pub fn new(base: ExtConfig, opts: Options) -> Self {
        let synaps_base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".synaps-cli"));
        Self {
            live: Live::new(base),
            opts,
            synaps_base,
            delta: stats::Stats::default(),
            view: stats::Stats::default(),
            last_flush: None,
            last_sweep: None,
            sweeping: Arc::new(AtomicBool::new(false)),
            warned_store: false,
        }
    }

    pub fn cfg(&self) -> &ExtConfig {
        &self.live.current
    }

    fn stats_path(&self) -> Option<PathBuf> {
        (!self.opts.no_fs).then(|| self.opts.state_dir.as_ref().map(|d| d.join("stats.json"))).flatten()
    }

    /// The raw store, or `None` when there is nowhere to keep raws.
    pub fn store(&self) -> Option<store::RawStore> {
        if let Some(d) = &self.opts.keep_raw_dir {
            return Some(store::RawStore::new(d.clone(), true));
        }
        if let Some(d) = &self.cfg().raw_dir {
            return Some(store::RawStore::new(d.clone(), false));
        }
        if self.opts.no_fs {
            return Some(store::RawStore::new(PathBuf::from("/nonexistent/crush/raw"), false));
        }
        self.opts.state_dir.as_ref().map(|d| store::RawStore::new(d.join("raw"), false))
    }

    fn spill_path(&self, session: Option<&str>) -> String {
        let base = self.synaps_base.as_deref().unwrap_or(Path::new("/home/user/.synaps-cli"));
        let _ = session; // Synaps' spill dir is its session key, not the hook's id; length is what matters
        format!("{}/{SPILL_STAND_IN}", base.display())
    }

    /// `initialize`: the config Synaps resolved, and the plugin dir.
    pub fn initialize(&mut self, params: Option<&Value>) -> Value {
        let cfg = params.and_then(|p| p.get("config")).and_then(Value::as_object);
        let root = if self.opts.no_fs {
            None
        } else {
            params.and_then(|p| p.get("plugin_root")).and_then(Value::as_str).map(PathBuf::from)
        };
        // the plugin lives in `<synaps base>/plugins/<id>`: that's where Synaps spills too
        if let Some(base) = root
            .as_deref()
            .and_then(|r| r.parent())
            .filter(|p| p.file_name().is_some_and(|n| n == "plugins"))
            .and_then(Path::parent)
        {
            self.synaps_base = Some(base.to_path_buf());
        }
        for p in self.live.initialize(cfg, root.as_deref()) {
            log(&format!("config: {p}; using the default"));
        }
        json!({ "protocol_version": PROTOCOL_VERSION, "capabilities": {} })
    }

    pub fn hook(&mut self, params: Option<&Value>) -> Value {
        let cont = json!({ "action": "continue" });
        if !self.opts.no_fs {
            self.live.refresh();
        }
        let call = match parse_hook(params) {
            Ok(c) => c,
            Err((Skip::NotOurs, _)) => return cont,
            Err((Skip::Pass(why), session)) => {
                self.delta.passed(&store::session_component(session), why);
                return cont;
            }
        };
        let session = store::session_component(call.session);
        let st = self.store();
        let no_fs = self.opts.no_fs;
        let path = st.as_ref().map(|s| s.path_for(call.session, call.output));
        let mut keep_err = None;
        let v = process(
            &call,
            self.cfg(),
            || path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            &self.spill_path(call.session),
            |_| {
                if no_fs {
                    return true;
                }
                match (st.as_ref(), path.as_deref()) {
                    (Some(s), Some(p)) => match s.keep(p, call.output) {
                        Ok(()) => true,
                        Err(e) => {
                            keep_err = Some(e);
                            false
                        }
                    },
                    _ => false,
                }
            },
        );
        if let Some(e) = keep_err {
            if !self.warned_store {
                self.warned_store = true;
                log(&format!("could not keep a raw output ({e}); lossy results pass through"));
            }
        }
        match v {
            Verdict::Continue(why) => {
                self.delta.passed(&session, why);
                cont
            }
            Verdict::Replace { text, key, row, .. } => {
                self.delta.fired(&session, key, row);
                json!({ "action": "replace", "output": text })
            }
        }
    }

    /// Fold pending stats into the file (or the in-memory view).
    pub fn flush(&mut self) {
        if self.delta.is_empty() {
            return;
        }
        self.last_flush = Some(Instant::now());
        let delta = std::mem::take(&mut self.delta);
        match self.stats_path() {
            Some(p) => match stats::flush(&p, &delta) {
                Ok(v) => self.view = v,
                Err(e) => {
                    log(&format!("stats: {e}"));
                    self.view.merge(&delta);
                }
            },
            None => self.view.merge(&delta),
        }
    }

    pub fn report(&mut self) -> String {
        self.flush();
        if let Some(p) = self.stats_path() {
            // other Synaps processes add to the same file
            self.view = stats::load(&p);
        }
        let cfg = self.cfg().clone();
        let st = if self.opts.no_fs { None } else { self.store() };
        let u = st.as_ref().map(|s| store::usage(s.root()));
        stats::render(
            &self.view,
            st.as_ref().zip(u).map(|(s, u)| (s.root(), u, cfg.raw_retention_days, cfg.raw_max_mb)),
        )
    }

    /// After each message: write stats now and then, sweep the raw store
    /// at startup and hourly (on a background thread).
    pub fn maintain(&mut self) {
        if self.last_flush.map_or(true, |t| t.elapsed() >= FLUSH_EVERY) {
            self.flush();
        }
        if self.opts.no_fs || self.last_sweep.is_some_and(|t| t.elapsed() < SWEEP_EVERY) {
            return;
        }
        self.last_sweep = Some(Instant::now());
        let Some(st) = self.store() else { return };
        if self.sweeping.swap(true, Ordering::AcqRel) {
            return;
        }
        let root = st.root().to_path_buf();
        let cfg = self.cfg().clone();
        let flag = Arc::clone(&self.sweeping);
        let spawned = std::thread::Builder::new().name("crush-sweep".into()).spawn(move || {
            sweep_now(&root, &cfg);
            flag.store(false, Ordering::Release);
        });
        if spawned.is_err() {
            self.sweeping.store(false, Ordering::Release);
        }
    }

    /// One JSON-RPC message in, its response (and notifications) out.
    pub fn handle<W: Write>(&mut self, msg: &Value, w: &mut W) -> Flow {
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let params = msg.get("params");
        match msg.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => respond(w, &id, self.initialize(params)),
            "hook.handle" => {
                // crush-core catches its own panics; this is the second net
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.hook(params)))
                    .unwrap_or_else(|_| json!({ "action": "continue" }));
                respond(w, &id, r);
            }
            "command.invoke" => {
                let p = params.cloned().unwrap_or(Value::Null);
                let rid = p.get("request_id").and_then(Value::as_str).unwrap_or("").to_string();
                let body = match p.get("command").and_then(Value::as_str).unwrap_or("") {
                    "crush" | "/crush" | "" => self.report(),
                    other => format!("crush: unknown command '{other}'"),
                };
                notify(w, &rid, json!({ "kind": "system", "content": body }));
                notify(w, &rid, json!({ "kind": "done" }));
                respond(w, &id, json!({ "ok": true }));
            }
            "shutdown" => {
                self.flush();
                respond(w, &id, json!({}));
                return Flow::Stop;
            }
            _ => {
                // a notification (no id) gets no answer
                if msg.get("id").is_some() {
                    respond(w, &id, json!({}));
                }
            }
        }
        Flow::Continue
    }
}

/// Run the retention sweep on `root` with `cfg`'s limits.
pub fn sweep_now(root: &Path, cfg: &ExtConfig) -> store::SweepReport {
    store::sweep(
        root,
        Duration::from_secs(cfg.raw_retention_days.saturating_mul(86_400)),
        cfg.raw_max_mb.saturating_mul(1024 * 1024),
        SystemTime::now(),
    )
}

pub fn respond<W: Write>(w: &mut W, id: &Value, result: Value) {
    if let Err(e) = protocol::write_response(w, id, result) {
        log(&format!("write error: {e}"));
    }
}

fn notify<W: Write>(w: &mut W, request_id: &str, event: Value) {
    if let Err(e) = protocol::write_notification(w, "command.output", json!({ "request_id": request_id, "event": event }))
    {
        log(&format!("notification write error: {e}"));
    }
}

#[cfg(test)]
mod tests;
