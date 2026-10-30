# crush — 6-agent review board findings (S225)

Board: shady (correctness), silverhand (adversarial), joestar (round-trip), yoru (optimization),
chrollo (architecture), gojo (Rust craft). Consolidated + deduplicated. Status updated as fixed.

## 🔴 CRITICAL — safety / corruption (fix before anything)

| ID | Finding | Source | Status |
|----|---------|--------|--------|
| C1 | **No panic firewall.** `panic=abort` + no `catch_unwind` at hook boundary → any transform panic kills the process instead of degrading to pass-through. Violates the core invariant. | chrollo H5 | ☐ |
| C2 | **Marker injection.** Input already containing `@crush.*` / `[crush:` / `(×N)` / `[crush.blob]` → folded as data, corrupts format + breaks idempotency + double-compression. | silverhand CRIT-3, chrollo M3 | ☐ |
| C3 | **Silent `?` on dict miss.** `unwrap_or(b'?')` in columnar emit → silent corruption if dict/rows ever diverge. Must bail `None`. | shady H2, silverhand CRIT-1 | ☐ |
| C4 | **Header not in size gate.** `with_header` adds ~35B AFTER the savings check → "never enlarge" holds only by `MIN_BYTES_SAVED=256` coincidence, not design. | shady H1 | ☐ |
| C5 | **Dict value with space/`=`.** A dict value containing whitespace breaks `@crush.dict` parsing → unfold returns None, data unrecoverable. Latent now (whitespace schemas), live once delimiter schemas (git_log) land. | silverhand CRIT-2 | ☐ |

## 🟠 HIGH — correctness / round-trip

| ID | Finding | Source | Status |
|----|---------|--------|--------|
| H1 | **`utf8_len` continuation bytes → 4.** `0x80..=0xbf` hits `_ => 4`. Latent (input is valid UTF-8) but defensive fix trivial. | silverhand HIGH-1 | ☐ |
| H2 | **`unfold` is `#[cfg(test)]`-only** → lossless guarantee unenforceable in prod; no decoder ships. Make it `pub`, add `crush --unfold`. | gojo #7, chrollo H3 | ☐ |
| H3 | **`unfold` `schema.len()-1` underflow panic** on empty `@crush.cols`. | joestar BUG-3, silverhand MED-3 | ☐ |
| H4 | **Empty-tail trailing-space** in unfold; **trailing-newline not preserved**; **empty mid-body lines dropped** → round-trip test falsely green (norm() masks). | joestar BUG-1/4/5 | ☐ |
| H5 | **`collapse_dup_runs` `(×N)` collision** — lines already ending `(×N)` double-annotate; breaks idempotency. | silverhand MED-4, chrollo M3 | ☐ |
| H6 | **OSC `ESC \\` ST handling leaves stray `\\`** in strip_ansi output. | shady H3 | ☐ |
| H7 | **No Content-Length cap** in protocol → OOM on huge/malicious frame. | shady M5 | ☐ |
| H8 | **`run()` doc says "smallest", code does "first-match".** Doc/behavior drift. | chrollo H1 | ☐ |
| H9 | **tabular "lossless" overclaim** — null vs "", bool vs string, type loss. It's meaning-preserving, not byte-lossless. | chrollo H4 | ☐ |

## 🟡 PERF (converged across gojo/joestar/shady/silverhand)

| ID | Finding | Status |
|----|---------|--------|
| P1 | O(n²) dedup: tabular column-union + columnar `choose_enc` distinct scan → HashSet | ☐ |
| P2 | CSV emission allocates 2-3×/row: unify `cell`+`escape_field`+`csv_row*` into write-into-buffer | ☐ |
| P3 | `split_preamble` uses `Vec::remove(0)` O(n) → slice | ☐ |
| P4 | `longest_common_prefix` returns String → `&str` | ☐ |
| P5 | `rstrip_lines` collects Vec to join → write loop | ☐ |
| P6 | main loop `break` on frame error → `continue` (resilience) | ☐ |
| P7 | dead `let _ = idx`, `#[must_use]`, infallible unwrap_or, missing comments | ☐ |

## 🟢 OPTIMIZATIONS — yoru roadmap (ranked G×F/E)

| ID | Idea | Expected gain | Status |
|----|------|--------------|--------|
| O1 | **git_log pipe-delim schema** (+ `Delim` in Schema) | 60-75% on git_log | ☐ |
| O2 | **cargo build/check/test schema** | 50-65% on build logs | ☐ |
| O3 | **truncated-JSON prefix recovery** (tabular, string-aware bracket scan) | ~45-60% on the 256KB cargo_metadata — biggest single move | ☐ |
| O4 | **nested-array tabular walk** (fold largest array-of-objects in tree) | 30-55% on kubectl/docker/gh json | ☐ |
| O5 | **tail-prefix factoring** (columnar tail LCP) | lifts ps 28→~40% | ☐ |
| O6 | **grep file-grouping schema** | 40-60% on multi-hit grep | ☐ |

Yoru's TIER-3 DON'Ts (respected): no delta-encoding PIDs, no 2-char dict codes (manufactured
complexity), no generic LZ (kills LLM readability), no read-gutter strip (line numbers are info).

## DEFERRED to go-live shakeout (not tonight)
- Manifest + `info.get` + protocol-version negotiation + capability declaration (chrollo M1)
- Verify real `tool_name` strings in a live session (chrollo M2)
- LLM-accuracy benchmark raw-vs-crushed (chrollo) — decides if dict-coding stays

---

## S225 FIX PASS — status

**FIXED (Batch 1 — safety/correctness/perf):**
- C1 panic firewall (`catch_unwind` at hook boundary; release now `panic=unwind`)
- C2 marker-injection / double-compress guard (`looks_crushed`, versioned `@crush/1` namespace)
- C3 dict-miss → bail `None` (no silent `?`)
- C4 header accounted in size gate (never-enlarge by construction)
- C5 token-safety in `choose_enc` (whitespace/`=` values stay Plain — safe for future delim schemas)
- H1 `utf8_len` continuation-byte fix · H2 `unfold` now `pub` + `crush --unfold` CLI · H3 empty-header underflow guard
- H4 empty-tail / empty-mid-body-line / field-level round-trip test (no longer falsely green)
- H5 `(×N)` double-annotation guard · H6 OSC ST stray-backslash fix · H7 Content-Length 64 MiB cap
- H8 `run()` doc reconciled (priority-order first-match) · H9 tabular "meaning-preserving" not "lossless"
- P1 HashSet dedup (tabular + columnar) · P2 write-into-buffer CSV · P3 `split_at` not `remove(0)`
- P4 `longest_common_prefix → &str` · P5 `rstrip_lines` no Vec · P6 frame-error resync · P7 `#[must_use]`, dead `idx` removed

Verified: 47 tests pass, clippy clean, ratios held (28% blended), `--unfold` field-lossless on 3888 real rows, double-compress guard confirmed.

**NEXT (Batch 2 — optimizations, yoru roadmap):** O1 git_log pipe-delim, O2 cargo schema, O3 truncated-JSON recovery, O4 nested-array, O5 tail-prefix, O6 grep.
**DEFERRED (go-live):** manifest/info.get/protocol-version, live tool_name verify, LLM-accuracy benchmark.
