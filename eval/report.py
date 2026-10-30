#!/usr/bin/env python3
"""Build eval/RESULTS.md (crush 2 gate report) from eval/out/*.json and the
private accuracy records. Aggregates only for real data; per-item rows only
for the synthetic fixtures. Sections live between `<!-- BEGIN:x -->` /
`<!-- END:x -->` markers so hand-written text around them survives.
"""
from __future__ import annotations

import json
import sys
from collections import Counter, defaultdict
from math import comb
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from common import CLASSES, CORPUS_DIR, EVAL, OUT, read_jsonl  # noqa: E402

M4, M5 = "claude-sonnet-4-6", "claude-opus-5-5"
RESULTS = EVAL / "RESULTS.md"
CLS = ["CONTENT", "SEARCH", "UNKNOWN", "PROSE", "LISTING", "VCS", "LOG", "STRUCTURED"]


def load(name):
    p = OUT / name
    return json.loads(p.read_text()) if p.exists() else None


def fmt(n):
    return "–" if n is None else f"{int(round(n)):,}"


def pct(a, b, d=1):
    return "–" if not b else f"{100 * a / b:.{d}f}%"


def table(head, rows):
    out = ["| " + " | ".join(head) + " |", "|" + "|".join("---" if i == 0 else "---:" for i in range(len(head))) + "|"]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


def mcnemar_p(b: int, c: int):
    n = b + c
    if n == 0:
        return None
    k = min(b, c)
    return min(1.0, 2 * sum(comb(n, i) for i in range(k + 1)) / 2 ** n)


# ── sections ────────────────────────────────────────────────────────────────

def sec_tokens(tc, run):
    if not tc or "composition" not in tc:
        return "(corpus token pass not run)"
    comp = defaultdict(Counter)
    for r in tc["composition"]:
        for k in ("samples", "bytes", f"tok:{M4}", f"tok:{M5}"):
            comp[r["class"]][k] += r[k]
    by = {m: tc["fired"][m]["by"] for m in (M5, M4)}
    rows = []
    tot = Counter()
    for c in CLS:
        if c not in comp:
            continue
        f5 = by[M5].get(f"{c}|*", {})
        f4 = by[M4].get(f"{c}|*", {})
        s5 = f5.get("raw_tok", 0) - f5.get("crushed_tok", 0)
        s4 = f4.get("raw_tok", 0) - f4.get("crushed_tok", 0)
        n = comp[c]["samples"]
        rows.append([c, fmt(n), fmt(f5.get("n", 0)), pct(f5.get("n", 0), n), fmt(comp[c][f"tok:{M5}"]), fmt(s5),
                     pct(s5, comp[c][f"tok:{M5}"], 2), pct(s4, comp[c][f"tok:{M4}"], 2),
                     pct(s5, f5.get("raw_tok", 0)) if f5 else "–"])
        tot.update({"n": n, "fired": f5.get("n", 0), "t5": comp[c][f"tok:{M5}"], "t4": comp[c][f"tok:{M4}"], "s5": s5, "s4": s4,
                    "fr5": f5.get("raw_tok", 0)})
    rows.append(["**total**", fmt(tot["n"]), fmt(tot["fired"]), pct(tot["fired"], tot["n"]), fmt(tot["t5"]), fmt(tot["s5"]),
                 pct(tot["s5"], tot["t5"], 2), pct(tot["s4"], tot["t4"], 2), pct(tot["s5"], tot["fr5"])])
    lines = [table(["class (crush-core)", "samples", "fired", "fire rate", "tokens 5.x", "saved 5.x", "saved 5.x %",
                    "saved 4.x %", "saved on fired 5.x"], rows), ""]
    lab = []
    for k, v in sorted(by[M5].items()):
        if k.endswith("|*") or k == "*|*":
            continue
        c, l = k.split("|", 1)
        v4 = by[M4][k]
        lab.append([c, l, fmt(v["n"]), fmt(v["raw_tok"]), pct(v["raw_tok"] - v["crushed_tok"], v["raw_tok"]),
                    pct(v4["raw_tok"] - v4["crushed_tok"], v4["raw_tok"]), pct(v["raw_bytes"] - v["crushed_bytes"], v["raw_bytes"])])
    lines.append("By class and transform (savings on the fired results themselves):\n")
    lines.append(table(["class", "transform", "fired", "raw tokens 5.x", "saved 5.x", "saved 4.x", "saved bytes"], lab))
    g = {m: tc["fired"][m] for m in (M5, M4)}
    lines.append("")
    lines.append(f"Real-token check on **every** fired result: more tokens than raw: {g[M5]['grew_real_tokens']} (5.x), "
                 f"{g[M4]['grew_real_tokens']} (4.x); equal: {g[M5]['equal_real_tokens']} / {g[M4]['equal_real_tokens']}. "
                 f"Per-result saving p10/p50/p90 (5.x): {g[M5]['saving_pct_p10_p50_p90']}%. "
                 f"Estimated vs real saving (5.x): {fmt(g[M5]['estimated_saving_vs_real']['sum_est'])} vs "
                 f"{fmt(g[M5]['estimated_saving_vs_real']['sum_real'])} tokens; per-result median |error| "
                 f"{g[M5]['estimated_saving_vs_real']['median_rel_err']:.1%}, p90 {g[M5]['estimated_saving_vs_real']['p90_rel_err']:.1%}.")
    if run:
        lines.append(f"\nPass-through reasons (all {fmt(run['samples'])} results): " +
                     ", ".join(f"{k} {fmt(v)}" for k, v in sorted(run["why"].items(), key=lambda kv: -kv[1])) + ".")
    return "\n".join(lines)


