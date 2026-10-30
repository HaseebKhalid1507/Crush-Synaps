#!/usr/bin/env python3
"""Savings after Synaps' spill step, and whether test failures survive it.

SynapsCLI #334 keeps any tool result over 12 KiB in a spill file and puts a
head (4 KiB) + tail (3 KiB) preview in history (`lib/spill.py` ports the
Rust). crush runs first, so what a crushed result costs after spill is the
preview of the *crushed* text when that is still over the threshold, else the
crushed text itself. The raw side is the preview of the raw.

For every fired result (fixtures and corpus) this counts real tokens of
after_spill(raw) and after_spill(crushed), per class and transform, next to
the raw-token numbers from count_tokens.py. Two host policies:

- A, "spill on crushed size" (compress, then spill if still > 12 KiB): a
  result crushed from just over to just under the threshold lands in history
  whole and can cost more than the raw's 7 KiB preview would have;
- B, "spill on raw size" (spill if the raw > 12 KiB, preview the crushed
  text, keep the raw in the spill file): never more than the preview budget,
  and the preview is denser. Recommended for Phase 3. For the class denominators it
also counts the preview of every corpus result over the threshold (the rest
cost what they cost raw), so "% of tool-result tokens after spill" uses the
same yardstick on both sides.

Failure survival (LOG results): failure lines (FAILED, panicked, ✕, --- FAIL,
Traceback, `error[`, assertion messages) visible to the model after spill,
raw vs crushed, overall and on the results that spill.

Writes eval/out/spill.<set>.json (aggregates; fixture rows by name).
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import OUT, read_jsonl, tagged  # noqa: E402
from run_crush import load_samples, results_path  # noqa: E402
from spill import THRESHOLD, after_spill, preview  # noqa: E402
from tokens import TokenCounter  # noqa: E402

M4, M5 = "claude-sonnet-4-6", "claude-opus-5-5"
FAIL = re.compile(r"FAILED|panicked at|✕|✖|--- FAIL|^FAIL\b|Traceback \(most recent|^error(\[E\d+\])?:|assertion|AssertionError")


def blen(s: str) -> int:
    return len(s.encode("utf-8", "surrogatepass"))


def after_spill_b(raw: str, crushed: str) -> str:
    """Policy B: spill decided on the raw's size, preview of the crushed."""
    if blen(raw) <= THRESHOLD:
        return crushed
    return preview(crushed) or crushed


