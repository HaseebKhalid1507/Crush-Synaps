#!/usr/bin/env python3
"""Build the generated sections of eval/BASELINE.md from eval/out/*.json and
the private corpus. Only aggregates are emitted for real data; per-item
detail is printed for the synthetic fixtures only.

Sections are written between `<!-- BEGIN:<name> -->` / `<!-- END:<name> -->`
markers so the hand-written commentary around them survives regeneration.
Missing inputs leave a section as "(not run)".
"""
from __future__ import annotations

import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from analysis import lines_altered, text_steps  # noqa: E402
from common import CLASSES, CORPUS_DIR, EVAL, OUT, quantile, read_jsonl  # noqa: E402

M4, M5 = "claude-sonnet-4-6", "claude-opus-5-5"
BASELINE = EVAL / "BASELINE.md"


def load(name):
    p = OUT / name
    return json.loads(p.read_text()) if p.exists() else None


def fmt(n, d=0):
    if n is None:
        return "–"
    if isinstance(n, float) and d:
        return f"{n:,.{d}f}"
    return f"{int(round(n)):,}"


def pct(a, b, d=1):
    return "–" if not b else f"{100 * a / b:.{d}f}%"


def mcnemar_p(b: int, c: int) -> float | None:
    """Exact two-sided McNemar (binomial on the discordant pairs)."""
    n = b + c
    if n == 0:
        return None
    from math import comb
    k = min(b, c)
    p = sum(comb(n, i) for i in range(0, k + 1)) / 2 ** n
    return min(1.0, 2 * p)


def table(head, rows):
    out = ["| " + " | ".join(head) + " |", "|" + "|".join("---:" if i else "---" for i in range(len(head))) + "|"]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)


# ── sections ────────────────────────────────────────────────────────────────

def sec_corpus(meta, tok):
    if not tok:
        return "(token pass not run)"
    comp = tok["composition"]
    lines = [f"Window: last {meta.get('window_days')} days · {meta['samples']:,} unique tool results "
             f"({meta['stats'].get('results_seen', 0):,} seen, {meta['stats'].get('results_dup', 0):,} exact duplicates removed) · "
             f"{meta['stats'].get('files_session', 0)} main sessions + {meta['stats'].get('files_subagent', 0)} worker transcripts · "
             f"{meta['bytes'] / 1e6:,.1f} MB."]
    for scope, pick in (("all (sessions + workers)", lambda r: True), ("main sessions only", lambda r: r["source"] == "session"),
                        ("worker transcripts only", lambda r: r["source"] == "subagent")):
        agg = defaultdict(Counter)
        for r in comp:
            if pick(r):
                for k in ("samples", "bytes", f"tok:{M4}", f"tok:{M5}"):
                    agg[r["class"]][k] += r[k]
        tot = Counter()
        for c in agg.values():
            tot.update(c)
        rows = []
        for c in sorted(agg, key=lambda c: -agg[c][f"tok:{M4}"]):
            a = agg[c]
            rows.append([c, fmt(a["samples"]), fmt(a["bytes"]), fmt(a[f"tok:{M4}"]), fmt(a[f"tok:{M5}"]),
                         f"{a['bytes'] / a[f'tok:{M4}']:.2f}", f"{a[f'tok:{M5}'] / a[f'tok:{M4}']:.2f}",
                         pct(a[f"tok:{M4}"], tot[f"tok:{M4}"])])
        rows.append(["**total**", fmt(tot["samples"]), fmt(tot["bytes"]), fmt(tot[f"tok:{M4}"]), fmt(tot[f"tok:{M5}"]),
                     f"{tot['bytes'] / tot[f'tok:{M4}']:.2f}", f"{tot[f'tok:{M5}'] / tot[f'tok:{M4}']:.2f}", "100%"])
        lines.append(f"\n**{scope}**\n")
        lines.append(table(["class", "samples", "bytes", "tokens (4.x)", "tokens (5.x)", "bytes/token (4.x)",
                            "5.x / 4.x", "share of tokens (4.x)"], rows))
    cal = tok.get("calibration", {})
    if cal:
        lines.append("\nBatching check (same seeded samples counted one by one vs packed): " + "; ".join(
            f"{m}: {c['individual']:,} vs {c['batched']:,} ({100 * c['rel_error']:+.2f}%)" for m, c in cal.items()))
    return "\n".join(lines)


