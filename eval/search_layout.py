#!/usr/bin/env python3
"""Pick the SEARCH layout by real tokens, not by assumption.

A seeded sample of corpus outputs the SEARCH grouping fires on is rendered in
each candidate layout (driver `--style`) and counted with count_tokens (both
tokenizer families). Aggregates only -> eval/out/search_layout.json.

Layouts: heading (rg --heading, the default), blank (blank line between
groups), colon (`path:` heading), inline (single-hit files stay `path:N:text`).
"""
from __future__ import annotations

import argparse
import json
import random
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import OUT, read_jsonl  # noqa: E402
from run_crush import DRIVER, load_samples, results_path  # noqa: E402
from tokens import TokenCounter  # noqa: E402

STYLES = {"heading": "", "blank": "blank", "colon": "colon", "inline": "inline"}


def render(samples, style):
    inp = "".join(json.dumps({"id": s["id"], "tool": s["tool"], "input": s["input"], "output": s["output"]}) + "\n" for s in samples)
    p = subprocess.run([str(DRIVER), "--style", style or "default"], input=inp, capture_output=True, text=True, check=True)
    return {json.loads(l)["id"]: json.loads(l)["output"] for l in p.stdout.splitlines()}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=250)
    ap.add_argument("--seed", type=int, default=29)
    args = ap.parse_args()
    res = {r["id"]: r for r in read_jsonl(results_path("corpus")) if r["fired"] and "search_group" in r["transforms"]}
    samples = [s for s in load_samples("corpus") if s["id"] in res]
    rng = random.Random(args.seed)
    pick = rng.sample(samples, min(args.n, len(samples)))
    variants = {k: render(pick, v) for k, v in STYLES.items()}
    tc = TokenCounter()
    out = {"samples": len(pick), "of_fired": len(samples), "by_model": {}}
    for m in tc.models:
        raw = tc.count_many([s["output"] for s in pick], m, progress="raw")
        row = {"raw": sum(raw)}
        for k, v in variants.items():
            texts = [v[s["id"]] or s["output"] for s in pick]
            c = tc.count_many(texts, m, progress=k)
            row[k] = sum(c)
            row[k + "_saving_pct"] = round(100 * (1 - sum(c) / sum(raw)), 2)
            row[k + "_grew"] = sum(1 for a, b in zip(raw, c) if b > a)
        out["by_model"][m] = row
    out["token_api"] = tc.stats()
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "search_layout.json").write_text(json.dumps(out, indent=1))
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