def failure_lines(text: str) -> list[str]:
    return [l for l in text.split("\n") if FAIL.search(l) and not l.startswith("[crush:")]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", choices=["corpus", "fixtures"], required=True)
    ap.add_argument("--models", default=f"{M5},{M4}")
    ap.add_argument("--rps", type=float, default=4.0)
    ap.add_argument("--skip-denominators", action="store_true")
    args = ap.parse_args()
    models = args.models.split(",")
    tc = TokenCounter(models=models, rate=args.rps)
    samples = {s["id"]: s for s in load_samples(args.set)}
    res = {r["id"]: r for r in read_jsonl(results_path(args.set))}
    fired = [i for i, r in res.items() if r["fired"]]
    raw_as = {i: after_spill(samples[i]["output"]) for i in fired}
    cr_as = {i: after_spill(res[i]["output"]) for i in fired}
    cr_b = {i: after_spill_b(samples[i]["output"], res[i]["output"]) for i in fired}
    out = {"set": args.set, "models": models, "threshold_bytes": THRESHOLD, "fired": len(fired)}
    per = {}
    for m in models:
        a = tc.count_many([samples[i]["output"] for i in fired], m, progress="raw")
        b = tc.count_many([res[i]["output"] for i in fired], m, progress="crushed")
        c = tc.count_many([raw_as[i] for i in fired], m, progress="raw-after-spill")
        d = tc.count_many([cr_as[i] for i in fired], m, progress="crushed-after-spill")
        e = tc.count_many([cr_b[i] for i in fired], m, progress="crushed-after-spill-b")
        per[m] = {i: (w, x, y, z, v) for i, w, x, y, z, v in zip(fired, a, b, c, d, e)}
    agg = {}
    for m in models:
        by = defaultdict(lambda: defaultdict(int))
        for i in fired:
            r = res[i]
            w, x, y, z, v = per[m][i]
            for k in (f"{r['class']}|{r['label']}", f"{r['class']}|*", "*|*"):
                g = by[k]
                g["n"] += 1
                g["raw"] += w
                g["crushed"] += x
                g["raw_after_spill"] += y
                g["crushed_after_spill"] += z
                g["raw_spilled"] += blen(samples[i]["output"]) > THRESHOLD
                g["crushed_spilled"] += blen(res[i]["output"]) > THRESHOLD
                g["grew_after_spill"] += z > y
                g["crushed_after_spill_b"] += v
                g["grew_after_spill_b"] += v > y
        agg[m] = {k: dict(x) for k, x in sorted(by.items())}
    out["fired_by"] = agg
    # failure survival on LOG results that fired
    surv = defaultdict(int)
    rows = []
    for i in fired:
        r = res[i]
        if r["class"] != "LOG":
            continue
        fl = failure_lines(samples[i]["output"])
        if not fl:
            continue
        vis_raw = sum(1 for l in fl if l in set(raw_as[i].split("\n")))
        vis_cr = sum(1 for l in fl if l in set(cr_as[i].split("\n")))
        vis_b = sum(1 for l in fl if l in set(cr_b[i].split("\n")))
        big = blen(samples[i]["output"]) > THRESHOLD
        for k in ("all", "spilled") if big else ("all",):
            surv[f"{k}:results"] += 1
            surv[f"{k}:failure_lines"] += len(fl)
            surv[f"{k}:visible_raw_after_spill"] += vis_raw
            surv[f"{k}:visible_crushed_after_spill"] += vis_cr
            surv[f"{k}:visible_crushed_after_spill_b"] += vis_b
            surv[f"{k}:results_losing_failures_raw"] += vis_raw < len(fl)
            surv[f"{k}:results_losing_failures_crushed"] += vis_cr < len(fl)
            surv[f"{k}:crushed_no_longer_spills"] += big and blen(res[i]["output"]) <= THRESHOLD
        if args.set == "fixtures":
            rows.append({"id": i, "raw_bytes": blen(samples[i]["output"]), "crushed_bytes": blen(res[i]["output"]),
                         "failure_lines": len(fl), "visible_raw_after_spill": vis_raw, "visible_crushed_after_spill": vis_cr,
                         "visible_crushed_after_spill_b": vis_b,
                         **{f"raw_after_spill_tok:{m}": per[m][i][2] for m in models},
                         **{f"crushed_after_spill_tok:{m}": per[m][i][3] for m in models}})
    out["failure_survival"] = dict(surv)
    if rows:
        out["failure_rows"] = rows
    # denominators: every result's after-spill cost, per class (the previews
    # of the big ones are counted; everything else costs its raw count, which
    # count_tokens.py has per class in its composition table)
    if args.set == "corpus" and not args.skip_denominators:
        big = [i for i, s in samples.items() if blen(s["output"]) > THRESHOLD]
        den = {}
        for m in models:
            raw_big = tc.count_many([samples[i]["output"] for i in big], m, progress="big-raw")
            prev = tc.count_many([after_spill(samples[i]["output"]) for i in big], m, progress="big-preview")
            by = defaultdict(lambda: [0, 0, 0])
            for i, a, b in zip(big, raw_big, prev):
                c = by[res[i]["class"] if i in res else "?"]
                c[0] += 1
                c[1] += a
                c[2] += b
            den[m] = {k: {"n": v[0], "raw": v[1], "preview": v[2]} for k, v in sorted(by.items())}
        out["spilled_results"] = den
    out["token_api"] = tc.stats()
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / tagged(f"spill.{args.set}.json")).write_text(json.dumps(out, indent=1))
    print(json.dumps({"set": args.set, "fired": len(fired), "failure_survival": out["failure_survival"],
                      "all": {m: agg[m].get("*|*") for m in models}}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
