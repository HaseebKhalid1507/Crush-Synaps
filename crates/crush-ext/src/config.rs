//! The extension's settings: crush-core's [`Config`] plus what only the shim
//! owns (raw store retention, Synaps' spill policy).
//!
//! Every key is declared in the plugin manifest's `extension.config` (so
//! Synaps resolves it and sends it at `initialize`) and the main ones in its
//! `/settings` category (which writes `plugins/<id>/config`). The shim re-reads
//! that file when it changes, so a `/settings` edit applies without a restart.
//!
//! Defaults are the gated defaults (eval/RESULTS.md): exactly
//! `crush_core::Config::default()`; a test holds the manifest to them.

use crush_core::Config;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// Synaps' `tool_output_spill_*` defaults (SynapsCLI #334).
pub const SPILL_THRESHOLD: usize = 12 * 1024;
pub const SPILL_HEAD: usize = 4 * 1024;
pub const SPILL_TAIL: usize = 3 * 1024;

/// What Synaps does with a result over the threshold: head + tail preview in
/// history, the whole text in a spill file. `threshold == 0`: spill off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpillPolicy {
    pub threshold: usize,
    pub head: usize,
    pub tail: usize,
}

impl Default for SpillPolicy {
    fn default() -> Self {
        Self { threshold: SPILL_THRESHOLD, head: SPILL_HEAD, tail: SPILL_TAIL }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtConfig {
    pub core: Config,
    pub spill: SpillPolicy,
    /// Raw outputs older than this are deleted by the retention sweep.
    pub raw_retention_days: u64,
    /// The raw store is cut back (oldest first) to this size.
    pub raw_max_mb: u64,
    /// Raw store root; `None`: `$XDG_CACHE_HOME/crush/raw` or `~/.cache/crush/raw`.
    pub raw_dir: Option<PathBuf>,
}

impl Default for ExtConfig {
    fn default() -> Self {
        Self {
            core: Config::default(),
            spill: SpillPolicy::default(),
            raw_retention_days: 7,
            raw_max_mb: 256,
            raw_dir: None,
        }
    }
}

/// Every key the shim understands (the manifest declares exactly these).
pub const KEYS: &[&str] = &[
    "enabled",
    "class_search",
    "class_listing",
    "class_log",
    "class_structured",
    "class_vcs",
    "class_unknown",
    "class_prose",
    "transform_progress",
    "transform_repeats",
    "transform_blobs",
    "transform_search_elide",
    "transform_build_progress",
    "transform_search_group",
    "transform_ls_columns",
    "transform_test_pass",
    "transform_test_group",
    "transform_json_minify",
    "raw_retention_days",
    "raw_max_mb",
    "raw_dir",
    "spill_threshold_bytes",
    "spill_head_bytes",
    "spill_tail_bytes",
];

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_u64().filter(|n| *n <= 1).map(|n| n == 1),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Some(true),
            "false" | "off" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().replace('_', "").parse().ok(),
        _ => None,
    }
}

impl ExtConfig {
    /// Apply one key. `Err` names the problem; the value is then left as it was.
    pub fn set(&mut self, key: &str, v: &Value) -> Result<(), String> {
        let bad = || format!("{key}: invalid value {v}");
        let b = || as_bool(v).ok_or_else(bad);
        let n = || as_u64(v).ok_or_else(bad);
        let c = &mut self.core;
        match key {
            "enabled" => c.enabled = b()?,
            "class_search" => c.classes.search = b()?,
            "class_listing" => c.classes.listing = b()?,
            "class_log" => c.classes.log = b()?,
            "class_structured" => c.classes.structured = b()?,
            "class_vcs" => c.classes.vcs = b()?,
            "class_unknown" => c.classes.unknown = b()?,
            "class_prose" => c.classes.prose = b()?,
            "transform_progress" => c.transforms.progress = b()?,
            "transform_repeats" => c.transforms.repeats = b()?,
            "transform_blobs" => c.transforms.blobs = b()?,
            "transform_search_elide" => c.transforms.search_elide = b()?,
            "transform_build_progress" => c.transforms.build_progress = b()?,
            "transform_search_group" => c.transforms.search_group = b()?,
            "transform_ls_columns" => c.transforms.ls_columns = b()?,
            "transform_test_pass" => c.transforms.test_pass = b()?,
            "transform_test_group" => c.transforms.test_group = b()?,
            "transform_json_minify" => c.transforms.json_minify = b()?,
            "raw_retention_days" => self.raw_retention_days = n()?.clamp(1, 3650),
            "raw_max_mb" => self.raw_max_mb = n()?.clamp(1, 1 << 20),
            "raw_dir" => {
                let s = v.as_str().ok_or_else(bad)?.trim();
                self.raw_dir = if s.is_empty() {
                    None
                } else if Path::new(s).is_absolute() {
                    Some(PathBuf::from(s))
                } else {
                    return Err(format!("raw_dir must be an absolute path, got {s:?}"));
                };
            }
            "spill_threshold_bytes" => self.spill.threshold = n()? as usize,
            "spill_head_bytes" => self.spill.head = n()? as usize,
            "spill_tail_bytes" => self.spill.tail = n()? as usize,
            _ => return Err(format!("unknown key {key}")),
        }
        Ok(())
    }

