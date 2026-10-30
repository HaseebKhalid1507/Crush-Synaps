#!/usr/bin/env python3
"""Phase 2B layout measurements on the real tokenizer, before building:

- git log (default format): header layouts, on every default-format log in
  the corpus that has no patch, plus the git fixtures.
- git status (long form): dropping git's `(use "git …")` hint lines, with and
  without the raw-pointer marker a lossy result must carry.
- pretty JSON: minified, `, `/`: ` separators, one top-level element per line.

Each candidate is rendered here (independently of crush-core) and counted with
`count_tokens` (both tokenizer families). Writes eval/out/p2b_layouts.json
(aggregates only).
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import FIXTURES, OUT, read_jsonl  # noqa: E402
from run_crush import load_samples, results_path  # noqa: E402
from tokens import TokenCounter  # noqa: E402

MODELS = ["claude-opus-5-5", "claude-sonnet-4-6"]
REF = "/home/user/.synaps-cli/spill/session-1497513-1793340440887-49/toolu_018HPHz1D9QgMpJsQMfa9R3f.txt"
MON = "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split()
COMMIT = re.compile(r"^commit [0-9a-f]{40}( \(.*\))?$")
HINT = re.compile(r'^(  |\t)\((use "git |fix conflicts|all conflicts fixed|commit or discard).*\)$')


def iso(d):
    p = d.split(" ")
    if len(p) != 6 or p[1] not in MON:
        return None
    wd, mon, day, t, y, tz = p
    return f"{y}-{MON.index(mon) + 1:02d}-{int(day):02d} {t} {tz}"


def gitlog(text, style):
    ls, out, i = text.split("\n"), [], 0
    while i < len(ls):
        l = ls[i]
        if COMMIT.match(l) and i + 3 < len(ls):
            k = i + 1
            merge = ls[k] if ls[k].startswith("Merge: ") else None
            k += merge is not None
            if k + 3 < len(ls) and ls[k].startswith("Author: ") and ls[k + 1].startswith("Date:   ") and ls[k + 2] == "" \
                    and ls[k + 3].startswith("    "):
                a, d = ls[k][8:], ls[k + 1][8:]
                m = f" | {merge}" if merge else ""
                out += {"joined, ISO date": [f"{l}{m} | Author: {a} | Date: {iso(d) or d}"],
                        "joined, git date": [f"{l}{m} | Author: {a} | Date: {d}"],
                        "no blank line": ls[i:k + 2],
                        "no labels": [f"{l[7:]}{m} {a} {d}"]}[style]
                i = k + 3
                continue
        out.append(l)
        i += 1
    return "\n".join(out)


def minify(t, comma=",", colon=":"):
    out, i, n = [], 0, len(t)
    while i < n:
        c = t[i]
        if c == '"':
            j = i + 1
            while t[j] != '"':
                j += 2 if t[j] == "\\" else 1
            out.append(t[i:j + 1])
            i = j + 1
        elif c in " \t\r\n":
            i += 1
        else:
            out.append(comma if c == "," else colon if c == ":" else c)
            i += 1
    return "".join(out)


def per_line(t):
    v = json.loads(t)
    dump = lambda x: minify(json.dumps(x, ensure_ascii=False))
    if isinstance(v, list) and v:
        return "[\n" + ",\n".join(dump(e) for e in v) + "\n]"
    if isinstance(v, dict) and v:
        return "{\n" + ",\n".join(dump(k) + ":" + dump(e) for k, e in v.items()) + "\n}"
    return minify(t)


def main() -> int:
    tc = TokenCounter(models=MODELS)
    res = {r["id"]: r for r in read_jsonl(results_path("corpus"))} if results_path("corpus").exists() else {}
    corpus = list(load_samples("corpus")) if res else []
    fx = {f["name"]: (FIXTURES / f["file"]).read_text() for f in json.loads((FIXTURES / "manifest.json").read_text())["fixtures"]}
    sets = {
        "git_log": [fx[n] for n in ("git_log_default", "git_log_stat") if n in fx]
        + [s["output"] for s in corpus if res.get(s["id"], {}).get("class") == "VCS" and "\nAuthor: " in s["output"]
           and re.search(r"^commit [0-9a-f]{40}", s["output"], re.M) and "diff --git" not in s["output"]],
        "git_status": [fx[n] for n in ("git_status_long",) if n in fx]
        + [s["output"] for s in corpus if '(use "git' in s["output"] and ("Changes not staged" in s["output"]
           or "Changes to be committed" in s["output"] or "Untracked files:" in s["output"])],
        "json": [fx[n].strip() for n in ("cargo_metadata_packages_pretty", "gh_issues_jq", "gh_repo_api") if n in fx],
    }
    for s in corpus:
        t = s["output"].strip()
        if res.get(s["id"], {}).get("class") == "STRUCTURED" and t[:1] in "[{" and "\n " in t:
            try:
                json.loads(t)
                sets["json"].append(t)
            except ValueError:
                pass
    variants = {
        "git_log": {k: (lambda k: lambda t: gitlog(t, k))(k) for k in ("joined, ISO date", "joined, git date", "no blank line", "no labels")},
        "git_status": {"hints dropped": lambda t: "\n".join(l for l in t.split("\n") if not HINT.match(l)),
                       "hints dropped + raw pointer": lambda t: "\n".join(l for l in t.split("\n") if not HINT.match(l))
                       + f"\n[crush: git's \"(use \\\"git …\\\")\" hint lines omitted; raw output: {REF}]"},
        "json": {"minified": minify, "`, ` / `: ` separators": lambda t: minify(t, ", ", ": "),
                 "`: ` only": lambda t: minify(t, ",", ": "), "one top-level element per line (shipped layout)": per_line},
    }
    out = {}
    for name, texts in sets.items():
        out[name] = {"samples": len(texts)}
        for m in MODELS:
            raw = tc.count_many(texts, m)
            row = {"raw": sum(raw)}
            for vn, f in variants[name].items():
                v = tc.count_many([f(t) for t in texts], m)
                row[vn] = {"tokens": sum(v), "saved_pct": round(100 * (sum(raw) - sum(v)) / max(1, sum(raw)), 2),
                           "grew": sum(1 for a, b in zip(raw, v) if b > a),
                           "median_saved": sorted(a - b for a, b in zip(raw, v))[len(v) // 2] if v else 0}
            out[name][m] = row
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "p2b_layouts.json").write_text(json.dumps(out, indent=1))
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