def sec_fixtures(tf):
    if not tf:
        return "(not run)"
    rows = []
    a = Counter()
    for r in tf["per_fixture"]:
        r5, c5, r4, c4 = r[f"raw_tok:{M5}"], r[f"crushed_tok:{M5}"], r[f"raw_tok:{M4}"], r[f"crushed_tok:{M4}"]
        a.update({"b": r["raw_bytes"], "cb": r["crushed_bytes"], "r5": r5, "c5": c5, "r4": r4, "c4": c4})
        if r["fired"]:
            rows.append([r["name"], r["class"], r["label"], fmt(r["raw_bytes"]), fmt(r5), pct(r["raw_bytes"] - r["crushed_bytes"], r["raw_bytes"]),
                         pct(r5 - c5, r5), pct(r4 - c4, r4)])
    rows.append([f"**all {len(tf['per_fixture'])} fixtures**", "", "", fmt(a["b"]), fmt(a["r5"]), pct(a["b"] - a["cb"], a["b"]),
                 pct(a["r5"] - a["c5"], a["r5"]), pct(a["r4"] - a["c4"], a["r4"])])
    return table(["fixture (fired)", "class", "transform", "bytes", "tokens 5.x", "saved bytes", "saved 5.x", "saved 4.x"], rows)


def sec_edit(es):
    if not es:
        return "(not run)"
    rows = []
    for s, r in es.items():
        rows.append([s, r["content_changed"], r["search_group_samples"], r["search_rebuild_failures"],
                     f"{fmt(r['hit_lines_verbatim'])} / {fmt(r['hit_lines'])}",
                     f"{r['content_p0_changed']} ({', '.join(f'{k} {v}' for k, v in r['content_p0_by_transform'].items())}); "
                     f"{r['content_p0_lines_altered']} lines not rebuildable"])
    return table(["set", "CONTENT changed (crush-core class)", "SEARCH grouped", "SEARCH rebuild failures",
                  "grep hit lines whose `N:text` is still a whole line", "Phase 0 classifier says CONTENT, crush changed"], rows)


def accuracy_records(acc):
    recs = list(read_jsonl(OUT / "accuracy.fixtures.jsonl")) if (OUT / "accuracy.fixtures.jsonl").exists() else []
    for rn in acc.get("runs", [acc.get("run")]):
        f = CORPUS_DIR / "accuracy" / f"{rn}.jsonl"
        if f.exists():
            recs += list(read_jsonl(f))
    return recs