def sec_crush_corpus(samples, res, tok, run):
    if not res:
        return "(not run)"
    n = len(res)
    elig = sum(1 for r in res.values() if r["in_bytes"] >= 2048)
    fired = [r for r in res.values() if r["fired"]]
    lines = [f"crush fired on **{len(fired):,} of {n:,}** samples ({pct(len(fired), n, 2)}); "
             f"{elig:,} samples are ≥ 2 KiB (crush's floor), so it fired on {pct(len(fired), elig, 1)} of eligible ones. "
             f"Panics: {run['panics'] if run else '–'}."]
    comp = {}
    for r in tok["composition"]:
        c = comp.setdefault(r["class"], Counter())
        c["bytes"] += r["bytes"]
        c["t4"] += r[f"tok:{M4}"]
        c["t5"] += r[f"tok:{M5}"]
    agg = defaultdict(Counter)
    for k, a in tok["fired_agg"][M4]["by_class_label"].items():
        _, cls, _ = k.split("|")
        agg[cls]["n"] += a["n"]
        agg[cls]["b_saved"] += a["raw_bytes"] - a["crushed_bytes"]
        agg[cls]["t4_saved"] += a["raw_tok"] - a["crushed_tok"]
    for k, a in tok["fired_agg"][M5]["by_class_label"].items():
        _, cls, _ = k.split("|")
        agg[cls]["t5_saved"] += a["raw_tok"] - a["crushed_tok"]
    rows = []
    tot = Counter()
    for cls in sorted(comp, key=lambda c: -comp[c]["t4"]):
        a = agg.get(cls, Counter())
        c = comp[cls]
        rows.append([cls, fmt(a["n"]), f"{fmt(a['b_saved'])} ({pct(a['b_saved'], c['bytes'], 2)})",
                     f"{fmt(a['t4_saved'])} ({pct(a['t4_saved'], c['t4'], 2)})",
                     f"{fmt(a['t5_saved'])} ({pct(a['t5_saved'], c['t5'], 2)})"])
        tot.update({"n": a["n"], "b": a["b_saved"], "t4": a["t4_saved"], "t5": a["t5_saved"],
                    "B": c["bytes"], "T4": c["t4"], "T5": c["t5"]})
    rows.append(["**total**", fmt(tot["n"]), f"{fmt(tot['b'])} ({pct(tot['b'], tot['B'], 2)})",
                 f"{fmt(tot['t4'])} ({pct(tot['t4'], tot['T4'], 2)})", f"{fmt(tot['t5'])} ({pct(tot['t5'], tot['T5'], 2)})"])
    lines.append("\nSavings, as a share of **all** tokens/bytes in that class:\n")
    lines.append(table(["class", "fired", "bytes saved", "tokens saved (4.x)", "tokens saved (5.x)"], rows))
    # where it fired: bytes vs tokens on the fired samples only
    fa = tok["fired_agg"]
    rb = sum(a["raw_bytes"] for a in fa[M4]["by_class_label"].values())
    cb = sum(a["crushed_bytes"] for a in fa[M4]["by_class_label"].values())
    r4 = sum(a["raw_tok"] for a in fa[M4]["by_class_label"].values())
    c4 = sum(a["crushed_tok"] for a in fa[M4]["by_class_label"].values())
    r5 = sum(a["raw_tok"] for a in fa[M5]["by_class_label"].values())
    c5 = sum(a["crushed_tok"] for a in fa[M5]["by_class_label"].values())
    lines.append(f"\nOn the {len(fired)} samples where it fired: bytes −{pct(rb - cb, rb)}, tokens −{pct(r4 - c4, r4)} (4.x) / "
                 f"−{pct(r5 - c5, r5)} (5.x). Samples where crushed output had **more** tokens than raw: "
                 f"{fa[M4]['tokens_grew_while_bytes_shrank']} (4.x), {fa[M5]['tokens_grew_while_bytes_shrank']} (5.x). "
                 f"Median per-sample saving: bytes {quantile(fa[M4]['byte_saving_pct_values'], .5):.1f}%, "
                 f"tokens {quantile(fa[M4]['token_saving_pct_values'], .5):.1f}% (4.x).")
    # transform firing
    steps = Counter()
    steps_tok = defaultdict(Counter)
    for r in fired:
        s = samples[r["id"]]
        for st in text_steps(s["output"], r["output"], r["label"]):
            steps[(r["label"], st)] += 1
            steps_tok[st][r["class"]] += 1
    rows = [[lab, st, fmt(n), ", ".join(f"{c} {k}" for c, k in steps_tok[st].most_common())] for (lab, st), n in steps.most_common()]
    lines.append("\nWhich transforms fired (a text-chain sample can fire several steps):\n")
    lines.append(table(["family", "step", "samples", "by class"], rows))
    return "\n".join(lines)


