#!/usr/bin/env python3
"""Edit-safety gate (crush 2).

Real edits copy their `old_string` from CONTENT (54%) and from grep output
(14%), so:

1. CONTENT must reach the model byte-identical. Checked twice: by crush-core's
   own class, and by the Phase 0 Python classifier (an independent opinion; a
   sample it calls CONTENT that crush changed is listed, with the lines that
   are no longer verbatim).
2. SEARCH `line:text` must stay byte-exact. Every result the SEARCH grouping
   touched is rebuilt here, independently of crush-core: each original line
   must appear verbatim or as `heading` + `:`/`-` + an output line under that
   heading. Also counted: grep hit lines whose `N:text` part is still a whole
   output line.
3. Lines no longer verbatim in other classes (generic transforms), for the
   record.

Exit status 1 with --strict on any CONTENT change (crush-core's class) or any
SEARCH rebuild failure. Samples only the Phase 0 classifier calls CONTENT are
reported, not failed: on this corpus they are `cat *.log` views that crush-core
deliberately classifies as LOG.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import OUT, read_jsonl, tagged  # noqa: E402
from run_crush import load_samples, results_path  # noqa: E402

HIT = re.compile(r"^([^\s:]+?)([:-])(\d+)([:-])(.*)$")


def lines(s: str):
    t = s.endswith("\n")
    return (s[:-1] if t else s).split("\n"), t


def rebuilds(raw: str, out: str) -> bool:
    """Independent re-implementation of the SEARCH round trip."""
    x, tx = lines(raw)
    y, ty = lines(out)
    if tx != ty:
        return False
    j, head = 0, None

    def under(l, h, rest):
        return len(l) == len(h) + 1 + len(rest) and l.startswith(h) and l[len(h)] in ":-" and l.endswith(rest)
    for l in x:
        if j < len(y) and y[j] == l:
            j += 1
            if l != "--":
                head = None
            continue
        if head is not None and j + 2 < len(y) and y[j] == "" and under(l, y[j + 1], y[j + 2]):
            j += 1  # the blank line crush puts between two touching groups
        if j + 1 < len(y) and under(l, y[j], y[j + 1]):
            head = y[j]
            j += 2
            continue
        if head is not None and j < len(y) and under(l, head, y[j]):
            j += 1
            continue
        return False
    return j == len(y)


def altered(raw: str, out: str) -> tuple[int, int]:
    have = set(out.split("\n"))
    rl = [l for l in raw.split("\n") if l.strip()]
    return sum(1 for l in rl if l not in have), len(rl)


def check(which: str):
    res = {r["id"]: r for r in read_jsonl(results_path(which))}
    rep = {"content_changed": 0, "content_p0_changed": 0, "content_p0_lines_altered": 0, "content_p0_by_transform": Counter(),
           "search_group_samples": 0, "search_rebuild_failures": 0, "hit_lines": 0, "hit_lines_verbatim": 0,
           "other_changed_by_class": Counter(), "other_lines_altered": Counter(), "items": []}
    for s in load_samples(which):
        r = res.get(s["id"])
        if not r or not r["fired"]:
            continue
        raw, out = s["output"], r["output"]
        if r["class"] == "CONTENT":
            rep["content_changed"] += 1
        a, n = altered(raw, out)
        if "search_group" in r["transforms"]:
            rep["search_group_samples"] += 1
            ok = rebuilds(raw, out)
            rep["search_rebuild_failures"] += not ok
            have = set(out.split("\n"))
            for l in raw.split("\n"):
                m = HIT.match(l)
                if m and m.group(2) == ":" and m.group(4) == ":":
                    rep["hit_lines"] += 1
                    rep["hit_lines_verbatim"] += f"{m.group(3)}:{m.group(5)}" in have or l in have
            a = 0 if ok else a  # a rebuilt line is still quotable: heading + line
        if s["class"] == "CONTENT":
            rep["content_p0_changed"] += 1
            rep["content_p0_lines_altered"] += a
            rep["content_p0_by_transform"][r["label"]] += 1
        if r["class"] != "SEARCH":
            rep["other_changed_by_class"][r["class"]] += 1
            rep["other_lines_altered"][r["class"]] += a
        if which == "fixtures":
            rep["items"].append({"id": s["id"], "class": r["class"], "label": r["label"], "lines_altered": a, "lines": n})
    for k in ("content_p0_by_transform", "other_changed_by_class", "other_lines_altered"):
        rep[k] = dict(rep[k])
    return rep


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
    (OUT / tagged("edit_safety.json")).write_text(json.dumps(report, indent=1))
    print(json.dumps({w: {k: v for k, v in r.items() if k != "items"} for w, r in report.items()}, indent=1))
    bad = sum(r["content_changed"] + r["search_rebuild_failures"] for r in report.values())
    if bad:
        print(f"EDIT-SAFETY: {bad} violation(s)", file=sys.stderr)
    return 1 if (args.strict and bad) else 0


if __name__ == "__main__":
    sys.exit(main())
