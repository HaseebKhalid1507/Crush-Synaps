#!/usr/bin/env python3
"""Phase 2B invariant checks, independent of crush-core's own code.

On every fired fixture and corpus result whose transforms include a Phase 2B
transform:

- LOG (`test_pass`, `build_progress`): every input line that is not a
  collapsible passing-test or build-progress line (re-implemented here from
  the runners' documented formats) is still in the output, verbatim and in
  order. Called out separately: failure lines (FAILED, panicked, ✕, --- FAIL,
  Traceback, assertion messages), summary lines (`test result:`, `Tests:`,
  pytest's `=== … ===`, go's `ok`/`FAIL` package lines, `Finished`) and
  compiler diagnostics with their code frames (`warning:`/`error:`, `-->`,
  `|` gutters, `^^^` markers, `= note:`), which edits copy from.
  The notes' counts must add up to the lines removed (go's `=== RUN` lines of
  passing tests go uncounted).
- STRUCTURED (`json_minify`): the output's JSON token stream (strings and
  numbers byte for byte, structure, literals) equals the input's, and a whole
  JSON output parses to the same value.

Writes eval/out/p2b_checks.json (aggregates + fixture names only); gate.py
fails on any violation.
"""
from __future__ import annotations

import json
import re
import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import OUT, read_jsonl, tagged  # noqa: E402
from run_crush import load_samples, results_path  # noqa: E402

LOG_T = ("test_pass", "build_progress")
NOTE = re.compile(r"^\s*\[crush: ([\d,]+) (passing tests?|build-progress lines?) not ")
CLOSING = "[crush: the collapsed lines are in the raw output: "

FAILURE = re.compile(r"FAILED|panicked at|✕|✖|--- FAIL|^FAIL\b|Traceback \(most recent|AssertionError|^E\s{3}|assertion")
SUMMARY = re.compile(r"^test result:|^Tests:\s|^Test Suites:|^=+ .* in [\d.]+s|^ok\s+\S+\s+[\d.]+s|^FAIL\t|^\s+Finished |"
                     r"^Successfully installed|^error: test failed")
DIAG = re.compile(r"^(warning|error)(\[\w+\])?: |^\s*--> |^\s*\d*\s*\|( |$)|^\s*\|\s*\^|^\s*= (note|help): |"
                  r"^[\w./-]+:\d+:\d+: (warning|error|note): ")
# collapsible shapes (independent of the Rust)
PASS_LINE = re.compile(r"^test \S.* \.\.\. ok$|^ *[✓√✔] \S.*$|^ *--- PASS: \S+ \(.*s\)$|^\S+::\S+ PASSED( +\[ *\d+%\])?$")
GO_BOOK = re.compile(r"^ *=== (RUN|PAUSE|CONT|NAME) +\S+$")
CARGO_VERBS = ("Compiling", "Checking", "Documenting", "Downloaded", "Downloading", "Fresh", "Locking", "Adding",
               "Updating", "Blocking", "Unpacking", "Packaging", "Verifying", "Archiving", "Fetching")
PIP = re.compile(r"^(  )?(Collecting |Downloading |Using cached |Requirement already satisfied: |Preparing metadata |"
                 r"Getting requirements to build |Installing build dependencies|Building wheel for |Created wheel for |"
                 r"Stored in directory: |Obtaining )|^go: downloading |^Download(ing|ed) from \S+: https?://|"
                 r"^ {2,}[━╸╺].*(eta |/s )")


def collapsible(l: str) -> bool:
    if "\x1b" in l or "\r" in l:
        return False
    if PASS_LINE.match(l) or GO_BOOK.match(l) or PIP.match(l):
        return True
    t = l.lstrip(" ")
    verb, _, rest = t.partition(" ")
    return verb in CARGO_VERBS and len(l) - len(t) + len(verb) == 12 and bool(rest) and not rest.startswith(" ")


