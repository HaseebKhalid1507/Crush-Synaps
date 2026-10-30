#!/usr/bin/env python3
"""Count real tokens (count_tokens, both tokenizer families) for a seeded,
stratified slice of the private corpus, one request per text. This is the
calibration/validation set for crush-core's local token estimator.

Strata: class x size bucket (<1 KiB, 1-4 KiB, 4-16 KiB, >=16 KiB), up to
--per samples each. Sample ids go to $CRUSH_CORPUS_DIR/estimator-ids.json
(private); counts live only in the sqlite token cache (hash -> int).
"""
from __future__ import annotations

import argparse
import json
import random
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from common import CORPUS_DIR, assert_not_in_repo, read_jsonl, write_private  # noqa: E402
from tokens import TokenCounter  # noqa: E402

BUCKETS = (1024, 4096, 16384, 1 << 62)


def bucket(n: int) -> int:
    return next(i for i, b in enumerate(BUCKETS) if n < b)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--per", type=int, default=100)
    ap.add_argument("--seed", type=int, default=23)
    ap.add_argument("--models", default="claude-opus-5-5,claude-sonnet-4-6")
    ap.add_argument("--max-bytes", type=int, default=65536)
    args = ap.parse_args()
    strata = defaultdict(list)
    for r in read_jsonl(CORPUS_DIR / "corpus.jsonl"):
        if 0 < r["bytes"] <= args.max_bytes:
            strata[(r["class"], bucket(r["bytes"]))].append(r["id"])
    rng = random.Random(args.seed)
    ids = []
    for k in sorted(strata):
        v = sorted(strata[k])
        ids += rng.sample(v, min(args.per, len(v)))
    out = CORPUS_DIR / "estimator-ids.json"
    assert_not_in_repo(out)
    write_private(out, json.dumps(ids))
    want = set(ids)
    texts = {r["id"]: r["output"] for r in read_jsonl(CORPUS_DIR / "corpus.jsonl") if r["id"] in want}
    tc = TokenCounter(models=args.models.split(","))
    for m in tc.models:
        tc.count_many([texts[i] for i in ids], m, progress="estimator")
    print(json.dumps({"samples": len(ids), "token_api": tc.stats()}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
