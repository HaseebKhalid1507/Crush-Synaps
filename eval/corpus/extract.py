#!/usr/bin/env python3
"""Extract real tool results from local Synaps sessions into a private corpus.

Sources
  ~/.synaps-cli/sessions/*.json            field `api_messages`
  ~/.synaps-cli/subagent-history/*/sa_*.json field `history` (worker transcripts;
                                            they carry raw tool_use/tool_result)

Each tool_result is paired with its tool_use by id. Identical outputs are
deduplicated (sha256 of the text); the first occurrence keeps its metadata and
`occurrences` counts the rest.

Output: $CRUSH_CORPUS_DIR (default ~/.cache/crush-corpus, mode 0700)/corpus.jsonl
(mode 0600). Never inside the repo. Session ids are salted-hashed.

Usage: extract.py [--since-days 30] [--all] [--no-subagents]
"""
from __future__ import annotations

import argparse
import datetime as dt
import glob
import json
import os
import secrets
import sys
import time
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import CORPUS_DIR, SYNAPS_BASE, assert_not_in_repo, private_dir, sha, write_jsonl_private, write_private  # noqa: E402
from classify import classify_detail  # noqa: E402

MARKERS = {
    "virtualised": ("[Output virtualised", "[Output virtualized"),
    "truncated": ("[truncated", "[output truncated", "bytes truncated"),
    "crushed": ("[@crush/1 ",),
}


def salt() -> bytes:
    p = CORPUS_DIR / "salt"
    if not p.exists():
        write_private(p, secrets.token_bytes(16).hex())
    return p.read_text().strip().encode()


def result_text(content) -> tuple[str, bool]:
    if isinstance(content, str):
        return content, False
    if isinstance(content, list):
        parts, img = [], False
        for b in content:
            if not isinstance(b, dict):
                continue
            if b.get("type") == "text":
                parts.append(b.get("text") or "")
            elif b.get("type") == "image":
                img = True
        return "\n".join(parts), img
    return "", False


def iter_messages_pairs(messages):
    uses = {}
    for m in messages or []:
        c = m.get("content") if isinstance(m, dict) else None
        if not isinstance(c, list):
            continue
        for b in c:
            if not isinstance(b, dict):
                continue
            t = b.get("type")
            if t == "tool_use":
                uses[b.get("id")] = b
            elif t == "tool_result":
                u = uses.get(b.get("tool_use_id"))
                text, img = result_text(b.get("content"))
                yield u, b, text, img


def parse_ts(s: str | None, fallback: float) -> float:
    if s:
        try:
            return dt.datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()
        except Exception:
            pass
    return fallback


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--since-days", type=float, default=30.0)
    ap.add_argument("--all", action="store_true", help="ignore --since-days")
    ap.add_argument("--no-subagents", action="store_true")
    ap.add_argument("--source", default=str(SYNAPS_BASE))
    ap.add_argument("--out", default=str(CORPUS_DIR / "corpus.jsonl"))
    args = ap.parse_args()

    out = Path(args.out)
    assert_not_in_repo(out)
    private_dir(CORPUS_DIR)
    private_dir(out.parent)
    s = salt()
    cutoff = 0.0 if args.all else time.time() - args.since_days * 86400
    src = Path(args.source)

    files = [("session", f) for f in sorted(glob.glob(str(src / "sessions" / "*.json")))]
    if not args.no_subagents:
        files += [("subagent", f) for f in sorted(glob.glob(str(src / "subagent-history" / "*" / "sa_*.json")))]

    seen: dict[str, dict] = {}
    order: list[str] = []
    stats = Counter()
    for kind, f in files:
        mtime = os.path.getmtime(f)
        if mtime < cutoff:
            stats["files_outside_window"] += 1
            continue
        try:
            d = json.load(open(f, encoding="utf-8"))
        except Exception:
            stats["files_unreadable"] += 1
            continue
        if kind == "session":
            msgs = d.get("api_messages") or []
            ts = parse_ts(d.get("updated_at"), mtime)
            sid = d.get("id") or os.path.basename(f)
        else:
            msgs = d.get("history") or []
            ts = mtime
            sid = os.path.basename(os.path.dirname(f)) + "/" + os.path.basename(f)
        if ts < cutoff:
            stats["files_outside_window"] += 1
            continue
        stats[f"files_{kind}"] += 1
        hsid = sha(s + sid.encode(), 12)
        for use, res, text, img in iter_messages_pairs(msgs):
            stats["results_seen"] += 1
            if not text:
                stats["results_empty"] += 1
                continue
            h = sha(text)
            if h in seen:
                seen[h]["occurrences"] += 1
                stats["results_dup"] += 1
                continue
            tool = (use or {}).get("name") or "?"
            tinput = (use or {}).get("input") or {}
            det = classify_detail(tool, tinput, text)
            flags = [k for k, pats in MARKERS.items() if any(p in text for p in pats)]
            rec = {
                "id": h[:20],
                "source": kind,
                "session": hsid,
                "day": dt.datetime.fromtimestamp(ts, dt.timezone.utc).strftime("%Y-%m-%d"),
                "tool": tool,
                "input": tinput,
                "command": tinput.get("command") if isinstance(tinput, dict) and isinstance(tinput.get("command"), str) else None,
                "is_error": bool(res.get("is_error")),
                "has_image": img,
                "flags": flags,
                "bytes": len(text.encode("utf-8", "surrogatepass")),
                "occurrences": 1,
                **{k: det[k] for k in ("class", "reason", "cmd_class", "sniff_class", "sniff_reason", "prog", "sub")},
                "output": text,
            }
            seen[h] = rec
            order.append(h)
    n = write_jsonl_private(out, (seen[h] for h in order))
    by_cls = Counter()
    for h in order:
        by_cls[seen[h]["class"]] += seen[h]["bytes"]
    meta = {
        "created": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "window_days": None if args.all else args.since_days,
        "samples": n,
        "bytes": sum(seen[h]["bytes"] for h in order),
        "stats": dict(stats),
        "bytes_by_class": dict(by_cls),
    }
    write_private(out.with_suffix(".meta.json"), json.dumps(meta, indent=2))
    print(json.dumps(meta, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