def sec_fixtures(tf):
    if not tf:
        return "(not run)"
    rows = []
    T = Counter()
    for r in tf["per_fixture"]:
        a4, b4 = r[f"raw_tok:{M4}"], r[f"crushed_tok:{M4}"]
        a5, b5 = r[f"raw_tok:{M5}"], r[f"crushed_tok:{M5}"]
        T.update({"rb": r["raw_bytes"], "cb": r["crushed_bytes"], "a4": a4, "b4": b4, "a5": a5, "b5": b5})
        rows.append([r["name"], r["class"], r["label"] or "–", fmt(r["raw_bytes"]), fmt(a4), f"{r['raw_bytes'] / a4:.2f}" if a4 else "–",
                     pct(r["raw_bytes"] - r["crushed_bytes"], r["raw_bytes"]), pct(a4 - b4, a4), pct(a5 - b5, a5)])
    rows.append(["**total**", "", "", fmt(T["rb"]), fmt(T["a4"]), f"{T['rb'] / T['a4']:.2f}", pct(T["rb"] - T["cb"], T["rb"]),
                 pct(T["a4"] - T["b4"], T["a4"]), pct(T["a5"] - T["b5"], T["a5"])])
    return table(["fixture", "class", "fired", "bytes", "tokens (4.x)", "B/tok", "bytes saved", "tokens saved (4.x)",
                  "tokens saved (5.x)"], rows)


def sec_edit(samples, res, es):
    lines = []
    if res:
        for cls in ("CONTENT", "SEARCH"):
            fired = [r for r in res.values() if r["fired"] and r["class"] == cls]
            total = sum(1 for r in res.values() if r["class"] == cls)
            st = Counter()
            alt = tot = 0
            for r in fired:
                s = samples[r["id"]]
                for x in text_steps(s["output"], r["output"], r["label"]):
                    st[x] += 1
                a, b = lines_altered(s["output"], r["output"])
                alt += a
                tot += b
            lines.append(f"- **{cls}**: crush changed the bytes of **{len(fired)}** of {total:,} samples "
                         f"({pct(len(fired), total, 2)}); steps: {', '.join(f'{k} ×{v}' for k, v in st.most_common()) or 'none'}; "
                         f"{fmt(alt)} of {fmt(tot)} non-empty lines in those samples no longer appear verbatim.")
    if es:
        n = es["edits_with_old_string"]
        rows = [[k, fmt(v), pct(v, n)] for k, v in es["source_class"].items()]
        lines.append(f"\nWhere real `edit` calls got their `old_string` (last {int(es['window_days'])} days, {n:,} edits; "
                     "most recent earlier result containing it):\n")
        lines.append(table(["source class", "edits", "share"], rows))
    return "\n".join(lines) or "(not run)"


