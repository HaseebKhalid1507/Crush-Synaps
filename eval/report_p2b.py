#!/usr/bin/env python3
"""Phase 2B sections of eval/RESULTS.md (LOG, VCS, STRUCTURED), rendered from
eval/out/*.json and the private accuracy records. Aggregates only for real
data. Called by report.py (`p2b_*` markers)."""
from __future__ import annotations

import json
from collections import Counter, defaultdict

from report import CORPUS_DIR, M4, M5, OUT, fmt, load, parity, pct, read_jsonl, table

P2B = ("test_pass", "build_progress", "json_minify")
SHIPS = {"build_progress": True, "test_pass": False, "json_minify": False}
SHAPES = ("cargo_test_log", "cargo_build_log", "pretty_json", "pretty_json_128k")


def _mine(label):
    return any(t in P2B for t in (label or "").split("+"))


def class_totals(tc):
    tot = defaultdict(Counter)
    for r in (tc or {}).get("composition", []):
        for m in (M5, M4):
            tot[r["class"]][m] += r[f"tok:{m}"]
            tot["*"][m] += r[f"tok:{m}"]
    return tot


def sec_tokens(tc, ts):
    """Fired results per (class, label) for the shipped default and the
    candidate variant (every transform on), real tokens."""
    if not tc or "composition" not in tc:
        return "(corpus token pass not run)"
    tot = class_totals(tc)
    rows = []
    for name, t in (("default (ships)", tc), ("candidates (all on)", ts)):
        if not t:
            continue
        b5, b4 = t["fired"][M5]["by"], t["fired"][M4]["by"]
        for k in sorted(b5):
            cls, lab = k.split("|")
            if lab == "*" or not _mine(lab):
                continue
            a5, a4 = b5[k], b4[k]
            s5, s4 = a5["raw_tok"] - a5["crushed_tok"], a4["raw_tok"] - a4["crushed_tok"]
            rows.append([name, cls, lab, a5["n"], fmt(a5["raw_tok"]), fmt(s5), pct(s5, a5["raw_tok"]), pct(s4, a4["raw_tok"]),
                         pct(s5, tot[cls][M5], 2), pct(s5, tot["*"][M5], 3), pct(s4, tot["*"][M4], 3)])
    grew = {n: {m: t["fired"][m]["grew_real_tokens"] for m in (M5, M4)} for n, t in (("default", tc), ("candidates", ts)) if t}
    return "\n".join([
        table(["config", "class", "transforms", "fired", "raw tokens 5.x", "saved 5.x", "saved on fired 5.x", "saved on fired 4.x",
               "% of class 5.x", "% of all tool-result tokens 5.x", "% of all 4.x"], rows),
        "",
        f"Fired results with more real tokens than the raw (every fired result, both families): {grew}. "
        f"Class totals 5.x: LOG {fmt(tot['LOG'][M5])}, STRUCTURED {fmt(tot['STRUCTURED'][M5])}, VCS {fmt(tot['VCS'][M5])}, "
        f"all tool results {fmt(tot['*'][M5])}."])


def _after_spill_den(tc, sp):
    """Class token totals after spill: big results cost their preview."""
    tot = class_totals(tc)
    den = defaultdict(Counter)
    for m in (M5, M4):
        big = (sp or {}).get("spilled_results", {}).get(m, {})
        for cls in list(tot):
            if cls == "*":
                continue
            b = big.get(cls, {"raw": 0, "preview": 0})
            den[cls][m] = tot[cls][m] - b["raw"] + b["preview"]
            den["*"][m] += den[cls][m]
    return tot, den


