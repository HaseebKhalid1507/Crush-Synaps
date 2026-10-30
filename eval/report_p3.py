#!/usr/bin/env python3
"""Phase 3 sections of eval/RESULTS.md (the crush-ext shim, after-spill
savings with its spill rule, test_pass v2), rendered from eval/out/*.json and
the private accuracy records. Aggregates only for real data. Called by
report.py (`p3_*` markers)."""
from __future__ import annotations

import re
from collections import Counter, defaultdict
from pathlib import Path

from report import CORPUS_DIR, M4, M5, OUT, fmt, load, parity, pct, read_jsonl, table

ALL_TOOL_TOKENS_5 = 63_635_115  # all tool-result tokens, real, 5.x (section 1)


def sec_shim(sh):
    if not sh:
        return "(not run)"
    ok = sum(c["ok"] for c in sh["checks"])
    rows = [[("ok" if c["ok"] else "**FAIL**"), c["check"]] for c in sh["checks"]]
    t = sh.get("timing", {})
    lines = [f"{ok} of {len(sh['checks'])} checks pass (`eval/shim_checks.py`, the release binary over JSON-RPC).", "",
             table(["", "check"], rows), ""]
    trows = []
    for k, label in (("in_process_hook", "in-process, every hook request (parse → response written)"),
                     ("fixtures", "round trip, every fixture × 10"), ("build_log_256k", "round trip, 256 KiB build log (lossy, raw written)"),
                     ("blob_256k", "round trip, 256 KiB base64 (lossy, raw written)"), ("poll_256k", "round trip, 205 KiB repeated lines")):
        v = t.get(k)
        if v:
            trows.append([label, fmt(v.get("requests") or 50), f"{v['p50_ms']:.3f}", f"{v['p99_ms']:.3f}"])
    lines += ["Shim time per request (ms). Round trips include Python's JSON encoding and the pipe:", "",
              table(["what", "requests", "p50 ms", "p99 ms"], trows)]
    return "\n".join(lines)


def _spill_rows(name, d):
    rows = []
    for lab, a in sorted((d or {}).get("by_label", {}).items(), key=lambda kv: (kv[0] == "all", kv[0])):
        saved = a["raw_5"] - a["shim_5"]
        fl = f"{a['fail_visible_raw']} / {a['fail_visible_shim']} of {a['fail_lines']}" if a.get("fail_lines") else "–"
        rows.append([name, "**all**" if lab == "all" else lab, a["fired"], a["replaced"], fmt(a["raw_5"]), fmt(a["A_5"]),
                     fmt(a["shim_5"]), f"{fmt(saved)} ({pct(saved, a['raw_5'])})", f"{pct(a['raw_4'] - a['shim_4'], a['raw_4'])}",
                     f"{a['A_costlier_5']} / {a['A_costlier_4']}", f"{a['shim_costlier_5']} / {a['shim_costlier_4']}", fl])
    return rows


def sec_spill(dc, df, tc, tf):
    if not dc and not df:
        return "(not run)"
    rows = []
    for name, d in (("default, corpus", dc), ("default, fixtures", df), ("+ test_group, corpus", tc), ("+ test_group, fixtures", tf)):
        rows += _spill_rows(name, d)
    lines = [table(["config, set", "transforms fired", "fired", "shim replaced", "after spill: raw 5.x", "A: always replace",
                    "shim", "shim saved 5.x", "shim saved 4.x", "A costlier 5.x / 4.x", "shim costlier 5.x / 4.x",
                    "failure lines visible: raw / shim"], rows), ""]
    for name, d in (("default, corpus", dc), ("+ test_group, corpus", tc), ("+ test_group, fixtures", tf)):
        if d and d.get("shim_costlier"):
            desc = "; ".join(f"{r['class']} {r['label']}, {fmt(r['raw_bytes'])} → {fmt(r['crushed_bytes'])} B, "
                             f"+{r['excess_5']} tokens, failure lines visible {r['fail_visible_raw']} → {r['fail_visible_shim']}"
                             for r in d["shim_costlier"])
            lines.append(f"- Costlier after spill under the shim ({name}): {desc} (the LOG failures exception).")
        if d:
            lines.append(f"- Shim pass-throughs on fired results ({name}): {d['shim_passed_on_fired']}.")
    return "\n".join(lines)