def sec_accuracy(acc, fx_records):
    if not acc:
        return "(not run)"
    rows = []
    for k, v in sorted(acc["by_model_set"].items()):
        model, st = k.split("|")
        a = v["acc"]
        p = v["paired"]
        b, c = p.get("raw_only", 0), p.get("crushed_only", 0)
        mp = mcnemar_p(b, c)
        len_ = lambda x: f"{x.get('lenient_pct', '–')}%" if "lenient_pct" in x else "–"
        rows.append([model.replace("claude-", ""), st, fmt(v["samples"]), f"{a['raw']['correct']}/{a['raw']['n']} ({a['raw']['pct']}%)",
                     f"{a['crushed']['correct']}/{a['crushed']['n']} ({a['crushed']['pct']}%)", f"{b} / {c}",
                     "–" if mp is None else f"{mp:.3f}", f"{len_(a['raw'])} → {len_(a['crushed'])}",
                     f"{p.get('rep_disagree', 0)}/{p.get('rep_pairs', 0)}" if p.get("rep_pairs") else "–"])
    lines = [table(["model", "set", "samples", "raw correct (strict)", "crushed correct (strict)",
                    "discordant raw-only / crushed-only", "McNemar p", "lenient raw → crushed",
                    "raw vs raw re-ask disagree"], rows)]
    cls_rows = []
    for k, v in sorted(acc["by_model_set_class"].items()):
        model, st = k.split("|")
        for c, d in sorted(v.items()):
            cls_rows.append([model, st, c, d.get("raw", "–"), d.get("crushed", "–")])
    lines.append("\nBy class:\n")
    lines.append(table(["model", "set", "class", "raw", "crushed"], cls_rows))
    kinds = [[k.split("|")[1], d.get("raw", "–"), d.get("crushed", "–")] for k, d in sorted(acc["by_kind"].items()) if k.startswith("fixtures|")]
    if kinds:
        lines.append("\nFixtures by question kind (both models pooled):\n")
        lines.append(table(["kind", "raw", "crushed"], kinds))
    rk = [[k.split("|")[1], d.get("raw", "–"), d.get("crushed", "–")] for k, d in sorted(acc["by_kind"].items()) if k.startswith("corpus|")]
    if rk:
        lines.append("\nReal sample by question kind (both models pooled):\n")
        lines.append(table(["kind", "raw", "crushed"], rk))
    tot = acc.get("per_model", {})
    lines.append("\nSpend: " + "; ".join(f"{m}: {t['api_requests']} requests, ${t['cost_usd_list'] or 0:.2f} at list price "
                                          f"(in {t['usage'].get('input', 0):,} / cache-write {t['usage'].get('cache_write', 0):,} / "
                                          f"cache-read {t['usage'].get('cache_read', 0):,} / out {t['usage'].get('output', 0):,} tokens)"
                                          for m, t in tot.items())
                 + f". Total {acc['calls_used']} model calls over {len(acc.get('merged_runs') or [acc['run']])} merged run(s) "
                 f"(cap {acc['cap']} per run), ${acc['cost_usd_list_total']:.2f}. "
                 f"Real auth.json unchanged: {acc['real_auth_unchanged']}. Generated questions dropped as unfair "
                 f"(they quote a line that is not verbatim in the crushed text), counted over all fired candidates "
                 f"before sampling: {acc.get('questions_dropped_unfair')}.")
    return "\n".join(lines)


def sec_latency(rc, rf):
    rows = []
    for name, r in (("real corpus", rc), ("fixtures", rf)):
        if not r:
            continue
        ip, e = r["in_process_ns"], r["e2e"]
        rows.append([name, fmt(r["samples"]), f"{ip['p50_all'] / 1e3:.1f} / {ip['p99_all'] / 1e3:.0f} µs",
                     f"{ip['p50_ge2k'] / 1e3:.0f} / {ip['p99_ge2k'] / 1e3:.0f} µs ({fmt(ip['n_ge2k'])})",
                     f"{e['rtt_ns']['p50_ge2k'] / 1e3:.0f} / {e['rtt_ns']['p99_ge2k'] / 1e3:.0f} µs ({fmt(e['rtt_ns']['n_ge2k'])})",
                     fmt(r["panics"]), f"{e['ok']}/{e['requests']}", fmt(e["stderr_panics"]), fmt(e["mismatch_vs_inprocess"])])
    return table(["set", "samples", "in-process p50/p99 (all)", "in-process p50/p99 (≥2 KiB)",
                  "binary JSON-RPC round trip p50/p99 (≥2 KiB)", "panics", "binary responses ok", "pass-through-on-panic",
                  "binary≠library"], rows) if rows else "(not run)"


