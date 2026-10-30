#!/usr/bin/env python3
"""Where SEARCH and LISTING tokens actually go (Phase 2 measurement).

Real `count_tokens` (both tokenizer families), on every corpus result of the
class (crush-core's classifier, from the latest `run_crush.py --set corpus`).
Components are attributed by deletion: tokens(full) - tokens(full without the
component), each variant counted over the whole class in packed batches, so
the numbers are class totals, not a sample. Aggregates only ->
eval/out/anatomy.json (private per-sample data never leaves the cache).

SEARCH lines are split into: multi-file hits (`path:N:text`, context
`path-N-text`; path / line-number / text), single-file numbered hits
(`N:text`, `N-text`), `--` separators and other lines (non-hit text in the
same output: file views chained after the grep, unnumbered `grep -A`, ...).
Overlapping subsets: context lines, over-long lines, duplicated lines.
Results are also bucketed by size (the 12 KiB spill threshold) and hit count,
and the "after spill" total is what Synaps #334 would keep in history.

LISTING: tokens by what the command was (ls, wc, ps, find, ...), the `ls -l`
rows inside them, and the part of those rows that is constant across a
listing (links / owner / group), i.e. the upper bound of column factoring.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import OUT, read_jsonl  # noqa: E402
from run_crush import load_samples, results_path  # noqa: E402
from spill import THRESHOLD, after_spill  # noqa: E402
from tokens import TokenCounter  # noqa: E402

M4, M5 = "claude-sonnet-4-6", "claude-opus-5-5"
SEP = "\n"

PATH_RE = r"(?:[^\s:]+:)?[^\s:]{1,300}?"
HIT = re.compile(rf"^({PATH_RE}):(\d+):(.*)$")
NUM = re.compile(r"^(\d+)([:-])(.*)$")
LONG = 500


def split_search(out: str):
    """[(kind, prefix_path, prefix_num, text, line)] for each line."""
    lines = out.split("\n")
    hits = [HIT.match(l) for l in lines]
    paths = {m.group(1) for m in hits if m}
    rows = []
    for l, m in zip(lines, hits):
        if l == "--":
            rows.append(("sep", "", "", "", l))
        elif m:
            rows.append(("hit", m.group(1) + ":", m.group(2) + ":", m.group(3), l))
        else:
            c = None
            for p in paths:
                if l.startswith(p + "-"):
                    mm = re.match(r"^(\d+)-(.*)$", l[len(p) + 1:], re.S)
                    if mm:
                        c = ("ctx", p + "-", mm.group(1) + "-", mm.group(2), l)
                        break
            if c:
                rows.append(c)
                continue
            n = NUM.match(l)
            if n:
                rows.append(("num" if n.group(2) == ":" else "numctx", "", n.group(1) + n.group(2), n.group(3), l))
            else:
                rows.append(("other", "", "", l, l))
    return rows


def variants_search(out: str) -> dict[str, str]:
    rows = split_search(out)
    j = lambda rs: "\n".join(rs)
    seen = Counter()
    dup = []
    for r in rows:
        k = r[4]
        dup.append(len(k.strip()) >= 8 and seen[k] > 0)
        seen[k] += 1
    return {
        "full": out,
        "no_path": j(r[2] + r[3] if r[0] in ("hit", "ctx") else r[4] for r in rows),
        "no_lineno": j(r[1] + r[3] if r[0] in ("hit", "ctx", "num", "numctx") else r[4] for r in rows),
        "no_hit_text": j(r[1] + r[2] if r[0] in ("hit", "ctx", "num", "numctx") else r[4] for r in rows),
        "no_context": j(r[4] for r in rows if r[0] not in ("ctx", "numctx", "sep")),
        "no_sep": j(r[4] for r in rows if r[0] != "sep"),
        "no_other": j(r[4] for r in rows if r[0] != "other"),
        "no_long": j(r[4] for r in rows if len(r[4]) <= LONG),
        "no_dup": j(r[4] for r, d in zip(rows, dup) if not d),
    }


LS = re.compile(r"^([-dlcbps][rwxsStT-]{9}[.+@]?)(\s+)(\d+)(\s+)(\S+)(\s+)(\S+)(\s+)(\S+)(\s+\w{3}\s+\d{1,2}\s+[\d:]{4,5}\s+.+)$")


def ls_constant_cols(out: str) -> tuple[str, str, int]:
    """(ls rows, the rows with columns constant across the listing removed, n rows)."""
    rows = [m for m in (LS.match(l) for l in out.split("\n")) if m]
    if len(rows) < 3:
        return "", "", 0
    const = [len({m.group(g) for m in rows}) == 1 for g in (3, 5, 7)]
    full = "\n".join(m.group(0) for m in rows)
    cut = []
    for m in rows:
        parts = [m.group(1)]
        for g, c in zip((3, 5, 7), const):
            if not c:
                parts.append(m.group(g - 1) + m.group(g))
        parts.append(m.group(8) + m.group(9) + m.group(10))
        cut.append("".join(parts))
    return full, "\n".join(cut), len(rows)


def listing_kind(reason: str) -> str:
    k = re.sub(r"\s+-.*", "", reason or "?")
    k = k.replace("cmd+sniff:", "sniff:")
    return k


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", default=f"{M5},{M4}")
    ap.add_argument("--batch-chars", type=int, default=200_000)
    ap.add_argument("--rps", type=float, default=4.0)
    ap.add_argument("--classes", default="SEARCH,LISTING")
    args = ap.parse_args()
    models = args.models.split(",")
    tc = TokenCounter(models=models, rate=args.rps)
    res = {r["id"]: r for r in read_jsonl(results_path("corpus"))}
    t0 = time.time()
    out: dict = {"models": models, "long_line_chars": LONG, "spill_threshold": THRESHOLD}
    classes = args.classes.split(",")
    samples = [s for s in load_samples("corpus") if res.get(s["id"], {}).get("class") in classes]

    def count(texts, m):
        return tc.count_batched(texts, m, max_chars=args.batch_chars, sep=SEP)[0]

    if "SEARCH" in classes:
        S = [s for s in samples if res[s["id"]]["class"] == "SEARCH"]
        V = [variants_search(s["output"]) for s in S]
        # byte anatomy (every line, exact)
        b = Counter()
        for s in S:
            for kind, p, n, t, l in split_search(s["output"]):
                b["lines:" + kind] += len(l.encode()) + 1
                b["path"] += len(p.encode())
                b["lineno"] += len(n.encode())
                if len(l) > LONG:
                    b["long"] += len(l.encode()) + 1
        b["total"] = sum(len(s["output"].encode()) for s in S)
        sec = {"samples": len(S), "bytes": dict(b), "tokens": {}}
        for m in models:
            full = count([v["full"] for v in V], m)
            row = {"full": full}
            for k in V[0]:
                if k != "full":
                    row["share:" + k[3:]] = round(100 * (full - count([v[k] for v in V], m)) / full, 2)
            # size and hit-count buckets
            big = [s["output"] for s in S if len(s["output"].encode()) > THRESHOLD]
            row["over_spill_threshold"] = {"results": len(big), "tokens": count(big, m),
                                           "tokens_after_spill": count([after_spill(x) for x in big], m)}
            buckets = defaultdict(list)
            for s, v in zip(S, V):
                nh = sum(1 for r in split_search(s["output"]) if r[0] != "other" and r[0] != "sep")
                k = "<20" if nh < 20 else "<50" if nh < 50 else "<100" if nh < 100 else "<200" if nh < 200 else ">=200"
                buckets[k].append(s["output"])
            row["by_hits"] = {k: {"results": len(v), "share": round(100 * count(v, m) / full, 2)} for k, v in sorted(buckets.items())}
            shapes = defaultdict(list)
            for s in S:
                rs = split_search(s["output"])
                kinds = Counter(r[0] for r in rs)
                n = max(1, len(rs))
                sh = ("multi-file" if kinds["hit"] + kinds["ctx"] > 0.5 * n else
                      "single-file" if kinds["num"] + kinds["numctx"] > 0.5 * n else "mixed")
                shapes[sh].append(s["output"])
            row["by_shape"] = {k: {"results": len(v), "share": round(100 * count(v, m) / full, 2)} for k, v in sorted(shapes.items())}
            sec["tokens"][m] = row
            print(f"  SEARCH {m}: {json.dumps(row)}", file=sys.stderr)
        out["SEARCH"] = sec

    if "LISTING" in classes:
        L = [s for s in samples if res[s["id"]]["class"] == "LISTING"]
        sec = {"samples": len(L), "bytes": sum(len(s["output"].encode()) for s in L), "tokens": {}}
        kinds = defaultdict(list)
        for s in L:
            kinds[listing_kind(res[s["id"]]["reason"])].append(s["output"])
        top = sorted(kinds, key=lambda k: -sum(len(x) for x in kinds[k]))[:14]
        lsl = [ls_constant_cols(s["output"]) for s in L]
        lsl = [x for x in lsl if x[2]]
        sec["ls_l"] = {"results": len(lsl), "rows": sum(x[2] for x in lsl)}
        for m in models:
            full = count([s["output"] for s in L], m)
            row = {"full": full,
                   "by_command": {k: {"results": len(kinds[k]), "share": round(100 * count(kinds[k], m) / full, 2)} for k in top}}
            rows_t = count([x[0] for x in lsl], m)
            cut_t = count([x[1] for x in lsl], m)
            row["ls_l_rows_share"] = round(100 * rows_t / full, 2)
            row["ls_l_constant_cols_share_of_rows"] = round(100 * (rows_t - cut_t) / max(1, rows_t), 2)
            big = [s["output"] for s in L if len(s["output"].encode()) > THRESHOLD]
            row["over_spill_threshold"] = {"results": len(big), "tokens": count(big, m),
                                           "tokens_after_spill": count([after_spill(x) for x in big], m)}
            sec["tokens"][m] = row
            print(f"  LISTING {m}: {json.dumps(row)}", file=sys.stderr)
        out["LISTING"] = sec

    out["token_api"] = tc.stats()
    out["elapsed_s"] = round(time.time() - t0, 1)
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "anatomy.json").write_text(json.dumps(out, indent=1))
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
