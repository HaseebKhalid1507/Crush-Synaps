#!/usr/bin/env python3
"""Phase 2A sections of eval/RESULTS.md (SEARCH layout, LISTING transforms).

Only `<!-- BEGIN:p2a_* -->` sections are written; everything else in
RESULTS.md is left alone. Inputs (all aggregate, in eval/out):
anatomy.json, tokens.{corpus,fixtures}.p2a_on.json,
after_spill.{corpus,fixtures}.p2a_on.json, edit_safety.p2a_on.json,
synthetic.p2a_on.json and the accuracy summaries accuracy.summary.p2a*.json
(per-answer records stay private in $CRUSH_CORPUS_DIR/accuracy).

Accuracy rounds (ROUNDS below): each names the summary file, the transform
label, and the variant it tested. A transform's verdict pools the rounds that
tested the same code; parity follows the Phase 1 rule (McNemar p >= 0.05 and
the raw-only excess within the raw-vs-raw disagreement, per model). Questions
whose quoted text crush cut (`cut:` kinds: the answer was moved to the raw
file) are reported in their own table and kept out of the verdict.
"""
from __future__ import annotations

import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import CORPUS_DIR, OUT, read_jsonl  # noqa: E402
from report import M4, M5, fmt, parity, pct, table  # noqa: E402

# (summary tag, label, variant name, verdict group)
ROUNDS = [
    ("p2a", "search_elide", "round 1", "search_elide"),
    ("p2a_el2", "search_elide", "round 2 (fresh samples)", "search_elide"),
    ("p2a", "ls_columns", "v1 note (round 1)", "ls_columns v1"),
    ("p2a_ls2", "ls_columns", "v2 note (round 2, fresh samples)", "ls_columns v2"),
]


def load(name):
    p = OUT / name
    return json.loads(p.read_text()) if p.exists() else None


def records(tag: str):
    s = load(f"accuracy.summary.{tag}.json")
    if not s:
        return None, []
    recs = []
    for rn in s.get("runs", [s.get("run")]):
        f = CORPUS_DIR / "accuracy" / f"{rn}.jsonl"
        if f.exists():
            recs += list(read_jsonl(f))
    return s, recs


def sec_anatomy(a):
    if not a:
        return None
    out = []
    S = a.get("SEARCH")
    if S:
        rows = []
        for m in (M5, M4):
            t = S["tokens"][m]
            o = t["over_spill_threshold"]
            rows.append([m.replace("claude-", ""), fmt(t["full"])] + [f"{t['share:' + k]}%" for k in
                        ("path", "lineno", "hit_text", "context", "other", "long", "dup")] +
                        [f"{o['results']} results, {pct(o['tokens'], t['full'])} → {pct(o['tokens_after_spill'], t['full'])}"])
        out += [f"SEARCH: {fmt(S['samples'])} real results, every one counted (shares by deletion, so they overlap: "
                "`context` and `long` lines are also hit/other text).", "",
                table(["tokenizer", "tokens", "paths", "line numbers", "hit text", "context lines + `--`", "other lines",
                       "lines > 500 chars", "duplicate lines", "> 12 KiB (raw → after spill)"], rows), ""]
        t = S["tokens"][M5]
        out.append("By shape (5.x): " + ", ".join(f"{k} {v['share']}% ({fmt(v['results'])})" for k, v in t["by_shape"].items()) +
                   ". By hit lines per result: " + ", ".join(f"{k} {v['share']}% ({fmt(v['results'])})" for k, v in t["by_hits"].items()) + ".")
    L = a.get("LISTING")
    if L:
        t5, t4 = L["tokens"][M5], L["tokens"][M4]
        o = t5["over_spill_threshold"]
        out += ["", f"LISTING: {fmt(L['samples'])} real results, {fmt(t5['full'])} tokens (5.x). By command (5.x): " +
                ", ".join(f"{k} {v['share']}%" for k, v in list(t5["by_command"].items())[:10]) + ".",
                f"`ls -l` rows: {t5['ls_l_rows_share']}% of LISTING tokens ({fmt(L['ls_l']['rows'])} rows in "
                f"{fmt(L['ls_l']['results'])} results); columns constant across a listing (links/owner/group) are "
                f"{t5['ls_l_constant_cols_share_of_rows']}% of those rows' tokens (5.x; 4.x {t4['ls_l_constant_cols_share_of_rows']}%), "
                f"i.e. at most {t5['ls_l_rows_share'] * t5['ls_l_constant_cols_share_of_rows'] / 100:.1f}% of LISTING. "
                f"Over 12 KiB: {o['results']} results, {pct(o['tokens'], t5['full'])} of LISTING tokens raw, "
                f"{pct(o['tokens_after_spill'], t5['full'])} after spill."]
    return "\n".join(out)


