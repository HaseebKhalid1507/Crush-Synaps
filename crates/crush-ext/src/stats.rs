//! What crush did, per session and in total, in estimated tokens. Kept in
//! `<state dir>/stats.json` so `/crush` (and `crush-ext --report`) can show
//! totals across restarts and across Synaps processes: each process adds its
//! own delta to the file (read, add, atomic rename).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Sessions kept in the file (most recently seen).
const MAX_SESSIONS: usize = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub fires: u64,
    /// Estimated tokens of the raw and of what crush handed over.
    pub before: u64,
    pub after: u64,
    /// The same after Synaps' spill step: what history carries either way.
    pub hist_before: u64,
    pub hist_after: u64,
}

impl Row {
    fn add(&mut self, o: &Row) {
        self.fires += o.fires;
        self.before += o.before;
        self.after += o.after;
        self.hist_before += o.hist_before;
        self.hist_after += o.hist_after;
    }
    fn saved(&self) -> u64 {
        self.before.saturating_sub(self.after)
    }
    fn hist_saved(&self) -> i64 {
        self.hist_before as i64 - self.hist_after as i64
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub results: u64,
    /// `CLASS transform+transform` -> row
    pub fired: BTreeMap<String, Row>,
    /// pass-through reason -> count
    pub passed: BTreeMap<String, u64>,
    /// last update, seconds since the epoch (sessions only)
    #[serde(default)]
    pub seen: u64,
}

impl Tally {
    fn add(&mut self, o: &Tally) {
        self.results += o.results;
        for (k, r) in &o.fired {
            self.fired.entry(k.clone()).or_default().add(r);
        }
        for (k, n) in &o.passed {
            *self.passed.entry(k.clone()).or_default() += n;
        }
        self.seen = self.seen.max(o.seen);
    }
    fn sum(&self) -> Row {
        let mut t = Row::default();
        for r in self.fired.values() {
            t.add(r);
        }
        t
    }
    pub fn is_empty(&self) -> bool {
        self.results == 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub version: u32,
    pub total: Tally,
    pub sessions: BTreeMap<String, Tally>,
    /// The session the last result came from.
    #[serde(default)]
    pub last_session: Option<String>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Stats {
    pub fn fired(&mut self, session: &str, key: String, row: Row) {
        let t = now_secs();
        for tally in [&mut self.total, self.sessions.entry(session.to_string()).or_default()] {
            tally.results += 1;
            tally.fired.entry(key.clone()).or_default().add(&row);
            tally.seen = t;
        }
        self.last_session = Some(session.to_string());
    }

    pub fn passed(&mut self, session: &str, why: &str) {
        let t = now_secs();
        for tally in [&mut self.total, self.sessions.entry(session.to_string()).or_default()] {
            tally.results += 1;
            *tally.passed.entry(why.to_string()).or_default() += 1;
            tally.seen = t;
        }
        self.last_session = Some(session.to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.total.is_empty()
    }

    /// Add another process's (or another flush's) counts.
    pub fn merge(&mut self, d: &Stats) {
        self.version = 1;
        self.total.add(&d.total);
        for (k, t) in &d.sessions {
            self.sessions.entry(k.clone()).or_default().add(t);
        }
        if d.last_session.is_some() {
            self.last_session = d.last_session.clone();
        }
        if self.sessions.len() > MAX_SESSIONS {
            let mut by_age: Vec<(u64, String)> = self.sessions.iter().map(|(k, t)| (t.seen, k.clone())).collect();
            by_age.sort();
            for (_, k) in by_age.into_iter().take(self.sessions.len() - MAX_SESSIONS) {
                self.sessions.remove(&k);
            }
        }
    }
}

pub fn load(path: &Path) -> Stats {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Add `delta` to the file at `path` (atomic replace). Returns the merged view.
pub fn flush(path: &Path, delta: &Stats) -> std::io::Result<Stats> {
    let mut s = load(path);
    s.merge(delta);
    let dir = path.parent().unwrap_or(Path::new("."));
    crate::store::ensure_private_dir(dir)?;
    let tmp = dir.join(format!(".stats.{}.tmp", std::process::id()));
    let res = (|| {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp)?;
        f.write_all(&serde_json::to_vec(&s)?)?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res.map(|()| s)
}

fn n(x: u64) -> String {
    let s = x.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn signed(x: i64) -> String {
    if x < 0 {
        format!("-{}", n(x.unsigned_abs()))
    } else {
        n(x as u64)
    }
}

fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0))
}

fn block(out: &mut Vec<String>, title: &str, t: &Tally) {
    let s = t.sum();
    out.push(format!(
        "{title}: {} of {} results crushed, ~{} tokens saved ({:.0}% of those), ~{} in history after Synaps' spill",
        n(s.fires),
        n(t.results),
        n(s.saved()),
        100.0 * s.saved() as f64 / s.before.max(1) as f64,
        signed(s.hist_saved()),
    ));
    let mut rows: Vec<(&String, &Row)> = t.fired.iter().collect();
    rows.sort_by(|a, b| b.1.saved().cmp(&a.1.saved()).then(a.0.cmp(b.0)));
    for (k, r) in rows.iter().take(8) {
        out.push(format!("  {k:<34} {:>4} × ~{} saved (history ~{})", r.fires, n(r.saved()), signed(r.hist_saved())));
    }
    if rows.len() > 8 {
        out.push(format!("  … {} more", rows.len() - 8));
    }
    let mut passed: Vec<(&String, &u64)> = t.passed.iter().collect();
    passed.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    if !passed.is_empty() {
        let p: Vec<String> = passed.iter().map(|(k, v)| format!("{k} {}", n(**v))).collect();
        out.push(format!("  passed through: {}", p.join(", ")));
    }
}

/// The `/crush` text. `store`: (root, usage, retention days, cap MiB).
pub fn render(s: &Stats, store: Option<(&Path, crate::store::Usage, u64, u64)>) -> String {
    let mut out = vec!["crush (estimated tokens)".to_string()];
    if s.is_empty() {
        out.push("no tool results seen yet.".into());
    } else {
        let this = s.last_session.as_ref().and_then(|id| s.sessions.get(id).map(|t| (id, t)));
        if let Some((id, t)) = this {
            block(&mut out, &format!("latest session ({id})"), t);
        }
        if this.is_some_and(|(_, t)| t.results == s.total.results) {
            out.push("all sessions: the same (no other session yet)".into());
        } else {
            block(&mut out, "all sessions", &s.total);
        }
    }
    match store {
        Some((root, u, days, cap)) => out.push(format!(
            "raw store: {} file{}, {} in {} (kept {days} days, capped at {cap} MiB)",
            n(u.files),
            if u.files == 1 { "" } else { "s" },
            mib(u.bytes),
            root.display()
        )),
        None => out.push("raw store: none (lossy transforms off)".into()),
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(b: u64, a: u64) -> Row {
        Row { fires: 1, before: b, after: a, hist_before: b, hist_after: a }
    }

    #[test]
    fn flush_merges_deltas_from_two_processes() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("crush").join("stats.json");
        let mut a = Stats::default();
        a.fired("s1", "LOG build_progress".into(), row(1000, 400));
        a.passed("s1", "too-small");
        let mut b = Stats::default();
        b.fired("s2", "UNKNOWN repeats".into(), row(300, 100));
        flush(&p, &a).unwrap();
        let merged = flush(&p, &b).unwrap();
        assert_eq!(merged.total.results, 3);
        assert_eq!(merged.total.sum().saved(), 800);
        assert_eq!(merged.sessions["s1"].results, 2);
        assert_eq!(merged.last_session.as_deref(), Some("s2"));
        assert_eq!(load(&p), merged);
        let text = render(&merged, Some((tmp.path(), crate::store::Usage { files: 2, bytes: 3 << 20 }, 7, 256)));
        assert!(text.contains("latest session (s2): 1 of 1 results crushed, ~200 tokens saved"), "{text}");
        assert!(text.contains("all sessions: 2 of 3 results crushed, ~800 tokens saved"), "{text}");
        assert!(text.contains("LOG build_progress"));
        assert!(text.contains("passed through: too-small 1"));
        assert!(text.contains("raw store: 2 files, 3.0 MiB"));
        assert!(text.lines().count() <= 12, "short: {text}");
    }

    #[test]
    fn a_corrupt_file_starts_over() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("stats.json");
        std::fs::write(&p, "{not json").unwrap();
        assert!(load(&p).is_empty());
        assert_eq!(n(1234567), "1,234,567");
        assert_eq!(signed(-1234), "-1,234");
    }
}
