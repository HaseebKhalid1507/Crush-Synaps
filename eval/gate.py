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
- Phase 2B (p2b_checks.json, spill.*.json): every non-collapsible LOG line
  kept verbatim and in order (failures, summaries, diagnostics and code
  frames called out), notes count exactly what they replaced, JSON layout
  keeps the token stream byte for byte; no fired result costs more after
  Synaps' spill step when the spill is decided on the raw's size (the
  crushed-size policy is reported); the 256 KiB LOG/JSON shapes < 1 ms p99
- Phase 3 (shim.json): every crush-ext shim check (host-truncated input
  passes through, runtime name preferred, raw kept before a lossy replace,
  spill rule, live config, /crush, retention sweep, malformed frames)
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

for tag in ("", ".search_on", ".p2a_on"):
    es = load(f"edit_safety{tag}.json")
    if es:
        for s, r in es.items():
            check(r["content_changed"] == 0, f"{s}{tag}: CONTENT byte changes {r['content_changed']}")
            check(r["search_rebuild_failures"] == 0, f"{s}{tag}: SEARCH rebuild failures {r['search_rebuild_failures']}")
            if "search_elide_failures" in r:
                check(r["search_elide_failures"] == 0, f"{s}{tag}: SEARCH cut lines not verbatim slices / wrong counts "
                      f"{r['search_elide_failures']} (of {r['search_elide_samples']})")
                check(r["ls_columns_restore_failures"] == 0, f"{s}{tag}: ls -l factoring not restorable "
                      f"{r['ls_columns_restore_failures']} (of {r['ls_columns_samples']})")
    for s in ("fixtures", "corpus"):
        t = load(f"tokens.{s}{tag}.json") if tag else None
        sp = load(f"after_spill.{s}{tag}.json") if tag else None
        if sp:
            for m, a in sp["by_model"].items():
                g = a.get("*|*", {})
                check(g.get("grew_raw", 0) == 0, f"{s}{tag}: fired results with more real tokens before spill ({m}): {g.get('grew_raw', 0)}")
        if t:
            for m, a in t["fired"].items():
                check(a["grew_real_tokens"] == 0, f"{s}{tag}: fired results with more real tokens ({m}): {a['grew_real_tokens']}")

for tag in ("", ".search_on", ".p2a_on"):
    syn = load(f"synthetic{tag}.json")
    if syn:
        for x in syn:
            check(x["best_us"] < 1000, f"256 KiB {x['shape']}{tag}: best {x['best_us']:.0f} µs (p99 {x['p99_us']:.0f} µs) < 1 ms")

est = load("estimator.json")
if est:
    for k, v in est["fit"].items():
        e = v["test"]["median_abs_rel_err"]
        check(e < 0.10, f"estimator {k} held-out median |err| {e:.1%} < 10%")

# Phase 2B: LOG / STRUCTURED invariants, re-checked independently of crush-core
for tag in ("", ".search_on"):
    p2b = load(f"p2b_checks{tag}.json")
    if p2b:
        for s, r in p2b.items():
            for k in ("log_lines_missing", "failure_lines_missing", "summary_lines_missing", "diagnostic_lines_missing",
                      "note_count_mismatches", "json_token_mismatches"):
                check(r.get(k, 0) == 0, f"{s}{tag}: p2b {k} {r.get(k, 0)}")
    for s in ("fixtures", "corpus"):
        sp = load(f"spill.{s}{tag}.json")
        if sp:
            for m, by in sp["fired_by"].items():
                # host policy B (spill decided on the raw's size, preview of the
                # crushed text): what Phase 3 should wire; policy A is reported.
                # Phase 2B's transforms, shipped and candidates (other phases'
                # after-spill effects are counted in their own sections, e.g.
                # search_elide's in Phase 2A's after_spill.py)
                rows = {k: v for k, v in by.items() if any(t in k for t in ("test_pass", "build_progress", "json_minify"))}
                for k, a in rows.items():
                    check(a.get("grew_after_spill_b", 0) == 0,
                          f"{s}{tag} {k}: fired results costlier after spill, spill-on-raw-size ({m}): {a.get('grew_after_spill_b', 0)} "
                          f"(spill-on-crushed-size: {a.get('grew_after_spill', 0)}, reported)")
# Phase 3: the extension shim (eval/shim_checks.py, the real binary over JSON-RPC)
shim = load("shim.json")
if shim:
    for c in shim["checks"]:
        check(c["ok"], f"shim: {c['check']}" + ("" if c["ok"] else f" ({c.get('detail')})"))
# ... and after Synaps' spill step, in real tokens (eval/p3_spill.py): the only
# results the shim may hand over costlier are LOG results whose failures the
# raw's preview would have cut
for s in ("fixtures", "corpus"):
    p3 = load(f"p3_spill.{s}.json")
    if p3:
        bad = [r for r in p3["shim_costlier"]
               if not (r["class"] == "LOG" and (r.get("fail_visible_shim") or 0) > (r.get("fail_visible_raw") or 0))]
        check(not bad, f"{s}: shim results costlier after spill (real tokens, either family), other than kept failures: {len(bad)}")
        a = p3["by_label"].get("all", {})
        if a.get("fail_lines"):
            check(a["fail_visible_shim"] >= a["fail_visible_raw"],
                  f"{s}: failure lines visible after spill, shim {a['fail_visible_shim']} >= raw {a['fail_visible_raw']}")

syn = load("synthetic.json")
if syn:
    for x in syn:
        if x["shape"] in ("cargo_test_log", "cargo_build_log", "pretty_json", "pretty_json_128k"):
            check(x["p99_us"] < 1000, f"256 KiB {x['shape']}: p99 {x['p99_us']:.0f} µs < 1 ms")

for o in oks:
    print("  ok  ", o)
for f in fails:
    print("  FAIL", f)
print(f"gate: {len(oks)} ok, {len(fails)} failed")
sys.exit(1 if fails else 0)
