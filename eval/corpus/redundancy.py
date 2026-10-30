#!/usr/bin/env python3
"""How much tool-result mass is a repeat of something already in the same
transcript? This bounds the CONTENT "dedupe stub" lever (the only change the
plan allows on CONTENT).

Per transcript (one session file or one worker transcript), in order:
  - exact repeat: the identical output text appeared earlier in this transcript;
  - read overlap: for the native `read` tool, lines (path, line number, text)
    already shown earlier in this transcript, whether by `read` or by a
    `read`-style gutter;
  - command repeat: same bash command string, different output (file changed
    or non-deterministic command), for context.

Aggregate counts only -> eval/out/redundancy.json.
"""
from __future__ import annotations

import glob
import json
import os
import re
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import OUT, SYNAPS_BASE  # noqa: E402
from classify import classify_detail  # noqa: E402
from edit_sources import walk  # noqa: E402

GUT = re.compile(r"^\s*(\d+)\t(.*)$")
# `[cd DIR &&|;] sed -n 'A,Bp' FILE` with nothing else: lines A.. of FILE
SED = re.compile(r"^(?:cd\s+(\S+)\s*(?:&&|;)\s*)?sed -n ['\"]?(\d+),(\d+)p['\"]?\s+(\S+)\s*$")


def main() -> int:
    days = float(sys.argv[1]) if len(sys.argv) > 1 else 30.0
    cutoff = time.time() - days * 86400
    files = sorted(glob.glob(str(SYNAPS_BASE / "sessions" / "*.json"))) + \
        sorted(glob.glob(str(SYNAPS_BASE / "subagent-history" / "*" / "sa_*.json")))
    tot = defaultdict(Counter)
    for f in files:
        if os.path.getmtime(f) < cutoff:
            continue
        try:
            d = json.load(open(f, encoding="utf-8"))
        except Exception:
            continue
        src = "session" if "/sessions/" in f else "subagent"
        seen_text = set()
        seen_lines = set()
        seen_cmd = {}
        for ev in walk(d.get("api_messages") or d.get("history") or []):
            if ev[0] != "result":
                continue
            _, tool, inp, text = ev
            if not text:
                continue
            cls = classify_detail(tool, inp, text)["class"]
            b = len(text.encode("utf-8", "surrogatepass"))
            t = tot[(src, cls)]
            t["bytes"] += b
            t["results"] += 1
            if text in seen_text:
                t["exact_repeat_bytes"] += b
                t["exact_repeat_results"] += 1
            else:
                seen_text.add(text)
                if tool == "read" and isinstance(inp, dict):
                    path = inp.get("path", "")
                    rep = 0
                    for line in text.split("\n"):
                        m = GUT.match(line)
                        if m:
                            k = (path, m.group(1), m.group(2))
                            if k in seen_lines:
                                rep += len(line.encode("utf-8", "surrogatepass")) + 1
                            seen_lines.add(k)
                    t["read_line_overlap_bytes"] += rep
                cmd = inp.get("command") if isinstance(inp, dict) else None
                m = SED.match(cmd.strip()) if isinstance(cmd, str) else None
                if m:
                    path = os.path.join(m.group(1) or "", m.group(4))
                    a = int(m.group(2))
                    rep = 0
                    t["sed_view_bytes"] += b
                    for i, line in enumerate(text.split("\n")[: int(m.group(3)) - a + 1]):
                        k = (path, str(a + i), line)
                        if k in seen_lines:
                            rep += len(line.encode("utf-8", "surrogatepass")) + 1
                        seen_lines.add(k)
                    t["sed_line_overlap_bytes"] += rep
                if isinstance(cmd, str):
                    if cmd in seen_cmd and seen_cmd[cmd] != text:
                        t["same_command_new_output_bytes"] += b
                    seen_cmd[cmd] = text
    rows = []
    for (src, cls), c in sorted(tot.items()):
        rows.append({"source": src, "class": cls, **c})
    res = {"window_days": days, "rows": rows}
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "redundancy.json").write_text(json.dumps(res, indent=1))
    agg = defaultdict(Counter)
    for r in rows:
        for k, v in r.items():
            if isinstance(v, int):
                agg[r["class"]][k] += v
    for cls, c in sorted(agg.items(), key=lambda kv: -kv[1]["bytes"]):
        print(f"{cls:10} bytes {c['bytes']:>12,}  exact-repeat {100 * c['exact_repeat_bytes'] / c['bytes']:5.1f}%  "
              f"read-line-overlap {100 * c['read_line_overlap_bytes'] / c['bytes']:5.1f}%  "
              f"sed-n views {100 * c['sed_view_bytes'] / c['bytes']:5.1f}% of which overlap {100 * c['sed_line_overlap_bytes'] / max(1, c['sed_view_bytes']):5.1f}%  "
              f"same-cmd-new-output {100 * c['same_command_new_output_bytes'] / c['bytes']:5.1f}%")
    return 0


if __name__ == "__main__":
    sys.exit(main())
