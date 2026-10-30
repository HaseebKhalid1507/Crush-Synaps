#!/usr/bin/env python3
"""How good is the first-draft classifier? Aggregate-only report.

1. agreement: command-derived class vs output-sniffed class where both exist
   (counts and bytes). Disagreement is our cheap confusion proxy.
2. what lands in UNKNOWN, by tool / program family (common programs only;
   anything else is bucketed as "(other)" so private tool names don't leak).
3. a hand audit: `--audit-sheet N` writes N seeded random samples (>=256 B)
   to a private sheet; label them by editing the `label:` lines, then
   `--audit-score` scores the classifier against the labels.
"""
from __future__ import annotations

import argparse
import json
import random
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import CLASSES, CORPUS_DIR, OUT, read_jsonl, write_private  # noqa: E402
from classify import CONTENT_PROGS, LISTING_PROGS, LOG_PROGS, SEARCH_PROGS, STRUCTURED_PROGS  # noqa: E402

COMMON = CONTENT_PROGS | SEARCH_PROGS | LISTING_PROGS | LOG_PROGS | STRUCTURED_PROGS | {
    "git", "gh", "python", "python3", "node", "bash", "sh", "echo", "printf", "curl", "for", "while", "if", "sed",
    "awk", "jq", "docker", "kubectl", "sqlite3", "date", "env", "printenv", "ssh", "rsync", "tar", "unzip", "sort",
    "uniq", "cut", "tr", "xargs", "synaps", "synaps-dev", "uname", "id", "whoami", "hostname", "nvidia-smi", "ping",
    "dig", "nslookup", "openssl", "md5sum", "sha256sum", "cmp", "test", "[", "true", "file", "perl", "ruby"}
SHEET = CORPUS_DIR / "classifier-audit.txt"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--audit-sheet", type=int, default=0)
    ap.add_argument("--audit-score", action="store_true")
    ap.add_argument("--seed", type=int, default=5)
    ap.add_argument("--classes-from", choices=["p0", "crush2"], default="p0",
                    help="p0: the Phase 0 Python classifier stored in the corpus; crush2: crush-core's (from run_crush results)")
    args = ap.parse_args()
    rows = list(read_jsonl(CORPUS_DIR / "corpus.jsonl"))
    if args.classes_from == "crush2":
        res = {r["id"]: r for r in read_jsonl(CORPUS_DIR / "crush2.results.jsonl")}
        for r in rows:
            x = res[r["id"]]
            r["class_p0"], r["class"], r["reason"] = r["class"], x["class"], x["reason"]

    if args.audit_sheet:
        rng = random.Random(args.seed)
        pool = [r for r in rows if r["bytes"] >= 256]
        pick = rng.sample(pool, args.audit_sheet)
        out = []
        for r in pick:
            head = "\n".join("    | " + l[:110] for l in r["output"].split("\n")[:6])
            cmd = (r.get("command") or json.dumps(r["input"]))[:160].replace("\n", "⏎")
            out.append(f"id: {r['id']}\ntool: {r['tool']}\ncmd: {cmd}\nclassifier: {r['class']} ({r['reason']})\n"
                       f"label: {r['class']}\n{head}\n")
        write_private(SHEET, "\n".join(out))
        print(f"wrote {SHEET} ({len(pick)} samples); edit the `label:` lines then run --audit-score")
        return 0

    res = {}
    if args.audit_score:
        lab = {}
        cur = None
        for line in SHEET.read_text().split("\n"):
            if line.startswith("id: "):
                cur = line[4:].strip()
            elif line.startswith("label: ") and cur:
                lab[cur] = line[7:].strip()
        by = {r["id"]: r for r in rows}
        conf = defaultdict(Counter)
        prose = 0
        for i, l in lab.items():
            c = by[i]["class"]
            if c == "PROSE":
                # the audit's label set predates PROSE (documents/messages);
                # it is a split of UNKNOWN, so it is scored as UNKNOWN
                prose += 1
                c = "UNKNOWN"
            conf[l][c] += 1
        n = sum(sum(c.values()) for c in conf.values())
        ok = sum(conf[c][c] for c in conf)
        res["audit"] = {"n": n, "accuracy": round(ok / n, 3) if n else None, "classes_from": args.classes_from,
                        "predicted_prose_scored_as_unknown": prose,
                        "confusion_true_by_predicted": {t: dict(c) for t, c in sorted(conf.items())},
                        "per_class_recall": {t: f"{c[t]}/{sum(c.values())}" for t, c in sorted(conf.items())},
                        "per_class_precision": {p: f"{conf[p][p]}/{sum(conf[t][p] for t in conf)}" for p in CLASSES
                                                if sum(conf[t][p] for t in conf)}}

    byc = defaultdict(lambda: [0, 0])
    for r in rows:
        byc[r["class"]][0] += 1
        byc[r["class"]][1] += r["bytes"]
    res["class_bytes"] = {c: {"samples": v[0], "bytes": v[1]} for c, v in sorted(byc.items())}
    agree = defaultdict(lambda: [0, 0])
    for r in rows:
        if args.classes_from == "crush2":
            break
        if r["cmd_class"] and r["sniff_class"]:
            agree[f"{r['cmd_class']} vs sniff {r['sniff_class']}"][0] += 1
            agree[f"{r['cmd_class']} vs sniff {r['sniff_class']}"][1] += r["bytes"]
    both = sum(v[0] for v in agree.values())
    same = sum(v[0] for k, v in agree.items() if k.split(" vs sniff ")[0] == k.split(" vs sniff ")[1])
    res["cmd_vs_sniff"] = {"samples_with_both": both, "agree": same,
                           "agree_pct": round(100 * same / both, 1) if both else None,
                           "pairs": {k: {"n": v[0], "bytes": v[1]} for k, v in sorted(agree.items(), key=lambda kv: -kv[1][1])}}
    decided = Counter()
    for r in rows:
        if args.classes_from == "crush2":
            decided["unknown" if r["class"] == "UNKNOWN" else "sniff" if r["reason"].startswith("sniff:") else "command/tool"] += r["bytes"]
        else:
            decided["command/tool" if r["cmd_class"] else ("sniff" if r["sniff_class"] else "unknown")] += r["bytes"]
    res["decided_by_bytes"] = dict(decided)
    unk = Counter()
    for r in rows:
        if r["class"] != "UNKNOWN":
            continue
        if r["tool"] in ("bash", "shell", "shell_send", "shell_start"):
            p = r.get("prog") or "?"
            unk[f"bash:{p if p in COMMON else '(other)'}"] += r["bytes"]
        else:
            t = r["tool"]
            unk[t if not t.startswith(("mcp__", "ext__")) and "_" not in t.split("_", 1)[0] else t] += r["bytes"]
    res["unknown_bytes_by_family"] = dict(unk.most_common(25))
    OUT.mkdir(parents=True, exist_ok=True)
    name = "classifier.json" if args.classes_from == "p0" else "classifier.crush2.json"
    (OUT / name).write_text(json.dumps(res, indent=1))
    print(json.dumps({k: res[k] for k in res if k != "cmd_vs_sniff"}, indent=1))
    print(json.dumps({k: res["cmd_vs_sniff"][k] for k in ("samples_with_both", "agree", "agree_pct")}))
    for k, v in list(res["cmd_vs_sniff"]["pairs"].items())[:14]:
        print(f"  {k:36} n={v['n']:6} bytes={v['bytes']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
