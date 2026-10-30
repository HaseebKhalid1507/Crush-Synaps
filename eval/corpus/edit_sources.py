#!/usr/bin/env python3
"""Where do `edit` old_strings come from? (edit-safety scope, real data)

For every `edit` tool call in the window, find the most recent earlier tool
result in the same transcript that contains the old_string (exactly, or line
by line after stripping a `read`-style `N<TAB>` gutter) and record that
result's class. This measures which output classes the model actually quotes
back byte-for-byte, i.e. which classes a compressor must never alter.

Writes aggregate counts to eval/out/edit_sources.json. No text leaves the box.
"""
from __future__ import annotations

import glob
import json
import os
import re
import sys
import time
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import OUT, SYNAPS_BASE  # noqa: E402
from classify import classify_detail  # noqa: E402
from extract import iter_messages_pairs  # noqa: E402

GUTTER = re.compile(r"^\s*\d+\t", re.M)


def walk(messages):
    """Yield events in order: ('result', tool, input, text) and ('edit', input)."""
    uses = {}
    for m in messages or []:
        c = m.get("content") if isinstance(m, dict) else None
        if not isinstance(c, list):
            continue
        for b in c:
            if not isinstance(b, dict):
                continue
            if b.get("type") == "tool_use":
                uses[b.get("id")] = b
                if b.get("name") == "edit" and isinstance(b.get("input"), dict):
                    yield ("edit", b["input"])
            elif b.get("type") == "tool_result":
                u = uses.get(b.get("tool_use_id")) or {}
                cont = b.get("content")
                text = cont if isinstance(cont, str) else "\n".join(
                    x.get("text", "") for x in (cont or []) if isinstance(x, dict) and x.get("type") == "text")
                yield ("result", u.get("name") or "?", u.get("input") or {}, text)


def main() -> int:
    days = float(sys.argv[1]) if len(sys.argv) > 1 else 30.0
    cutoff = time.time() - days * 86400
    files = sorted(glob.glob(str(SYNAPS_BASE / "sessions" / "*.json"))) + \
        sorted(glob.glob(str(SYNAPS_BASE / "subagent-history" / "*" / "sa_*.json")))
    src = Counter()
    how = Counter()
    edits = 0
    for f in files:
        if os.path.getmtime(f) < cutoff:
            continue
        try:
            d = json.load(open(f, encoding="utf-8"))
        except Exception:
            continue
        msgs = d.get("api_messages") or d.get("history") or []
        prior = []  # (tool, input, text, deguttered)
        for ev in walk(msgs):
            if ev[0] == "result":
                _, tool, inp, text = ev
                prior.append((tool, inp, text, None))
                if len(prior) > 400:
                    prior.pop(0)
                continue
            old = ev[1].get("old_string")
            if not isinstance(old, str) or len(old.strip()) < 8:
                continue
            edits += 1
            found = None
            for i in range(len(prior) - 1, -1, -1):
                tool, inp, text, deg = prior[i]
                if old in text:
                    found = (tool, inp, text, "verbatim")
                    break
                if deg is None:
                    deg = GUTTER.sub("", text) if GUTTER.search(text) else ""
                    prior[i] = (tool, inp, text, deg)
                if deg and old in deg:
                    found = (tool, inp, text, "after-gutter-strip")
                    break
            if not found:
                src["(not in any earlier result)"] += 1
                continue
            det = classify_detail(found[0], found[1], found[2])
            fam = found[0] if found[0] not in ("bash", "shell", "shell_send") else f"bash:{det['prog'] or '?'}"
            src[det["class"]] += 1
            how[found[3]] += 1
            how[f"family:{fam}"] += 1
    res = {"window_days": days, "edits_with_old_string": edits, "source_class": dict(src.most_common()),
           "match_kind_and_family": dict(how.most_common(40))}
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "edit_sources.json").write_text(json.dumps(res, indent=1))
    print(json.dumps(res, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