    /// Apply a JSON object of keys (the `initialize` config). Returns the problems.
    pub fn apply(&mut self, map: &serde_json::Map<String, Value>) -> Vec<String> {
        map.iter().filter_map(|(k, v)| self.set(k, v).err()).collect()
    }
}

/// `key = value` lines, as Synaps' plugin config store writes them.
pub fn parse_plugin_config(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            // first occurrence wins, like Synaps' reader
            out.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    out
}

/// The live config: what `initialize` resolved, plus edits made to the plugin
/// config file afterwards (`/settings` writes there).
///
/// Synaps resolves env override > plugin config file > legacy key > manifest
/// default once, at load. The shim can't see which source won, so it only
/// overlays keys whose file value *changed* since startup: an untouched key
/// keeps its resolved value (env overrides included), a key edited in
/// `/settings` takes the new value at once.
#[derive(Debug)]
pub struct Live {
    base: ExtConfig,
    pub current: ExtConfig,
    file: Option<PathBuf>,
    at_start: BTreeMap<String, String>,
    stamp: Option<(SystemTime, u64)>,
    checked: Option<Instant>,
}

impl Live {
    pub fn new(base: ExtConfig) -> Self {
        Self { current: base.clone(), base, file: None, at_start: BTreeMap::new(), stamp: None, checked: None }
    }

    /// `initialize`: the resolved config, and the plugin dir whose `config`
    /// file `/settings` edits.
    pub fn initialize(&mut self, resolved: Option<&serde_json::Map<String, Value>>, plugin_root: Option<&Path>) -> Vec<String> {
        let mut problems = Vec::new();
        if let Some(m) = resolved {
            problems = self.base.apply(m);
        }
        self.file = plugin_root.map(|r| r.join("config"));
        self.at_start = self.read_file().unwrap_or_default();
        self.stamp = self.file.as_deref().and_then(stamp);
        self.current = self.base.clone();
        problems
    }

    fn read_file(&self) -> Option<BTreeMap<String, String>> {
        let f = self.file.as_deref()?;
        let meta = std::fs::metadata(f).ok()?;
        // a settings file is a few hundred bytes; refuse anything absurd
        if !meta.is_file() || meta.len() > 64 * 1024 {
            return None;
        }
        Some(parse_plugin_config(&std::fs::read_to_string(f).ok()?))
    }

    /// Re-read the plugin config file if it changed (checked at most once a
    /// second: one `stat`). Returns true when the effective config changed.
    pub fn refresh(&mut self) -> bool {
        if self.file.is_none() {
            return false;
        }
        let now = Instant::now();
        if self.checked.is_some_and(|t| now.duration_since(t).as_millis() < 1000) {
            return false;
        }
        self.checked = Some(now);
        self.reload()
    }

