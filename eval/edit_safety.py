#!/usr/bin/env python3
"""Edit-safety gate: CONTENT-class output must reach the model byte-identical.

The model copies file text back as an edit's `old_string`; any byte change
(trailing-whitespace strip, blank-line collapse, run collapse, prefix fold,
padding normalisation, a header line) can make that edit fail or, worse,
match the wrong place. Reads the run_crush results and reports every
CONTENT-class sample whose output changed. SEARCH is reported alongside
because real edits quote grep hits too (see corpus/edit_sources.py).

Exit status 1 with --strict if any CONTENT sample changed.
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from analysis import lines_altered, text_steps  # noqa: E402
from common import OUT, read_jsonl  # noqa: E402
from run_crush import load_samples, results_path  # noqa: E402


def check(which: str):
    res = {r["id"]: r for r in read_jsonl(results_path(which))}
    out = {}
    for cls in ("CONTENT", "SEARCH"):
        viol = []
        for s in load_samples(which):
            r = res.get(s["id"])
            if not r or s["class"] != cls or not r["fired"]:
                continue
            a, b = lines_altered(s["output"], r["output"])
            viol.append({"id": s["id"] if which == "fixtures" else None, "steps": text_steps(s["output"], r["output"], r["label"]),
                         "lines_altered": a, "lines": b})
        out[cls] = {"samples_changed": len(viol), "lines_altered": sum(v["lines_altered"] for v in viol),
                    "steps": dict(Counter(st for v in viol for st in v["steps"])),
                    "items": [v for v in viol if v["id"]]}
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--sets", default="fixtures,corpus")
    ap.add_argument("--strict", action="store_true")
    args = ap.parse_args()
    report = {}
    for w in args.sets.split(","):
        if results_path(w).exists():
            report[w] = check(w)
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "edit_safety.json").write_text(json.dumps(report, indent=1))
    print(json.dumps(report, indent=1))
    bad = sum(r["CONTENT"]["samples_changed"] for r in report.values())
    return 1 if (args.strict and bad) else 0


if __name__ == "__main__":
    sys.exit(main())