def sec_spill(tc, spc, spc_v, spf, spf_v):
    if not spc:
        return "(spill accounting not run)"
    tot, den = _after_spill_den(tc, spc)
    rows = []
    for name, sp in (("default (ships)", spc), ("candidates (all on)", spc_v)):
        if not sp:
            continue
        for k, a5 in sorted(sp["fired_by"][M5].items()):
            cls, lab = k.split("|")
            if lab == "*" or not _mine(lab):
                continue
            a4 = sp["fired_by"][M4][k]
            r5, r4 = a5["raw"] - a5["crushed"], a4["raw"] - a4["crushed"]
            s5, s4 = a5["raw_after_spill"] - a5["crushed_after_spill"], a4["raw_after_spill"] - a4["crushed_after_spill"]
            rows.append([name, cls, lab, a5["n"], f"{a5['raw_spilled']} → {a5['crushed_spilled']}", fmt(r5), pct(r5, tot['*'][M5], 3),
                         fmt(s5), pct(s5, a5["raw_after_spill"]), pct(s5, den['*'][M5], 3), pct(s4, den['*'][M4], 3),
                         a5["grew_after_spill"]])
    lines = [table(["config", "class", "transforms", "fired", "spilled raw → crushed", "saved raw 5.x", "% of all (raw) 5.x",
                    "saved after spill 5.x", "on fired, after spill", "% of all after spill 5.x", "% of all after spill 4.x",
                    "costlier after spill"], rows)]
    big = spc.get("spilled_results", {}).get(M5, {})
    if big:
        n = sum(v["n"] for v in big.values())
        lines += ["", f"Denominators after spill: every corpus result over {spc['threshold_bytes']:,} B ({n:,} results) is counted as "
                      f"its head + tail preview; all tool-result tokens 5.x: {fmt(tot['*'][M5])} raw → {fmt(den['*'][M5])} after spill "
                      f"(LOG {fmt(tot['LOG'][M5])} → {fmt(den['LOG'][M5])}, STRUCTURED {fmt(tot['STRUCTURED'][M5])} → "
                      f"{fmt(den['STRUCTURED'][M5])})."]
    srows = []
    for name, sp in (("fixtures, default", spf), ("fixtures, candidates", spf_v), ("corpus, default", spc), ("corpus, candidates", spc_v)):
        fs = (sp or {}).get("failure_survival") or {}
        for k in ("all", "spilled"):
            if fs.get(f"{k}:results"):
                srows.append([name, "every fired LOG result with a failure line" if k == "all" else "… of which the raw spills",
                              fs[f"{k}:results"], fs[f"{k}:failure_lines"], fs[f"{k}:visible_raw_after_spill"],
                              fs[f"{k}:visible_crushed_after_spill"], fs[f"{k}:results_losing_failures_raw"],
                              fs[f"{k}:results_losing_failures_crushed"], fs[f"{k}:crushed_no_longer_spills"]])
    if srows:
        lines += ["", "Failure lines the model can still see after spill (FAILED, panicked, ✕, --- FAIL, Traceback, `error:`, assertions):", "",
                  table(["set", "results", "n", "failure lines", "visible: raw after spill", "visible: crushed after spill",
                         "results losing a failure line: raw", "… crushed", "crushed no longer spills"], srows)]
    rows = [r for r in (spf_v or {}).get("failure_rows", [])]
    if rows:
        lines += ["", "Fixtures (candidates config):", "",
                  table(["fixture", "raw bytes", "crushed bytes", "failure lines", "visible raw after spill", "visible crushed after spill",
                         "tokens after spill raw → crushed 5.x"],
                        [[r["id"], fmt(r["raw_bytes"]), fmt(r["crushed_bytes"]), r["failure_lines"], r["visible_raw_after_spill"],
                          r["visible_crushed_after_spill"], f"{fmt(r[f'raw_after_spill_tok:{M5}'])} → {fmt(r[f'crushed_after_spill_tok:{M5}'])}"]
                         for r in rows])]
    return "\n".join(lines)


def p2b_records(acc):
    """The Phase 2B run keeps its outputs under accuracy_p2b.* (the general
    accuracy.* files belong to whichever full `make eval` ran last)."""
    recs = list(read_jsonl(OUT / "accuracy_p2b.fixtures.jsonl")) if (OUT / "accuracy_p2b.fixtures.jsonl").exists() else []
    for rn in acc.get("runs", [acc.get("run")]):
        f = CORPUS_DIR / "accuracy" / f"{rn}.jsonl"
        if f.exists():
            recs += list(read_jsonl(f))
    return recs