def parity(recs):
    """Per (transform, model): paired raw vs crushed + raw-vs-raw noise."""
    out = {}
    noise = defaultdict(lambda: [0, 0])
    idx = defaultdict(dict)
    for r in recs:
        idx[(r["model"], r.get("label"), r["set"], r["sample"], r["q"])][r["variant"]] = r["correct"]
    agg = defaultdict(Counter)
    for (m, lab, st, smp, q), d in idx.items():
        if "raw" in d and "raw2" in d:
            noise[m][0] += 1
            noise[m][1] += d["raw"] != d["raw2"]
        if "raw" in d and "crushed" in d:
            a = agg[(lab, m)]
            a["pairs"] += 1
            a["raw"] += d["raw"]
            a["crushed"] += d["crushed"]
            a["raw_only"] += d["raw"] and not d["crushed"]
            a["crushed_only"] += d["crushed"] and not d["raw"]
            a["samples_" + st + ":" + smp] = 1
    pooled = defaultdict(Counter)
    for (lab, m), a in list(agg.items()):
        pooled[(lab, "both models")].update({k: v for k, v in a.items()})
    for k, v in pooled.items():
        agg[k] = v
    nb = [sum(n[0] for n in noise.values()), sum(n[1] for n in noise.values())]
    noise["both models"] = nb
    for (lab, m), a in agg.items():
        nz = noise[m][1] / noise[m][0] if noise[m][0] else 0.0
        p = mcnemar_p(a["raw_only"], a["crushed_only"])
        drop = (a["raw_only"] - a["crushed_only"]) / a["pairs"] if a["pairs"] else 0
        out[(lab, m)] = {"pairs": a["pairs"], "samples": sum(1 for k in a if k.startswith("samples_")), "raw": a["raw"],
                         "crushed": a["crushed"], "raw_only": a["raw_only"], "crushed_only": a["crushed_only"], "p": p,
                         "noise": nz, "noise_n": noise[m][0], "drop": drop,
                         "parity": (p is None or p >= 0.05) and drop <= max(nz, 0.0) + 1e-9}
    return out


def sec_accuracy(acc):
    if not acc:
        return "(not run)"
    recs = accuracy_records(acc)
    par = parity(recs)
    rows = []
    verdict = defaultdict(lambda: True)
    for (lab, m), v in par.items():
        if m != "both models":
            verdict[lab] &= v["parity"]
    for (lab, m), v in sorted(par.items(), key=lambda kv: (str(kv[0][0]), kv[0][1] == "both models", kv[0][1])):
        rows.append([lab, m.replace("claude-", ""), v["samples"], v["pairs"], f"{v['raw']}/{v['pairs']} ({pct(v['raw'], v['pairs'])})",
                     f"{v['crushed']}/{v['pairs']} ({pct(v['crushed'], v['pairs'])})", f"{v['raw_only']} / {v['crushed_only']}",
                     "–" if v["p"] is None else f"{v['p']:.3f}", f"{v['noise']:.1%} (n={v['noise_n']})",
                     ("**parity**" if v["parity"] else "**FAIL**") if m != "both models" else
                     ("ships" if verdict[lab] else "**disabled**")])
    lines = [table(["transform", "model", "samples", "questions", "raw correct", "crushed correct", "discordant raw-only / crushed-only",
                    "McNemar p", "raw-vs-raw disagreement", "verdict"], rows)]
    kinds = defaultdict(lambda: Counter())
    for r in recs:
        kinds[(r.get("label"), r["kind"])][r["variant"]] += r["correct"]
        kinds[(r.get("label"), r["kind"])][r["variant"] + "_n"] += 1
    krows = [[lab, k, f"{c['raw']}/{c['raw_n']}", f"{c['crushed']}/{c['crushed_n']}"] for (lab, k), c in sorted(kinds.items(), key=lambda kv: (str(kv[0][0]), kv[0][1]))]
    lines += ["", "By question kind (both models pooled):", "", table(["transform", "kind", "raw", "crushed"], krows)]
    pm = acc.get("per_model", {})
    spend = "; ".join(f"{m}: {t['api_requests']} requests, ${t['cost_usd_list'] or 0:.2f}" for m, t in pm.items())
    lines += ["", f"Spend: {spend}. Total {acc.get('calls_used')} model calls (cap {acc.get('cap')}), "
                  f"${acc.get('cost_usd_list_total', 0):.2f} at list price. Real auth.json unchanged: {acc.get('real_auth_unchanged')}. "
                  f"Questions dropped as unfair (they quote a line not verbatim in the crushed text): {acc.get('questions_dropped_unfair')}."]
    (OUT / "accuracy.parity.json").write_text(json.dumps({f"{k[0]}|{k[1]}": v for k, v in par.items()}, indent=1))
    return "\n".join(lines)