def sec_tg_tokens(tc):
    if not tc:
        return "(corpus token pass not run)"
    rows = []
    for m in (M5, M4):
        by = tc["fired"][m]["by"]
        for k, v in sorted(by.items()):
            if "test_group" in k:
                s = v["raw_tok"] - v["crushed_tok"]
                rows.append([m.replace("claude-", ""), k.split("|")[1], v["n"], fmt(v["raw_tok"]), fmt(s), pct(s, v["raw_tok"]),
                             f"{100 * s / ALL_TOOL_TOKENS_5:.3f}%" if m == M5 else "–"])
    grew = {m.replace("claude-", ""): tc["fired"][m]["grew_real_tokens"] for m in (M5, M4)}
    lines = [table(["family", "transforms", "fired", "raw tokens", "saved", "saved on fired", "% of all tool-result tokens"], rows), "",
             f"Corpus results it fired on that grew in real tokens: {grew}."]
    return "\n".join(lines)


def p3_records(acc):
    fx = OUT / "accuracy.fixtures.p3_tg.jsonl"
    recs = list(read_jsonl(fx)) if fx.exists() else []
    for rn in acc.get("runs", [acc.get("run")]):
        f = CORPUS_DIR / "accuracy" / f"{rn}.jsonl"
        if f.exists():
            recs += list(read_jsonl(f))
    return recs


def sec_accuracy(acc):
    if not acc:
        return "(not run)"
    recs = [r for r in p3_records(acc) if "test_group" in (r.get("label") or "").split("+")]
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
        if r["variant"] in ("raw", "crushed"):
            k = (r["model"].replace("claude-", ""), r["kind"])
            kinds[k][r["variant"]] += r["correct"]
            kinds[k][r["variant"] + "_n"] += 1
    krows = [[m, k, f"{c['raw']}/{c['raw_n']}", f"{c['crushed']}/{c['crushed_n']}"] for (m, k), c in sorted(kinds.items())]
    lines += ["", "By question kind:", "", table(["model", "kind", "raw", "crushed"], krows)]
    pm = acc.get("per_model", {})
    spend = "; ".join(f"{m}: {t['api_requests']} requests, ${t['cost_usd_list'] or 0:.2f}" for m, t in pm.items())
    lines += ["", f"Spend: {spend}. Total {acc.get('calls_used')} model calls (cap {acc.get('cap')}), "
                  f"${acc.get('cost_usd_list_total', 0):.2f} at list price. Real auth.json unchanged: {acc.get('real_auth_unchanged')}. "
                  f"Questions dropped before asking: {acc.get('questions_dropped_unfair')}."]
    return "\n".join(lines)


def _scrub(text: str) -> str:
    """The smoke test's isolated base dir is under the user's home: show it
    as `<isolated home>` (never commit a local path or user name)."""
    return re.sub(r"/\S*/smoke\d+(?=/)", "<isolated home>", text).replace(str(Path.home()), "~")


def sec_smoke(sm):
    if not sm:
        return "(not run)"
    ok = sum(c["ok"] for c in sm["checks"])
    rows = [[("ok" if c["ok"] else "**FAIL**"), c["check"]] for c in sm["checks"]]
    hb, ob = sm.get("history_bytes", {}), sm.get("output_bytes", {})
    sizes = [[k, fmt(ob.get(k, 0)), fmt(hb.get(k, 0))] for k in ("wait.sh", "build.sh", "flood.sh")]
    return "\n".join([
        f"`{sm.get('synaps')} rpc`, model {sm.get('model')}, {sm.get('model_calls')} model calls; plugin built by "
        f"`scripts/setup.sh` in {sm.get('setup_secs')} s. {ok} of {len(sm['checks'])} checks pass.", "",
        table(["", "check"], rows), "",
        table(["command", "output bytes", "bytes in session history"], sizes), "",
        "`crush-ext --report` afterwards (the backing stats of `/crush`):", "", "```text", _scrub((sm.get("crush_report") or "").strip()), "```"])


def render(md: str, replace) -> str:
    md = replace(md, "p3_shim", sec_shim(load("shim.json")))
    md = replace(md, "p3_spill", sec_spill(load("p3_spill.corpus.json"), load("p3_spill.fixtures.json"),
                                           load("p3_spill.corpus.p3_tg.json"), load("p3_spill.fixtures.p3_tg.json")))
    md = replace(md, "p3_tg_tokens", sec_tg_tokens(load("tokens.corpus.p3_tg.json")))
    acc = load("accuracy.summary.p3_tg.json")
    if acc:
        md = replace(md, "p3_tg_accuracy", sec_accuracy(acc))
    md = replace(md, "p3_smoke", sec_smoke(load("smoke.json")))
    return md
