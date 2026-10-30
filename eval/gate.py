#!/usr/bin/env python3
"""The free gate: read eval/out/*.json and fail on any broken invariant.

Checked (each only if its input exists):
- run (fixtures, corpus): 0 panics, 0 results that compress again
  (idempotence, checked from outside), crush-ext answered every request and
  agreed with the library, no stderr panics/frame errors
- edit safety: 0 CONTENT byte changes (crush-core class), 0 SEARCH results
  that don't rebuild byte for byte
- tokens: 0 fired results with more real tokens than the raw (both families)
- latency: corpus in-process p99 < 1 ms; every 256 KiB worst case < 1 ms
  best-of-200 (p99 is reported, not gated: it measures machine load)
- estimator: held-out median |error| < 10% (both families)
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from common import OUT  # noqa: E402

fails: list[str] = []
oks: list[str] = []


def load(name):
    p = OUT / name
    return json.loads(p.read_text()) if p.exists() else None


def check(cond: bool, what: str):
    (oks if cond else fails).append(what)


for s in ("fixtures", "corpus"):
    r = load(f"{s}.run.json")
    if not r:
        continue
    e = r["e2e"]
    check(r["panics"] == 0, f"{s}: panics {r['panics']}")
    check(r["not_idempotent"] == 0, f"{s}: not idempotent {r['not_idempotent']}")
    check(e["ok"] == e["requests"] and e["initialize_ok"], f"{s}: crush-ext ok {e['ok']}/{e['requests']}")
    check(e["mismatch_vs_inprocess"] == 0, f"{s}: crush-ext vs library mismatches {e['mismatch_vs_inprocess']}")
    check(e["stderr_panics"] == 0 and e["stderr_frame_errors"] == 0, f"{s}: crush-ext stderr panics/frame errors")
    if s == "corpus":
        p99 = r["in_process_ns"]["p99_all"] / 1e6
        check(p99 < 1.0, f"corpus in-process p99 {p99:.3f} ms < 1 ms")
    t = load(f"tokens.{s}.json")
    if t:
        for m, a in t["fired"].items():
            check(a["grew_real_tokens"] == 0, f"{s}: fired results with more real tokens ({m}): {a['grew_real_tokens']}")

for tag in ("", ".search_on"):
    es = load(f"edit_safety{tag}.json")
    if es:
        for s, r in es.items():
            check(r["content_changed"] == 0, f"{s}{tag}: CONTENT byte changes {r['content_changed']}")
            check(r["search_rebuild_failures"] == 0, f"{s}{tag}: SEARCH rebuild failures {r['search_rebuild_failures']}")
    for s in ("fixtures", "corpus"):
        t = load(f"tokens.{s}{tag}.json") if tag else None
        if t:
            for m, a in t["fired"].items():
                check(a["grew_real_tokens"] == 0, f"{s}{tag}: fired results with more real tokens ({m}): {a['grew_real_tokens']}")

for tag in ("", ".search_on"):
    syn = load(f"synthetic{tag}.json")
    if syn:
        for x in syn:
            check(x["best_us"] < 1000, f"256 KiB {x['shape']}{tag}: best {x['best_us']:.0f} µs (p99 {x['p99_us']:.0f} µs) < 1 ms")

est = load("estimator.json")
if est:
    for k, v in est["fit"].items():
        e = v["test"]["median_abs_rel_err"]
        check(e < 0.10, f"estimator {k} held-out median |err| {e:.1%} < 10%")

for o in oks:
    print("  ok  ", o)
for f in fails:
    print("  FAIL", f)
print(f"gate: {len(oks)} ok, {len(fails)} failed")
sys.exit(1 if fails else 0)