    /// [`Self::refresh`] without the rate limit.
    pub fn reload(&mut self) -> bool {
        let s = self.file.as_deref().and_then(stamp);
        if s == self.stamp {
            return false;
        }
        self.stamp = s;
        let now = self.read_file().unwrap_or_default();
        let mut next = self.base.clone();
        for (k, v) in &now {
            if self.at_start.get(k) != Some(v) && KEYS.contains(&k.as_str()) {
                if let Err(e) = next.set(k, &Value::String(v.clone())) {
                    crate::log(&format!("plugin config: {e}; keeping the previous value"));
                }
            }
        }
        let changed = next != self.current;
        self.current = next;
        changed
    }
}

fn stamp(p: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manifest_declares_every_key_with_the_gated_defaults() {
        let m: Value = serde_json::from_str(include_str!("../../../.synaps-plugin/plugin.json")).unwrap();
        let entries = m["extension"]["config"].as_array().expect("extension.config");
        let keys: Vec<&str> = entries.iter().map(|e| e["key"].as_str().unwrap()).collect();
        assert_eq!(keys, KEYS, "manifest config keys == config::KEYS, same order");
        let mut from_manifest = ExtConfig::default();
        for e in entries {
            if let Some(d) = e.get("default") {
                from_manifest.set(e["key"].as_str().unwrap(), d).unwrap();
            }
        }
        assert_eq!(from_manifest, ExtConfig::default());
        assert_eq!(from_manifest.core, Config::default(), "gated defaults (eval/RESULTS.md)");
        // every /settings field is a declared key, with the same default
        for cat in m["settings"]["categories"].as_array().unwrap() {
            for f in cat["fields"].as_array().unwrap() {
                let k = f["key"].as_str().unwrap();
                let e = entries.iter().find(|e| e["key"] == k).unwrap_or_else(|| panic!("{k} not declared"));
                assert_eq!(f["default"], e["default"], "{k}");
            }
        }
        let perms = m["extension"]["permissions"].as_array().unwrap();
        assert!(perms.contains(&json!("tools.transform_output")));
        assert_eq!(m["extension"]["hooks"][0]["hook"], "after_tool_call");
    }

    #[test]
    fn values_parse_from_json_and_from_the_plugin_file() {
        let mut c = ExtConfig::default();
        c.set("enabled", &json!("off")).unwrap();
        c.set("class_log", &json!(false)).unwrap();
        c.set("transform_test_pass", &json!("true")).unwrap();
        c.set("raw_retention_days", &json!("3")).unwrap();
        c.set("spill_threshold_bytes", &json!(0)).unwrap();
        assert!(!c.core.enabled && !c.core.classes.log && c.core.transforms.test_pass);
        assert_eq!((c.raw_retention_days, c.spill.threshold), (3, 0));
        assert!(c.set("enabled", &json!("maybe")).is_err());
        assert!(c.set("raw_dir", &json!("relative/dir")).is_err());
        assert!(c.set("nope", &json!(1)).is_err());
        assert!(!c.core.enabled, "a bad value leaves the old one");
    }

    #[test]
    fn live_overlays_only_keys_edited_after_start() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("config");
        std::fs::write(&f, "# crush\nclass_log = false\n").unwrap();
        let mut live = Live::new(ExtConfig::default());
        // resolved at load: an env override won for class_log, the file for nothing else
        let resolved = json!({"class_log": "true", "enabled": true});
        live.initialize(resolved.as_object(), Some(dir.path()));
        assert!(live.current.core.classes.log, "resolved value kept: the file didn't change");
        // /settings edits another key
        std::fs::write(&f, "# crush\nclass_log = false\ntransform_test_pass = true\n").unwrap();
        set_mtime_later(&f);
        assert!(live.reload());
        assert!(live.current.core.transforms.test_pass);
        assert!(live.current.core.classes.log);
        // and then class_log itself
        std::fs::write(&f, "# crush\nclass_log = off\ntransform_test_pass = true\n").unwrap();
        set_mtime_later(&f);
        assert!(live.reload());
        assert!(!live.current.core.classes.log);
        assert!(!live.reload(), "unchanged file: nothing to do");
    }

    fn set_mtime_later(f: &Path) {
        let t = std::fs::metadata(f).unwrap().modified().unwrap() + std::time::Duration::from_secs(2);
        std::fs::File::options().write(true).open(f).unwrap().set_modified(t).unwrap();
    }
}