def sec_tokens(tc, sp, anat):
    if not tc or not sp:
        return None
    rows = []
    for lab in ("search_elide", "ls_columns"):
        cls = "SEARCH" if lab == "search_elide" else "LISTING"
        r = []
        for m in (M5, M4):
            v = tc["fired"][m]["by"].get(f"{cls}|{lab}")
            s = sp["by_model"][m].get(f"{cls}|{lab}")
            if not v or not s:
                r = None
                break
            full = anat[cls]["tokens"][m]["full"] if anat and cls in anat else None
            o = anat[cls]["tokens"][m]["over_spill_threshold"] if anat and cls in anat else None
            after_full = full - o["tokens"] + o["tokens_after_spill"] if o else None
            r.append((v, s, full, after_full))
        if not r:
            continue
        (v5, s5, f5, a5), (v4, s4, f4, a4) = r
        sv5, sv4 = v5["raw_tok"] - v5["crushed_tok"], v4["raw_tok"] - v4["crushed_tok"]
        ss5, ss4 = s5["raw_after_spill"] - s5["crushed_after_spill"], s4["raw_after_spill"] - s4["crushed_after_spill"]
        rows.append([lab, fmt(v5["n"]), fmt(v5["raw_tok"]), f"{pct(sv5, v5['raw_tok'])} / {pct(sv4, v4['raw_tok'])}",
                     f"{fmt(sv5)} ({pct(sv5, f5, 2)} of {cls})", f"{pct(ss5, s5['raw_after_spill'])} / {pct(ss4, s4['raw_after_spill'])}",
                     f"{fmt(ss5)} ({pct(ss5, a5, 2)} of {cls} after spill)",
                     f"{s5['grew_raw']} / {s4['grew_raw']}",
                     f"{s5['grew_after_spill']} / {s4['grew_after_spill']}"])
    out = [table(["transform", "fired", "raw tokens 5.x", "saved on fired 5.x / 4.x", "saved 5.x", "after spill: saved on fired 5.x / 4.x",
                  "after spill: saved 5.x", "grew raw 5.x / 4.x", "grew after spill 5.x / 4.x"], rows)]
    reg = []
    for m in (M5,):
        for k, v in sorted(sp["by_model"][m].items()):
            if k.count("|") == 2 and any(t in k for t in ("search_elide", "ls_columns")):
                reg.append([k.split("|")[1], k.split("|")[2], fmt(v["n"]), f"{fmt(v['raw'])} → {fmt(v['crushed'])}",
                            f"{fmt(v['raw_after_spill'])} → {fmt(v['crushed_after_spill'])}", v["grew_after_spill"]])
    out += ["", "By spill regime (5.x; Synaps previews any result over 12 KiB, crush runs first):", "",
            table(["transform", "regime", "results", "tokens raw → crushed", "after spill raw → crushed", "grew after spill"], reg)]
    e = tc["fired"][M5]["estimated_saving_vs_real"]
    out.append(f"\nEvery fired result counted with count_tokens, both families. All fired results of the candidate config "
               f"(Phase 1's generic transforms included) with more real tokens than raw: {tc['fired'][M5]['grew_real_tokens']} (5.x), "
               f"{tc['fired'][M4]['grew_real_tokens']} (4.x). Estimated vs real saving (5.x, all fired): {fmt(e['sum_est'])} vs "
               f"{fmt(e['sum_real'])} tokens.")
    return "\n".join(out)


