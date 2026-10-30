#!/usr/bin/env python3
"""Count real Claude tokens for raw vs crushed outputs (crush 2).

  --set fixtures : per-sample raw + crushed counts for every fixture.
  --set corpus   : (a) composition: token totals per (source, class) for both
                   classifiers (crush-core's, and Phase 0's for the before/after
                   UNKNOWN share), counted in packed batches; (b) per-sample
                   raw + crushed counts for EVERY sample crush fired on (so
                   "never larger" is checked with the real tokenizer, not just
                   the estimator); (c) estimator error on those real pairs.

Writes aggregate-only JSON to eval/out/tokens.<set>.json. Per-sample counts
stay in the private sqlite cache.
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from common import OUT, quantile, read_jsonl  # noqa: E402
from tokens import TokenCounter  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run_crush import load_samples, results_path  # noqa: E402

M4, M5 = "claude-sonnet-4-6", "claude-opus-5-5"
EST = {M5: "v5", M4: "v4"}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", choices=["corpus", "fixtures"], required=True)
    ap.add_argument("--models", default=f"{M5},{M4}")
    ap.add_argument("--batch-chars", type=int, default=200_000)
    ap.add_argument("--rps", type=float, default=4.0)
    ap.add_argument("--skip-composition", action="store_true")
    args = ap.parse_args()
    models = args.models.split(",")
    tc = TokenCounter(models=models, rate=args.rps)
    samples = {s["id"]: s for s in load_samples(args.set)}
    res = {r["id"]: r for r in read_jsonl(results_path(args.set))}
    t0 = time.time()
    out = {"set": args.set, "models": models, "samples": len(samples)}

    fired = [i for i, r in res.items() if r["fired"]]
    per = {}
    for m in models:
        raw = tc.count_many([samples[i]["output"] for i in fired], m, progress="fired-raw")
        cr = tc.count_many([res[i]["output"] for i in fired], m, progress="fired-crushed")
        per[m] = {i: (a, b) for i, a, b in zip(fired, raw, cr)}

    def rows(m):
        for i in fired:
            r = res[i]
            a, b = per[m][i]
            yield {"id": i, "class": r["class"], "class_p0": r["class_p0"], "source": r["source"], "label": r["label"],
                   "lossless": r["lossless"], "raw_bytes": r["in_bytes"], "crushed_bytes": r["out_bytes"],
                   "raw_tok": a, "crushed_tok": b,
                   "est_in": r[f"est_{EST[m]}_in"], "est_out": r[f"est_{EST[m]}_out"]}

    agg = {}
    for m in models:
        rs = list(rows(m))
        by = defaultdict(lambda: defaultdict(int))
        for r in rs:
            for k in (f"{r['class']}|{r['label']}", f"{r['class']}|*", "*|*"):
                a = by[k]
                a["n"] += 1
                for f in ("raw_bytes", "crushed_bytes", "raw_tok", "crushed_tok", "est_in", "est_out"):
                    a[f] += r[f]
        # estimator vs real on the deltas that matter
        rel = [((r["est_in"] - r["est_out"]) - (r["raw_tok"] - r["crushed_tok"])) / max(1, r["raw_tok"] - r["crushed_tok"])
               for r in rs if r["raw_tok"] - r["crushed_tok"] > 0]
        agg[m] = {
            "n": len(rs), "by": {k: dict(v) for k, v in sorted(by.items())},
            "grew_real_tokens": sum(1 for r in rs if r["crushed_tok"] > r["raw_tok"]),
            "equal_real_tokens": sum(1 for r in rs if r["crushed_tok"] == r["raw_tok"]),
            "grew_bytes": sum(1 for r in rs if r["crushed_bytes"] > r["raw_bytes"]),
            "saving_pct_p10_p50_p90": [round(quantile([100 * (1 - r["crushed_tok"] / r["raw_tok"]) for r in rs if r["raw_tok"]], q), 2)
                                       for q in (.1, .5, .9)] if rs else None,
            "estimated_saving_vs_real": {
                "sum_est": sum(r["est_in"] - r["est_out"] for r in rs), "sum_real": sum(r["raw_tok"] - r["crushed_tok"] for r in rs),
                "median_rel_err": round(quantile([abs(x) for x in rel], .5), 4) if rel else None,
                "p90_rel_err": round(quantile([abs(x) for x in rel], .9), 4) if rel else None},
        }
        if args.set == "fixtures":
            agg[m]["fired_rows"] = [{k: r[k] for k in ("id", "class", "label", "raw_bytes", "crushed_bytes", "raw_tok", "crushed_tok")}
                                    for r in rs]
    out["fired"] = agg

    if args.set == "fixtures":
        allraw = {m: dict(zip(samples, tc.count_many([s["output"] for s in samples.values()], m))) for m in models}
        out["per_fixture"] = []
        for i, s in samples.items():
            r = res[i]
            row = {"name": i, "class": r["class"], "class_p0": s["class"], "fired": r["fired"], "label": r["label"],
                   "raw_bytes": r["in_bytes"], "crushed_bytes": r["out_bytes"]}
            for m in models:
                row[f"raw_tok:{m}"] = allraw[m][i]
                row[f"crushed_tok:{m}"] = per[m][i][1] if r["fired"] else allraw[m][i]
            out["per_fixture"].append(row)
    elif not args.skip_composition:
        for key, cls_of in (("composition", lambda i: res[i]["class"]), ("composition_p0", lambda i: samples[i]["class"])):
            groups = defaultdict(list)
            for i, s in samples.items():
                groups[(s["source"], cls_of(i))].append(s["output"])
            comp = []
            for (src, cls), texts in sorted(groups.items()):
                row = {"source": src, "class": cls, "samples": len(texts),
                       "bytes": sum(len(t.encode("utf-8", "surrogatepass")) for t in texts)}
                for m in models:
                    tok, nreq = tc.count_batched(texts, m, max_chars=args.batch_chars)
                    row[f"tok:{m}"] = tok
                    row[f"requests:{m}"] = nreq
                comp.append(row)
                print(f"  {key} {src:8} {cls:10} {row['bytes']:>11} B  " + "  ".join(f"{m}={row[f'tok:{m}']}" for m in models),
                      file=sys.stderr)
            out[key] = comp
    out["token_api"] = tc.stats()
    out["elapsed_s"] = round(time.time() - t0, 1)
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / f"tokens.{args.set}.json").write_text(json.dumps(out, indent=1))
    print(json.dumps({"set": args.set, "fired": len(fired), "token_api": out["token_api"], "elapsed_s": out["elapsed_s"],
                      "grew": {m: agg[m]["grew_real_tokens"] for m in models}}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