def sec_classifier(c0, c2, tc):
    if not c2:
        return "(not run)"
    lines = []
    a0, a2 = (c0 or {}).get("audit"), c2.get("audit")
    if a0 and a2:
        lines.append(f"Hand audit ({a2['n']} seeded real samples, labels from Phase 0): Phase 0 classifier **{a0['accuracy']:.0%}**, "
                     f"crush-core **{a2['accuracy']:.0%}** ({a2['predicted_prose_scored_as_unknown']} PROSE prediction scored as UNKNOWN: "
                     "the audit's label set predates PROSE). Recall by true class: " +
                     ", ".join(f"{k} {v}" for k, v in a2["per_class_recall"].items()) + ".")
    if tc and "composition" in tc and "composition_p0" in tc:
        def share(key):
            agg = defaultdict(Counter)
            for r in tc[key]:
                for k in ("samples", "bytes", f"tok:{M4}", f"tok:{M5}"):
                    agg[r["class"]][k] += r[k]
            tot = Counter()
            for v in agg.values():
                tot.update(v)
            return agg, tot
        a0c, t0 = share("composition_p0")
        a2c, t2 = share("composition")
        rows = []
        for c in CLS:
            rows.append([c, pct(a0c[c][f"tok:{M5}"], t0[f"tok:{M5}"]), pct(a2c[c][f"tok:{M5}"], t2[f"tok:{M5}"]),
                         pct(a0c[c][f"tok:{M4}"], t0[f"tok:{M4}"]), pct(a2c[c][f"tok:{M4}"], t2[f"tok:{M4}"]),
                         pct(a0c[c]["bytes"], t0["bytes"]), pct(a2c[c]["bytes"], t2["bytes"])])
        lines += ["", table(["class", "Phase 0 (5.x tokens)", "crush-core (5.x tokens)", "Phase 0 (4.x)", "crush-core (4.x)",
                              "Phase 0 (bytes)", "crush-core (bytes)"], rows)]
    unk = c2.get("unknown_bytes_by_family")
    if unk:
        lines.append("\nLargest remaining UNKNOWN families (bytes): " +
                     ", ".join(f"{k} {v / 1e6:.1f} MB" for k, v in list(unk.items())[:10]) + ".")
    return "\n".join(lines)


def sec_latency(rf, rc, syn):
    rows = []
    for name, r in (("real corpus", rc), ("fixtures", rf)):
        if not r:
            continue
        ip, e = r["in_process_ns"], r["e2e"]
        rows.append([name, fmt(r["samples"]), f"{ip['p50_all'] / 1e3:.1f} / {ip['p99_all'] / 1e3:.0f} µs",
                     f"{ip['p50_ge2k'] / 1e3:.0f} / {ip['p99_ge2k'] / 1e3:.0f} µs", f"{(ip['p50_fired'] or 0) / 1e3:.0f} / {(ip['p99_fired'] or 0) / 1e3:.0f} µs",
                     f"{ip['max_all'] / 1e3:.0f} µs", f"{e['rtt_ns']['p50'] / 1e3:.0f} / {e['rtt_ns']['p99'] / 1e3:.0f} µs ({fmt(e['requests'])})",
                     r["panics"], f"{e['ok']}/{e['requests']}", e["mismatch_vs_inprocess"]])
    out = [table(["set", "results", "in-process p50/p99 (all)", "p50/p99 (≥ 2 KiB)", "p50/p99 (fired)", "max", "crush-ext JSON-RPC round trip p50/p99",
                  "panics", "crush-ext ok", "ext ≠ library"], rows)]
    if syn:
        srows = [[x["shape"], fmt(x["bytes"]), x["why"], x["transforms"] or "–", f"{x['best_us']:.0f}", f"{x['p50_us']:.0f}", f"{x['p99_us']:.0f}"] for x in syn]
        out += ["", "256 KiB worst cases (200 runs each):", "", table(["shape", "bytes", "result", "transforms", "best µs", "p50 µs", "p99 µs"], srows)]
    return "\n".join(out)