def sec_accuracy():
    groups = defaultdict(list)
    summaries = {}
    for tag, lab, name, grp in ROUNDS:
        s, recs = records(tag)
        if not s:
            continue
        summaries[tag] = s
        groups[grp].append((name, [r for r in recs if r.get("label") == lab]))
    if not groups:
        return None
    rows, cut_rows, kinds = [], [], defaultdict(Counter)
    verdict = {}
    for grp, rounds in groups.items():
        allr = [r for _, rs in rounds for r in rs]
        # ships only if every round, and the rounds pooled, are at parity on
        # both models; a round without answers for a model is a failure
        ok = True
        for name, rs in rounds + ([("pooled", allr)] if len(rounds) > 1 else []):
            par = parity([r for r in rs if not r.get("cut")])
            for m in ("claude-haiku-4-5-20251001", "claude-sonnet-4-6", "both models"):
                v = next((v for (lab, mm), v in par.items() if mm == m), None)
                if not v:
                    ok &= m == "both models"
                    continue
                if m != "both models":
                    ok &= v["parity"]
                rows.append([grp, name, m.replace("claude-", ""), v["samples"], v["pairs"],
                             f"{v['raw']}/{v['pairs']} ({pct(v['raw'], v['pairs'])})",
                             f"{v['crushed']}/{v['pairs']} ({pct(v['crushed'], v['pairs'])})",
                             f"{v['raw_only']} / {v['crushed_only']}", "–" if v["p"] is None else f"{v['p']:.3f}",
                             f"{v['noise']:.1%} (n={v['noise_n']})",
                             ("parity" if v["parity"] else "**FAIL**") if m != "both models" else ""])
        verdict[grp] = ok
        cut = [r for r in allr if r.get("cut")]
        if cut:
            c = Counter()
            for r in cut:
                c[r["variant"]] += r["correct"]
                c[r["variant"] + "_n"] += 1
            cut_rows.append([grp, f"{c['raw']}/{c['raw_n']}", f"{c['crushed']}/{c['crushed_n']}"])
        for r in allr:
            k = (grp, r["kind"])
            kinds[k][r["variant"]] += r["correct"]
            kinds[k][r["variant"] + "_n"] += 1
    out = [table(["transform", "round", "model", "samples", "questions", "raw correct", "crushed correct",
                  "discordant raw-only / crushed-only", "McNemar p", "raw-vs-raw disagreement", "per model"], rows), "",
           "Verdicts: " + "; ".join(f"**{g}: {'ships' if ok else 'disabled'}**" for g, ok in verdict.items()) + "."]
    if cut_rows:
        out += ["", "Questions quoting text crush cut (`cut:` kinds; the answer was moved to the raw file, so the crushed side "
                    "can only say \"unknown\"; kept out of the verdict, shown here):", "",
                table(["transform", "raw correct", "crushed correct"], cut_rows)]
    krows = [[g, k, f"{c['raw']}/{c['raw_n']}", f"{c['crushed']}/{c['crushed_n']}"] for (g, k), c in sorted(kinds.items())]
    out += ["", "By question kind (both models pooled):", "", table(["transform", "kind", "raw", "crushed"], krows)]
    calls = sum(s.get("calls_used", 0) for s in summaries.values())
    cost = sum(s.get("cost_usd_list_total", 0) for s in summaries.values())
    auth = all(s.get("real_auth_unchanged") for s in summaries.values())
    out.append(f"\nSpend: {calls} model calls in {len(summaries)} runs (cap 400 for the phase), ${cost:.2f} at list price; "
               f"real auth.json unchanged in every run: {auth}.")
    return "\n".join(out)


def sec_edit(es):
    if not es:
        return None
    rows = []
    for s, r in es.items():
        rows.append([s, r["content_changed"], f"{r['search_elide_samples']} ({r['search_elide_lines_cut']} lines cut, "
                     f"{fmt(r['search_elide_lines_verbatim'])} kept verbatim)", r["search_elide_failures"],
                     r["ls_columns_samples"], r["ls_columns_restore_failures"]])
    return table(["set", "CONTENT changed", "search_elide results", "cut lines not an exact slice / wrong counts",
                  "ls_columns results", "not restorable from the note"], rows)


def sec_latency(syn, run_c):
    if not syn:
        return None
    rows = [[x["shape"], fmt(x["bytes"]), x["why"], x["transforms"] or "–", f"{x['best_us']:.0f}", f"{x['p50_us']:.0f}",
             f"{x['p99_us']:.0f}"] for x in syn]
    out = [table(["shape (candidates on)", "bytes", "result", "transforms", "best µs", "p50 µs", "p99 µs"], rows)]
    if run_c:
        ip = run_c["in_process_ns"]
        out.append(f"\nReal corpus, candidates on: p50 / p99 {ip['p50_all'] / 1e3:.1f} / {ip['p99_all'] / 1e3:.0f} µs (all), "
                   f"{(ip['p50_fired'] or 0) / 1e3:.0f} / {(ip['p99_fired'] or 0) / 1e3:.0f} µs (fired), max {ip['max_all'] / 1e3:.0f} µs; "
                   f"panics {run_c['panics']}, not idempotent {run_c['not_idempotent']}, crush-ext ≠ library "
                   f"{run_c['e2e']['mismatch_vs_inprocess']} / {run_c['e2e']['requests']}.")
    return "\n".join(out)


def apply(md: str, replace) -> str:
    anat = load("anatomy.json")
    for name, content in (
        ("p2a_anatomy", sec_anatomy(anat)),
        ("p2a_tokens", sec_tokens(load("tokens.corpus.p2a_on.json"), load("after_spill.corpus.p2a_on.json"), anat)),
        ("p2a_accuracy", sec_accuracy()),
        ("p2a_edit", sec_edit(load("edit_safety.p2a_on.json"))),
        ("p2a_latency", sec_latency(load("synthetic.p2a_on.json"), load("corpus.run.p2a_on.json"))),
    ):
        if content is not None:
            md = replace(md, name, content)
    return md
