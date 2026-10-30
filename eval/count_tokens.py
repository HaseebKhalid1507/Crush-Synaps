#!/usr/bin/env python3
"""Count real Claude tokens for raw vs crushed outputs.

  --set fixtures : per-sample raw + crushed counts for every fixture.
  --set corpus   : (a) composition: token totals per (source, class), counted
                   in packed batches (all ~60K samples, few hundred requests);
                   (b) per-sample raw + crushed counts for every sample crush
                   fired on; (c) a calibration slice counted both ways to
                   measure the batching error.

Writes aggregate-only JSON to eval/out/tokens.<set>.json. Per-sample counts
stay in the private sqlite cache.
"""
from __future__ import annotations

import argparse
import json
import random
import sys
import time
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from common import OUT, read_jsonl  # noqa: E402
from tokens import TokenCounter  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run_crush import load_samples, results_path  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", choices=["corpus", "fixtures"], required=True)
    ap.add_argument("--models", default="claude-sonnet-4-6,claude-opus-5-5")
    ap.add_argument("--calibration", type=int, default=150)
    ap.add_argument("--batch-chars", type=int, default=200_000)
    ap.add_argument("--rps", type=float, default=4.0)
    ap.add_argument("--seed", type=int, default=11)
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

    def fired_rows(m):
        rows = []
        for i in fired:
            s, r = samples[i], res[i]
            a, b = per[m][i]
            rows.append({"id": i, "class": s["class"], "source": s["source"], "label": r["label"],
                         "raw_bytes": r["in_bytes"], "crushed_bytes": r["out_bytes"], "raw_tok": a, "crushed_tok": b})
        return rows

    if args.set == "fixtures":
        allraw = {m: dict(zip(samples, tc.count_many([s["output"] for s in samples.values()], m))) for m in models}
        out["per_fixture"] = []
        for i, s in samples.items():
            r = res[i]
            row = {"name": i, "class": s["class"], "fired": r["fired"], "label": r["label"],
                   "raw_bytes": r["in_bytes"], "crushed_bytes": r["out_bytes"]}
            for m in models:
                row[f"raw_tok:{m}"] = allraw[m][i]
                row[f"crushed_tok:{m}"] = per[m][i][1] if r["fired"] else allraw[m][i]
            out["per_fixture"].append(row)
    else:
        # (a) composition by (source, class), batched
        groups = defaultdict(list)
        for s in samples.values():
            groups[(s["source"], s["class"])].append(s["output"])
        comp = []
        for (src, cls), texts in sorted(groups.items()):
            row = {"source": src, "class": cls, "samples": len(texts),
                   "bytes": sum(len(t.encode("utf-8", "surrogatepass")) for t in texts)}
            for m in models:
                tok, nreq = tc.count_batched(texts, m, max_chars=args.batch_chars)
                row[f"tok:{m}"] = tok
                row[f"requests:{m}"] = nreq
            comp.append(row)
            print(f"  composition {src:8} {cls:10} {row['bytes']:>11} B  " +
                  "  ".join(f"{m}={row[f'tok:{m}']}" for m in models), file=sys.stderr)
        out["composition"] = comp
        # (c) calibration: individual vs batched on a seeded random slice
        rng = random.Random(args.seed)
        ids = rng.sample(sorted(samples), min(args.calibration, len(samples)))
        cal = {}
        for m in models:
            indiv = sum(tc.count_many([samples[i]["output"] for i in ids], m, progress="calibration"))
            batched, nreq = tc.count_batched([samples[i]["output"] for i in ids], m, max_chars=args.batch_chars)
            cal[m] = {"samples": len(ids), "individual": indiv, "batched": batched, "requests": nreq,
                      "rel_error": (batched - indiv) / indiv if indiv else None}
        out["calibration"] = cal
    out["fired"] = {m: fired_rows(m) for m in models} if args.set == "fixtures" else None
    # fired per-sample rows only as aggregates for the corpus
    if args.set == "corpus":
        agg = {}
        for m in models:
            rows = fired_rows(m)
            agg[m] = {
                "n": len(rows),
                "by_class_label": {},
                "tokens_grew_while_bytes_shrank": sum(1 for r in rows if r["crushed_tok"] > r["raw_tok"]),
                "token_saving_pct_values": sorted(round(100 * (1 - r["crushed_tok"] / r["raw_tok"]), 2) for r in rows if r["raw_tok"]),
                "byte_saving_pct_values": sorted(round(100 * (1 - r["crushed_bytes"] / r["raw_bytes"]), 2) for r in rows),
            }
            for r in rows:
                k = f"{r['source']}|{r['class']}|{r['label']}"
                a = agg[m]["by_class_label"].setdefault(k, {"n": 0, "raw_bytes": 0, "crushed_bytes": 0, "raw_tok": 0, "crushed_tok": 0})
                a["n"] += 1
                for f in ("raw_bytes", "crushed_bytes", "raw_tok", "crushed_tok"):
                    a[f] += r[f]
        out["fired_agg"] = agg
    out["token_api"] = tc.stats()
    out["elapsed_s"] = round(time.time() - t0, 1)
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / f"tokens.{args.set}.json").write_text(json.dumps(out, indent=1))
    print(json.dumps({k: v for k, v in out.items() if k in ("set", "samples", "calibration", "token_api", "elapsed_s")}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
