#!/usr/bin/env python3
"""What crush saves on top of Synaps' spill (#334), in real tokens.

Synaps keeps any tool result over 12 KiB as a head + tail preview (the raw goes
to a spill file). crush runs first, so the model sees:

  raw world:     after_spill(raw)
  crush world:   after_spill(crushed)   (crushed itself when it is <= 12 KiB)

For every result a variant fired on, both are counted with count_tokens (both
tokenizer families) and summed by class and transform, next to the plain raw
vs crushed saving. A crushed result that drops under the threshold can cost
*more* than the preview it replaces: those are counted, not hidden.

Reads the tagged results file of the variant (CRUSH_EVAL_TAG); writes
aggregates to eval/out/after_spill.<set>.<tag>.json.
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import OUT, read_jsonl, tagged  # noqa: E402
from run_crush import load_samples, results_path  # noqa: E402
from spill import THRESHOLD, after_spill  # noqa: E402
from tokens import TokenCounter  # noqa: E402

M4, M5 = "claude-sonnet-4-6", "claude-opus-5-5"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", choices=["corpus", "fixtures"], required=True)
    ap.add_argument("--models", default=f"{M5},{M4}")
    ap.add_argument("--rps", type=float, default=4.0)
    ap.add_argument("--only", default="", help="comma list of transform names to count (default: every fired result)")
    args = ap.parse_args()
    models = args.models.split(",")
    only = set(filter(None, args.only.split(",")))
    tc = TokenCounter(models=models, rate=args.rps)
    res = {r["id"]: r for r in read_jsonl(results_path(args.set))}
    fired = {i: r for i, r in res.items() if r["fired"] and (not only or only & set(r["transforms"]))}
    samples = {s["id"]: s for s in load_samples(args.set) if s["id"] in fired}
    ids = sorted(fired)
    raw = [samples[i]["output"] for i in ids]
    cr = [fired[i]["output"] for i in ids]
    raw_as = [after_spill(t) for t in raw]
    cr_as = [after_spill(t) for t in cr]
    big = lambda t: len(t.encode("utf-8", "surrogatepass")) > THRESHOLD
    out = {"set": args.set, "threshold": THRESHOLD, "models": models, "fired": len(ids), "by_model": {}}
    for m in models:
        n_raw = tc.count_many(raw, m, progress="raw")
        n_cr = tc.count_many(cr, m, progress="crushed")
        n_ras = tc.count_many(raw_as, m, progress="raw-after-spill")
        n_cas = tc.count_many(cr_as, m, progress="crushed-after-spill")
        by = defaultdict(Counter)
        for k, i in enumerate(ids):
            r = fired[i]
            lab = "+".join(r["transforms"])
            rb, cb = big(raw[k]), big(cr[k])
            regime = "both under" if not rb else ("crush brings it under" if not cb else "both over")
            for key in (f"{r['class']}|{lab}", f"{r['class']}|*", "*|*", f"{r['class']}|{lab}|{regime}"):
                a = by[key]
                a["n"] += 1
                a["raw"] += n_raw[k]
                a["crushed"] += n_cr[k]
                a["raw_after_spill"] += n_ras[k]
                a["crushed_after_spill"] += n_cas[k]
                a["grew_after_spill"] += n_cas[k] > n_ras[k]
                a["grew_raw"] += n_cr[k] > n_raw[k]
        out["by_model"][m] = {k: dict(v) for k, v in sorted(by.items())}
        a = by["*|*"]
        print(f"  {m}: fired {a['n']}, raw {a['raw']} -> {a['crushed']} ({a['raw'] - a['crushed']} saved); "
              f"after spill {a['raw_after_spill']} -> {a['crushed_after_spill']} ({a['raw_after_spill'] - a['crushed_after_spill']} saved); "
              f"grew after spill {a['grew_after_spill']}", file=sys.stderr)
    out["token_api"] = tc.stats()
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / tagged(f"after_spill.{args.set}.json")).write_text(json.dumps(out, indent=1))
    print(json.dumps({"set": args.set, "fired": len(ids), "token_api": out["token_api"]}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
