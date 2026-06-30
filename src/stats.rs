//! Session-cumulative compression stats — what `/crush` reports.
//!
//! Tracks per-bucket (transform / tool) input/output bytes and call counts
//! across the long-lived extension session. Only **wins** are recorded
//! (calls where compression actually replaced the output); passthroughs are
//! invisible to this surface by design — the `/crush` report answers
//! "how much context did crush save me?", not "how many calls did it see?".

use std::collections::HashMap;

/// One bucket's running totals.
#[derive(Debug, Default, Clone, Copy)]
struct Bucket {
    input_bytes: u64,
    output_bytes: u64,
    calls: u64,
}

impl Bucket {
    fn saved(&self) -> u64 {
        self.input_bytes.saturating_sub(self.output_bytes)
    }
    fn pct(&self) -> u64 {
        self.saved()
            .checked_mul(100)
            .and_then(|v| v.checked_div(self.input_bytes))
            .unwrap_or(0)
    }
}

/// Session-cumulative compression stats.
#[derive(Debug, Default)]
pub struct Stats {
    total_input_bytes: u64,
    total_output_bytes: u64,
    call_count: u64,
    by_bucket: HashMap<String, Bucket>,
}

impl Stats {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one compression win. `bucket` is the transform/tool label
    /// (e.g. `"ls"`, `"ps"`, `"git_log"`, `"tabular"`, `"text"`).
    pub fn record(&mut self, bucket: &str, input_bytes: usize, output_bytes: usize) {
        // Only count genuine wins — guards against future callers that
        // accidentally feed passthroughs into the breakdown.
        if output_bytes >= input_bytes {
            return;
        }
        let entry = self.by_bucket.entry(bucket.to_string()).or_default();
        entry.input_bytes += input_bytes as u64;
        entry.output_bytes += output_bytes as u64;
        entry.calls += 1;
        self.total_input_bytes += input_bytes as u64;
        self.total_output_bytes += output_bytes as u64;
        self.call_count += 1;
    }

    #[must_use]
    pub fn call_count(&self) -> u64 {
        self.call_count
    }

    #[must_use]
    pub fn total_saved_bytes(&self) -> u64 {
        self.total_input_bytes
            .saturating_sub(self.total_output_bytes)
    }

    /// Human-readable session report. Buckets sorted by bytes saved desc.
    #[must_use]
    pub fn render(&self) -> String {
        if self.call_count == 0 {
            return "🗜️  crush — no compressions yet this session.".to_string();
        }
        let saved = self.total_saved_bytes();
        let avg_pct = saved
            .checked_mul(100)
            .and_then(|v| v.checked_div(self.total_input_bytes))
            .unwrap_or(0);

        let mut lines = Vec::new();
        lines.push("🗜️  crush — context saved this session".to_string());
        lines.push(format!(
            "   {} saved across {} tool call{}  ({}% avg reduction)",
            human_bytes(saved),
            self.call_count,
            if self.call_count == 1 { "" } else { "s" },
            avg_pct,
        ));
        lines.push("   ───────────────────────────────".to_string());

        let mut rows: Vec<(&String, &Bucket)> = self.by_bucket.iter().collect();
        rows.sort_by(|a, b| {
            b.1.saved()
                .cmp(&a.1.saved())
                .then_with(|| a.0.cmp(b.0))
        });

        let label_w = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(4).max(4);
        for (name, b) in rows {
            lines.push(format!(
                "   {name:<label_w$}  -{pct:>2}%   ({calls} call{plural}, {saved})",
                pct = b.pct(),
                calls = b.calls,
                plural = if b.calls == 1 { "" } else { "s" },
                saved = human_bytes(b.saved()),
            ));
        }
        lines.join("\n")
    }
}

fn human_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let f = n as f64;
    if f >= MB {
        format!("{:.1} MB", f / MB)
    } else if f >= KB {
        format!("{:.1} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stats_render_says_no_compressions() {
        let s = Stats::new();
        let r = s.render();
        assert!(r.contains("no compressions yet"), "got: {r}");
        assert_eq!(s.call_count(), 0);
        assert_eq!(s.total_saved_bytes(), 0);
    }

    #[test]
    fn record_accumulates_across_calls_and_buckets() {
        let mut s = Stats::new();
        s.record("ls", 1000, 500); // saved 500
        s.record("ls", 2000, 1000); // saved 1000
        s.record("ps", 800, 600); // saved 200
        assert_eq!(s.call_count(), 3);
        assert_eq!(s.total_saved_bytes(), 1700);
    }

    #[test]
    fn record_ignores_non_wins() {
        let mut s = Stats::new();
        s.record("ls", 100, 100);
        s.record("ls", 100, 200);
        assert_eq!(s.call_count(), 0);
        assert_eq!(s.total_saved_bytes(), 0);
    }

    #[test]
    fn render_sorts_breakdown_by_bytes_saved_desc() {
        let mut s = Stats::new();
        s.record("ps", 800, 600); // 200 saved
        s.record("ls", 5000, 1000); // 4000 saved
        s.record("text", 2000, 1000); // 1000 saved
        let r = s.render();
        let ls_pos = r.find("\n   ls").expect("ls present");
        let text_pos = r.find("\n   text").expect("text present");
        let ps_pos = r.find("\n   ps").expect("ps present");
        assert!(ls_pos < text_pos, "ls should sort first:\n{r}");
        assert!(text_pos < ps_pos, "text before ps:\n{r}");
        assert!(r.contains("3 tool calls"), "got: {r}");
    }

    #[test]
    fn render_includes_per_bucket_percent_and_count() {
        let mut s = Stats::new();
        s.record("ls", 1000, 500); // -50%, 1 call
        let r = s.render();
        assert!(r.contains("ls"), "{r}");
        assert!(r.contains("-50%"), "{r}");
        assert!(r.contains("1 call"), "{r}");
    }
}