def sec_estimator(est, lay):
    out = []
    if est:
        rows = []
        for k, v in est["fit"].items():
            t = v["test"]
            rows.append([k, est["models"][k], t["n"], f"{t['sum_ratio']:.3f}", f"{t['median_abs_rel_err']:.1%}", f"{t['p90_abs_rel_err']:.1%}",
                         f"{t['p95_abs_rel_err']:.1%}", f"{t['median_abs_rel_err_ge500']:.1%}", f"{t['p95_abs_rel_err_ge500']:.1%}"])
        out.append(f"Fitted on {est['train']} real results (stratified class × size), scored on {est['test']} held out:\n")
        out.append(table(["family", "model", "held-out n", "Σest/Σreal", "median |err|", "p90", "p95", "median (≥500 tok)", "p95 (≥500 tok)"], rows))
    if lay:
        rows = [[m, fmt(v["raw"])] + [f"{v[k + '_saving_pct']}% ({v[k + '_grew']} grew)" for k in ("heading", "blank", "colon", "inline")]
                for m, v in lay["by_model"].items()]
        out += ["", f"SEARCH layout, {lay['samples']} seeded fired real results, real tokens:", "",
                table(["model", "raw tokens", "rg --heading", "+ blank between groups", "`path:` heading", "singles inline"], rows)]
    return "\n".join(out) or "(not run)"


def replace(md: str, name: str, content: str) -> str:
    a, b = f"<!-- BEGIN:{name} -->", f"<!-- END:{name} -->"
    # an input this checkout doesn't have (e.g. a phase's private accuracy
    # records) keeps the section as it was
    if content.startswith("(not run)") or content.startswith("(corpus token pass not run)"):
        if a in md and b in md:
            return md
    if a in md and b in md:
        pre, rest = md.split(a, 1)
        _, post = rest.split(b, 1)
        return f"{pre}{a}\n{content}\n{b}{post}"
    return md + f"\n{a}\n{content}\n{b}\n"


def main() -> int:
    md = RESULTS.read_text() if RESULTS.exists() else "# crush 2 — Phase 1 gate results\n"
    tc, tf = load("tokens.corpus.json"), load("tokens.fixtures.json")
    rc, rf = load("corpus.run.json"), load("fixtures.run.json")
    md = replace(md, "tokens", sec_tokens(tc, rc))
    md = replace(md, "fixtures", sec_fixtures(tf))
    ts = load("tokens.corpus.search_on.json")
    if ts and tc and "composition" not in ts:
        ts["composition"] = tc.get("composition")  # same classifier, same corpus
    md = replace(md, "tokens_search_on", sec_tokens(ts, load("corpus.run.search_on.json")))
    md = replace(md, "fixtures_search_on", sec_fixtures(load("tokens.fixtures.search_on.json")))
    md = replace(md, "edit", sec_edit(load("edit_safety.json")))
    md = replace(md, "edit_search_on", sec_edit(load("edit_safety.search_on.json")))
    md = replace(md, "latency_search_on", sec_latency(load("fixtures.run.search_on.json"), load("corpus.run.search_on.json"),
                                                      load("synthetic.search_on.json")))
    md = replace(md, "accuracy", sec_accuracy(load("accuracy.summary.json")))
    md = replace(md, "classifier", sec_classifier(load("classifier.json"), load("classifier.crush2.json"), tc))
    md = replace(md, "latency", sec_latency(rf, rc, load("synthetic.json")))
    md = replace(md, "estimator", sec_estimator(load("estimator.json"), load("search_layout.json")))
    import report_p2a  # Phase 2A: SEARCH layout and LISTING transforms
    md = report_p2a.apply(md, replace)
    import report_p2b  # Phase 2B: LOG / VCS / STRUCTURED
    md = report_p2b.render(md, replace)
    import report_p3  # Phase 3: the crush-ext shim, after-spill savings, test_pass v2
    md = report_p3.render(md, replace)
    RESULTS.write_text(md)
    print(f"wrote {RESULTS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