def sec_accuracy(acc):
    if not acc:
        return "(not run)"
    recs = [r for r in p2b_records(acc) if _mine(r.get("label"))]
    par = parity(recs)
    rows = []
    for (lab, m), v in sorted(par.items(), key=lambda kv: (str(kv[0][0]), kv[0][1] == "both models", kv[0][1])):
        better = v["p"] is not None and v["p"] < 0.05 and v["crushed_only"] > v["raw_only"]
        rule = "**parity**" if v["parity"] else ("crushed **better** (two-sided p < 0.05)" if better and v["drop"] <= 0 else "**FAIL**")
        rows.append([lab, m.replace("claude-", ""), v["samples"], v["pairs"], f"{v['raw']}/{v['pairs']} ({pct(v['raw'], v['pairs'])})",
                     f"{v['crushed']}/{v['pairs']} ({pct(v['crushed'], v['pairs'])})", f"{v['raw_only']} / {v['crushed_only']}",
                     "–" if v["p"] is None else f"{v['p']:.3f}", f"{v['drop']:+.1%}", f"{v['noise']:.1%} (n={v['noise_n']})",
                     rule if m != "both models" else ""])
    lines = [table(["transforms fired", "model", "samples", "questions", "raw correct", "crushed correct",
                    "discordant raw-only / crushed-only", "McNemar p", "raw-only excess", "raw-vs-raw disagreement", "rule"], rows)]
    kinds = defaultdict(Counter)
    for r in recs:
        for t in (r.get("label") or "").split("+"):
            if t in P2B:
                kinds[(t, r["kind"])][r["variant"]] += r["correct"]
                kinds[(t, r["kind"])][r["variant"] + "_n"] += 1
    krows = [[t, k, f"{c['raw']}/{c['raw_n']}", f"{c['crushed']}/{c['crushed_n']}"] for (t, k), c in sorted(kinds.items())]
    lines += ["", "By question kind (both models pooled; a sample counts for every transform that fired on it):", "",
              table(["transform", "kind", "raw", "crushed"], krows)]
    pm = acc.get("per_model", {})
    spend = "; ".join(f"{m}: {t['api_requests']} requests, ${t['cost_usd_list'] or 0:.2f}" for m, t in pm.items())
    lines += ["", f"Spend: {spend}. Total {acc.get('calls_used')} model calls (cap {acc.get('cap')}), "
                  f"${acc.get('cost_usd_list_total', 0):.2f} at list price. Real auth.json unchanged: {acc.get('real_auth_unchanged')}. "
                  f"Questions dropped before asking: {acc.get('questions_dropped_unfair')} (`set`: the question quotes an anchor line "
                  f"not verbatim in the crushed text; `set:collapsed`: a `lines:next`/`lines:gap` question whose answer is a line "
                  f"the LOG collapse removed, rule fixed before the run)."]
    return "\n".join(lines)


def sec_checks(pc, pcv):
    rows = []
    for name, d in (("default", pc), ("candidates", pcv)):
        for s, r in (d or {}).items():
            rows.append([name, s, r.get("log_samples", 0), f"{r.get('log_lines_missing', 0)} / {fmt(r.get('log_lines_kept_needed', 0))}",
                         f"{r.get('failure_lines_missing', 0)} / {r.get('failure_lines', 0)}",
                         f"{r.get('summary_lines_missing', 0)} / {r.get('summary_lines', 0)}",
                         f"{r.get('diagnostic_lines_missing', 0)} / {r.get('diagnostic_lines', 0)}",
                         r.get("note_count_mismatches", 0), fmt(r.get("lines_collapsed", 0)), r.get("json_samples", 0),
                         r.get("json_token_mismatches", 0)])
    return table(["config", "set", "LOG results", "lines that must stay, missing", "failure lines missing", "summary lines missing",
                  "diagnostic / code-frame lines missing", "note-count mismatches", "lines collapsed", "JSON results",
                  "JSON token-stream mismatches"], rows)


def sec_layouts(lay):
    if not lay:
        return "(not measured)"
    rows = []
    for name, v in lay.items():
        for vn in [k for k in v[M5] if k != "raw"]:
            a5, a4 = v[M5][vn], v[M4][vn]
            rows.append([name, v["samples"], vn, f"{a5['saved_pct']:.2f}%", f"{a4['saved_pct']:.2f}%", f"{a5['grew']} / {a4['grew']}",
                         a5["median_saved"]])
    return table(["output", "samples", "candidate", "saved 5.x", "saved 4.x", "results that grew 5.x / 4.x", "median tokens saved per result 5.x"],
                 rows)


def sec_latency(syn, synv):
    rows = []
    for name, d in (("default", syn), ("candidates", synv)):
        for x in d or []:
            if x["shape"] in SHAPES:
                rows.append([name, x["shape"], fmt(x["bytes"]), x["why"], x["transforms"] or "–", f"{x['best_us']:.0f}",
                             f"{x['p50_us']:.0f}", f"{x['p99_us']:.0f}"])
    return table(["config", "shape", "bytes", "result", "transforms", "best µs", "p50 µs", "p99 µs"], rows)


def render(md: str, replace) -> str:
    tc, ts = load("tokens.corpus.json"), load("tokens.corpus.search_on.json")
    md = replace(md, "p2b_tokens", sec_tokens(tc, ts))
    md = replace(md, "p2b_spill", sec_spill(tc, load("spill.corpus.json"), load("spill.corpus.search_on.json"),
                                            load("spill.fixtures.json"), load("spill.fixtures.search_on.json")))
    acc = load("accuracy_p2b.summary.json") or load("accuracy.summary.json")
    if acc:
        md = replace(md, "p2b_accuracy", sec_accuracy(acc))
    md = replace(md, "p2b_checks", sec_checks(load("p2b_checks.json"), load("p2b_checks.search_on.json")))
    md = replace(md, "p2b_layouts", sec_layouts(load("p2b_layouts.json")))
    md = replace(md, "p2b_latency", sec_latency(load("synthetic.json"), load("synthetic.search_on.json")))
    return md