def sec_classifier(cl):
    if not cl:
        return "(not run)"
    lines = []
    a = cl.get("audit")
    if a:
        lines.append(f"Hand audit, {a['n']} seeded random real samples ≥ 256 B: **{100 * a['accuracy']:.0f}%** agree with my label. "
                     f"Recall by true class: {', '.join(f'{k} {v}' for k, v in a['per_class_recall'].items())}. "
                     f"Precision by predicted class: {', '.join(f'{k} {v}' for k, v in a['per_class_precision'].items())}.")
    cv = cl["cmd_vs_sniff"]
    lines.append(f"\nCommand-derived vs output-sniffed class, where both exist: {cv['agree']:,}/{cv['samples_with_both']:,} agree "
                 f"({cv['agree_pct']}%). Largest disagreements by bytes:\n")
    rows = [[k.split(" vs sniff ")[0], k.split(" vs sniff ")[1], fmt(v["n"]), fmt(v["bytes"])]
            for k, v in list(cv["pairs"].items()) if k.split(" vs sniff ")[0] != k.split(" vs sniff ")[1]][:8]
    lines.append(table(["by command", "by sniffing", "samples", "bytes"], rows))
    d = cl["decided_by_bytes"]
    T = sum(d.values())
    lines.append(f"\nDecided by: tool/command {pct(d.get('command/tool', 0), T)}, sniffing {pct(d.get('sniff', 0), T)}, "
                 f"nothing (UNKNOWN) {pct(d.get('unknown', 0), T)} of bytes. Biggest UNKNOWN families (bytes): "
                 + ", ".join(f"{k} {v / 1e6:.1f} MB" for k, v in list(cl["unknown_bytes_by_family"].items())[:10]) + ".")
    return "\n".join(lines)


def sec_redundancy(rd, tok):
    if not rd:
        return "(not run)"
    agg = defaultdict(Counter)
    for r in rd["rows"]:
        for k, v in r.items():
            if isinstance(v, int):
                agg[r["class"]][k] += v
    rows = []
    for cls, c in sorted(agg.items(), key=lambda kv: -kv[1]["bytes"]):
        rows.append([cls, fmt(c["bytes"]), pct(c["exact_repeat_bytes"], c["bytes"], 2),
                     pct(c["read_line_overlap_bytes"], c["bytes"], 2), pct(c["sed_view_bytes"], c["bytes"]),
                     pct(c["sed_line_overlap_bytes"], c["sed_view_bytes"], 2) if c["sed_view_bytes"] else "–"])
    return (f"Every tool result in each stored transcript (last {int(rd['window_days'])} days; a stored transcript is the "
            "current context window, so this is what a live-context dedupe could hit), counted with repeats:\n\n"
            + table(["class", "bytes (with repeats)", "exact repeat of an earlier result", "`read` lines already shown",
                     "`sed -n A,Bp` views", "…of which lines already shown"], rows))


def replace(md: str, name: str, content: str) -> str:
    a, b = f"<!-- BEGIN:{name} -->", f"<!-- END:{name} -->"
    if a not in md:
        return md + f"\n{a}\n{content}\n{b}\n"
    return re.sub(re.escape(a) + r".*?" + re.escape(b), lambda m: f"{a}\n{content}\n{b}", md, flags=re.S)


def main() -> int:
    meta_p = CORPUS_DIR / "corpus.meta.json"
    meta = json.loads(meta_p.read_text()) if meta_p.exists() else None
    samples, res = {}, {}
    if (CORPUS_DIR / "crush-v0.1.results.jsonl").exists():
        res = {r["id"]: r for r in read_jsonl(CORPUS_DIR / "crush-v0.1.results.jsonl")}
        need = {i for i, r in res.items() if r["fired"]}
        for s in read_jsonl(CORPUS_DIR / "corpus.jsonl"):
            if s["id"] in need:
                samples[s["id"]] = s
    tok_c, tok_f = load("tokens.corpus.json"), load("tokens.fixtures.json")
    secs = {
        "corpus": sec_corpus(meta, tok_c) if meta else "(corpus not extracted)",
        "crush_corpus": sec_crush_corpus(samples, res, tok_c, load("corpus.run.json")) if tok_c else "(not run)",
        "fixtures": sec_fixtures(tok_f),
        "edit": sec_edit(samples, res, load("edit_sources.json")),
        "accuracy": sec_accuracy(load("accuracy.summary.json"), None),
        "latency": sec_latency(load("corpus.run.json"), load("fixtures.run.json")),
        "classifier": sec_classifier(load("classifier.json")),
        "redundancy": sec_redundancy(load("redundancy.json"), tok_c),
    }
    md = BASELINE.read_text() if BASELINE.exists() else "# crush v0.1 baseline\n"
    for k, v in secs.items():
        md = replace(md, k, v)
    BASELINE.write_text(md)
    (OUT / "report.generated.md").write_text("\n\n".join(f"## {k}\n\n{v}" for k, v in secs.items()))
    print(f"updated {BASELINE}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