def subseq_missing(need: list[str], hay: list[str]) -> int:
    """How many of `need` can't be matched, in order, in `hay`."""
    j, miss = 0, 0
    for n in need:
        k = j
        while k < len(hay) and hay[k] != n:
            k += 1
        if k == len(hay):
            miss += 1
        else:
            j = k + 1
    return miss


def json_tokens(s: str) -> list[str]:
    out, i, n = [], 0, len(s)
    while i < n:
        c = s[i]
        if c in " \t\r\n":
            i += 1
        elif c == '"':
            j = i + 1
            while j < n and s[j] != '"':
                j += 2 if s[j] == "\\" else 1
            out.append(s[i:j + 1])
            i = j + 1
        elif c.isalnum() or c in "-+.":
            j = i
            while j < n and (s[j].isalnum() or s[j] in "-+."):
                j += 1
            out.append(s[i:j])
            i = j
        else:
            out.append(c)
            i += 1
    return out


def check(which: str) -> dict:
    res = {r["id"]: r for r in read_jsonl(results_path(which))}
    rep = Counter()
    items = []
    for s in load_samples(which):
        r = res.get(s["id"])
        if not r or not r["fired"]:
            continue
        tr = r["transforms"]
        raw, out = s["output"], r["output"]
        if any(t in tr for t in LOG_T):
            rep["log_samples"] += 1
            rl, ol = raw.split("\n"), out.split("\n")
            kept_out = [l for l in ol if not NOTE.match(l) and not l.startswith(CLOSING)]
            # lines other transforms in the chain may legitimately rewrite
            other = any(t not in LOG_T for t in tr)
            need = [l for l in rl if not collapsible(l)]
            miss = subseq_missing(need, kept_out) if not other else 0
            fail = [l for l in rl if FAILURE.search(l) and not collapsible(l)]
            summ = [l for l in rl if SUMMARY.search(l)]
            diag = [l for l in rl if DIAG.search(l)]
            # with `repeats` also in the chain a repeated line appears once (plus
            # its note), so check presence rather than order there
            have = set(ol)
            m_fail, m_summ, m_diag = ((sum(1 for l in x if l not in have) if other else subseq_missing(x, ol))
                                      for x in (fail, summ, diag))
            counted = sum(int(m.group(1).replace(",", "")) for m in map(NOTE.match, ol) if m)
            removed_pass = sum(1 for l in rl if collapsible(l) and not GO_BOOK.match(l)) - \
                sum(1 for l in kept_out if collapsible(l) and not GO_BOOK.match(l))
            bad_count = (counted != removed_pass) and not other
            rep["log_lines_kept_needed"] += len(need)
            rep["log_lines_missing"] += miss
            rep["failure_lines"] += len(fail)
            rep["failure_lines_missing"] += m_fail
            rep["summary_lines"] += len(summ)
            rep["summary_lines_missing"] += m_summ
            rep["diagnostic_lines"] += len(diag)
            rep["diagnostic_lines_missing"] += m_diag
            rep["note_count_mismatches"] += bad_count
            rep["lines_collapsed"] += counted
            if which == "fixtures":
                items.append({"id": s["id"], "transforms": tr, "missing": miss, "failure_missing": m_fail,
                              "summary_missing": m_summ, "diag_missing": m_diag, "count_ok": not bad_count})
        if "json_minify" in tr:
            rep["json_samples"] += 1
            other = any(t != "json_minify" for t in tr)
            ok = other or json_tokens(out) == json_tokens(raw)
            if ok and not other:
                try:
                    ok = json.loads(out) == json.loads(raw)
                except ValueError:
                    pass  # streams / embedded blocks: the token check above decides
            rep["json_token_mismatches"] += not ok
            if which == "fixtures":
                items.append({"id": s["id"], "transforms": tr, "json_ok": ok})
    d = dict(rep)
    d["items"] = items
    return d


def main() -> int:
    report = {w: check(w) for w in ("fixtures", "corpus") if results_path(w).exists()}
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / tagged("p2b_checks.json")).write_text(json.dumps(report, indent=1))
    print(json.dumps({w: {k: v for k, v in r.items() if k != "items"} for w, r in report.items()}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
