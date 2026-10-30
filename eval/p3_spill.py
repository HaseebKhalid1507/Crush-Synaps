#!/usr/bin/env python3
"""Phase 3: what the extension saves after Synaps' spill step, with the shim's
spill rule, and whether test failures survive it.

Every fired result (fixtures, corpus) goes through the real crush-ext binary
as Synaps would call it (spill rule on, default threshold), and three history
views are counted in real tokens (both families):

- raw:  after_spill(raw), i.e. crush not installed;
- A:    after_spill(crushed), a hook that always replaces (2B's policy A);
- shim: after_spill(what crush-ext answered): the crushed text when it
        replaced, the raw when it passed through.

Also: the shim's pass-through reasons on fired results, results costlier than
raw after spill under A and under the shim, and test-failure lines visible
after spill (raw vs shim). Writes eval/out/p3_spill[.<tag>].json (aggregates;
fixture rows by name). CRUSH_EVAL_TAG / CRUSH_EVAL_CONFIG pick the variant, as
for run_crush.py.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import EVAL_CONFIG, OUT, read_jsonl, tagged  # noqa: E402
from run_crush import EXT, frame, load_samples, read_frame, results_path  # noqa: E402
from spill import after_spill  # noqa: E402
from tokens import TokenCounter  # noqa: E402

M4, M5 = "claude-sonnet-4-6", "claude-opus-5-5"
FAIL = re.compile(r"FAILED|panicked at|✕|✖|--- FAIL|^FAIL\b|Traceback \(most recent|^error(\[E\d+\])?:|assertion|AssertionError")


def fail_lines(t: str) -> list[str]:
    return [l for l in t.split("\n") if FAIL.search(l) and not l.startswith("[crush:")]


def visible(raw: str, view: str) -> int:
    """The raw's failure lines whose text the view shows (as spill_report.py
    counts them: a run written once by `repeats` still shows its text)."""
    seen = set(view.split("\n"))
    return sum(1 for l in fail_lines(raw) if l in seen)


def shim_answers(samples):
    state = tempfile.mkdtemp(prefix="crush-p3spill-")
    cfg = ["--config", EVAL_CONFIG] if EVAL_CONFIG else []
    p = subprocess.Popen([str(EXT), "--state-dir", state, *cfg], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, env={"PATH": os.environ.get("PATH", ""), "HOME": state})
    p.stdin.write(frame({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}))
    p.stdin.flush()
    read_frame(p.stdout)
    out = {}
    for i, s in enumerate(samples, start=1):
        p.stdin.write(frame({"jsonrpc": "2.0", "id": i, "method": "hook.handle",
                             "params": {"kind": "after_tool_call", "tool_name": s["tool"], "tool_runtime_name": s["tool"],
                                        "tool_input": s["input"], "tool_output": s["output"], "session_id": "p3"}}))
        p.stdin.flush()
        r = (read_frame(p.stdout) or {}).get("result") or {}
        out[s["id"]] = r.get("output") if r.get("action") == "replace" else None
    p.stdin.write(frame({"jsonrpc": "2.0", "id": 10**9, "method": "shutdown"}))
    p.stdin.flush()
    p.wait(timeout=30)
    st = json.loads((Path(state) / "stats.json").read_text())
    shutil.rmtree(state, ignore_errors=True)
    return out, st["total"]["passed"]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", choices=["corpus", "fixtures"], required=True)
    args = ap.parse_args()
    samples = {s["id"]: s for s in load_samples(args.set)}
    res = [r for r in read_jsonl(results_path(args.set)) if r["fired"]]
    fired = [samples[r["id"]] for r in res]
    answers, passed = shim_answers(fired)
    tc = TokenCounter()
    rows = []
    for r in res:
        s = samples[r["id"]]
        raw, crushed = s["output"], r["output"]
        shim = answers[r["id"]] if answers[r["id"]] is not None else raw
        views = {"raw": after_spill(raw), "A": after_spill(crushed), "shim": after_spill(shim)}
        row = {"id": r["id"], "class": r["class"], "label": "+".join(r["transforms"]), "replaced": answers[r["id"]] is not None,
               "raw_bytes": len(raw.encode()), "crushed_bytes": len(crushed.encode())}
        for m in (M5, M4):
            for k, v in views.items():
                row[f"{k}_{m}"] = tc.count(v, m)
        if r["class"] == "LOG" and fail_lines(raw):
            row["fail_lines"] = len(fail_lines(raw))
            row["fail_visible_raw"] = visible(raw, views["raw"])
            row["fail_visible_shim"] = visible(raw, views["shim"])
        rows.append(row)
    agg = defaultdict(Counter)
    for row in rows:
        for key in (row["label"], "all"):
            a = agg[key]
            a["fired"] += 1
            a["replaced"] += row["replaced"]
            for m, tag in ((M5, "5"), (M4, "4")):
                a[f"raw_{tag}"] += row[f"raw_{m}"]
                a[f"A_{tag}"] += row[f"A_{m}"]
                a[f"shim_{tag}"] += row[f"shim_{m}"]
                a[f"A_costlier_{tag}"] += row[f"A_{m}"] > row[f"raw_{m}"]
                a[f"shim_costlier_{tag}"] += row[f"shim_{m}"] > row[f"raw_{m}"]
            if "fail_lines" in row:
                a["fail_results"] += 1
                a["fail_lines"] += row["fail_lines"]
                a["fail_visible_raw"] += row["fail_visible_raw"]
                a["fail_visible_shim"] += row["fail_visible_shim"]
    # results the shim made costlier after spill (real tokens): sizes and
    # excess only, never ids or text (real data)
    costlier = [{"class": r["class"], "label": r["label"], "raw_bytes": r["raw_bytes"], "crushed_bytes": r["crushed_bytes"],
                 "excess_5": r[f"shim_{M5}"] - r[f"raw_{M5}"], "excess_4": r[f"shim_{M4}"] - r[f"raw_{M4}"],
                 "fail_visible_raw": r.get("fail_visible_raw"), "fail_visible_shim": r.get("fail_visible_shim")}
                for r in rows if r[f"shim_{M5}"] > r[f"raw_{M5}"] or r[f"shim_{M4}"] > r[f"raw_{M4}"]]
    out = {"set": args.set, "config": EVAL_CONFIG or "default", "by_label": {k: dict(v) for k, v in agg.items()},
           "shim_passed_on_fired": passed, "shim_costlier": costlier}
    if args.set == "fixtures":
        out["rows"] = [{k: v for k, v in r.items()} for r in rows]
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / tagged(f"p3_spill.{args.set}.json")).write_text(json.dumps(out, indent=1))
    print(json.dumps(out["by_label"]["all"] if "all" in out["by_label"] else {}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
