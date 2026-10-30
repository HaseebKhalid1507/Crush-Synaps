"""Programmatic questions with exact answers derived from the RAW tool output.

gen_questions(sample, max_q) -> [{"q", "answer", "kind", "match"}]

`match` tells the scorer how to compare: int | exact | ci | enum | line.
Every generator parses the raw text itself and returns [] when the shape is
not what it expects, so arbitrary real outputs are safe to feed in.
Questions target exactly what a compressor can distort: which file a hit is
in (prefix folding), field values (dictionary coding / column factoring),
null vs "" (tabular folds), repeat counts (run collapse), line numbers and
adjacent lines (blank-line collapse), pass/fail of a named test (log folding).
"""
from __future__ import annotations

import json
import random
import re
from collections import Counter, OrderedDict

GREP = re.compile(r"^([^\s:]{1,300}):(\d+):(.*)$")
GREP1 = re.compile(r"^(\d+)[:-](.*)$")
LS = re.compile(r"^([-dlcbps][rwxsStT-]{9}[.+@]?)\s+(\d+)\s+(\S+)\s+(\S+)\s+(\S+)\s+(\w{3})\s+(\d{1,2})\s+([\d:]{4,5})\s+(.+)$")
PS_HDR = re.compile(r"^USER\s+PID\s+%CPU\s+%MEM")
CARGO_T = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)\s*$")
CARGO_R = re.compile(r"^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored")
PYTEST_V = re.compile(r"^(\S+::\S+) (PASSED|FAILED|ERROR|SKIPPED)")
PYTEST_S = re.compile(r"=+ (.*?) in [\d.]+s")
NODE_T = re.compile(r"^\s*(✔|✖) (.+?) \([\d.]+m?s\)\s*$")
GCC_W = re.compile(r"^([\w./-]+\.\w+):(\d+):(\d+): (warning|error): (.+?)(?: \[(-W[\w-]+)\])?$")
RUST_LOC = re.compile(r"^\s*--> ([\w./-]+):(\d+):(\d+)")
RUST_DIAG = re.compile(r"^(warning|error)(\[(E\d+)\])?: (.+)$")
GITLOG_C = re.compile(r"^commit ([0-9a-f]{40})")
ONELINE = re.compile(r"^([0-9a-f]{7,12}) (.+)$")


def _rng(sample):
    return random.Random(f"q:{sample.get('id')}")


def _snip(text: str, others: list[str], rng) -> str | None:
    """A short substring of `text` unique among `others` (incl. itself once)."""
    words = re.findall(r"[A-Za-z_][A-Za-z0-9_]{3,}", text)
    rng.shuffle(words)
    for w in words[:12]:
        if sum(1 for o in others if w in o) == 1:
            return w
    t = text.strip()
    for n in (24, 32, 48):
        if len(t) >= n:
            start = rng.randrange(0, len(t) - n + 1)
            s = t[start:start + n].strip()
            if len(s) >= 12 and sum(1 for o in others if s in o) == 1 and "`" not in s:
                return s
    return None


def q(qs, text, answer, kind, match="exact"):
    qs.append({"q": text, "answer": str(answer), "kind": kind, "match": match})


# ── per-shape generators ─────────────────────────────────────────────────────

def gen_grep(out, rng, sample):
    hits = []
    for l in out.split("\n"):
        m = GREP.match(l)
        if m:
            hits.append((m.group(1), int(m.group(2)), m.group(3)))
    if len(hits) < 4:
        return []
    qs = []
    per = Counter(h[0] for h in hits)
    files = [f for f, n in per.items() if 2 <= n <= 25] or list(per)
    f = rng.choice(files)
    q(qs, f"How many matching lines are shown for the file `{f}`?", per[f], "grep:count", "int")
    texts = [h[2] for h in hits]
    for _ in range(3):
        h = rng.choice(hits)
        s = _snip(h[2], texts, rng)
        if s:
            q(qs, f"Which file contains the match whose text includes `{s}`? Give the file's path as grep reported it, "
                  "including every directory component.",
              h[0], "grep:path", "exact")
            q(qs, f"What line number is the match whose text includes `{s}`?", h[1], "grep:line", "int")
            break
    if len(per) > 1:
        (f1, n1), (f2, n2) = per.most_common(2)
        if n1 > n2:
            q(qs, "Which file has the most matching lines? Give the file's path as grep reported it, including every "
                  "directory component.", f1, "grep:most", "exact")
    return qs


def gen_grep_single(out, rng, sample):
    hits = [(int(m.group(1)), m.group(2)) for m in (GREP1.match(l) for l in out.split("\n")) if m]
    lines = [l for l in out.split("\n") if l.strip()]
    if len(hits) < 4 or len(hits) < 0.6 * len(lines):
        return []
    qs = []
    texts = [h[1] for h in hits]
    for h in rng.sample(hits, min(3, len(hits))):
        s = _snip(h[1], texts, rng)
        if s:
            q(qs, f"On which line number is the line that includes `{s}`?", h[0], "numbered:line", "int")
    return qs[:2]


def gen_ls(out, rng, sample):
    rows = [m for m in (LS.match(l) for l in out.split("\n")) if m]
    if len(rows) < 4:
        return []
    names = Counter(m.group(9) for m in rows)
    uniq = [m for m in rows if names[m.group(9)] == 1 and m.group(9) not in (".", "..")]
    if not uniq:
        return []
    qs = []
    a, b, c = rng.sample(uniq, 3) if len(uniq) >= 3 else (uniq * 3)[:3]
    nm = lambda m: m.group(9).split(" -> ")[0]
    q(qs, f"What is the size of `{nm(a)}` exactly as shown in the listing?", a.group(5), "ls:size", "exact")
    q(qs, f"What are the permissions of `{nm(b)}` (the 10-character mode string, e.g. -rw-r--r--)?", b.group(1)[:10], "ls:perms", "exact")
    q(qs, f"What modification date/time is shown for `{nm(c)}`? Answer as shown, e.g. `Jun 3 14:02`.",
      f"{c.group(6)} {c.group(7)} {c.group(8)}", "ls:mtime", "exact")
    owners = Counter(m.group(3) for m in rows)
    q(qs, f"Which user owns `{nm(a)}`?", a.group(3), "ls:owner", "exact")
    links = [m for m in uniq if " -> " in m.group(9)]
    if links:
        m = rng.choice(links)
        src, dst = m.group(9).split(" -> ", 1)
        q(qs, f"`{src}` is a symlink. What is its target exactly as shown?", dst, "ls:symlink", "exact")
    return qs


def gen_ps(out, rng, sample):
    lines = out.split("\n")
    if not lines or not PS_HDR.match(lines[0]):
        return []
    rows = []
    for l in lines[1:]:
        p = l.split(None, 10)
        if len(p) == 11:
            rows.append(p)
    if len(rows) < 5:
        return []
    qs = []
    cmds = [r[10] for r in rows]
    for r in rng.sample(rows, min(6, len(rows))):
        c = r[10]
        if cmds.count(c) == 1 and len(c) < 120 and "`" not in c:
            q(qs, f"What is the PID of the process whose command is `{c}`?", r[1], "ps:pid", "int")
            break
    a, b, c2 = rng.sample(rows, 3)
    q(qs, f"What is the %MEM value of PID {a[1]}?", a[3], "ps:mem", "exact")
    q(qs, f"What is the STAT value of PID {b[1]}?", b[7], "ps:stat", "exact")
    q(qs, f"Which user is running PID {c2[1]}?", c2[0], "ps:user", "exact")
    q(qs, f"What is the RSS value of PID {c2[1]}?", c2[5], "ps:rss", "exact")
    return qs


def gen_pathlist(out, rng, sample):
    lines = [l.strip() for l in out.split("\n") if l.strip()]
    paths = [l for l in lines if re.fullmatch(r"\.{0,2}/?[\w@.+-]+(/[\w@.+-]*)+/?", l)]
    if len(paths) < 6 or len(paths) < 0.7 * len(lines):
        return []
    qs = []
    base = Counter(p.rstrip("/").rsplit("/", 1)[-1] for p in paths)
    uniq = [p for p in paths if base[p.rstrip("/").rsplit("/", 1)[-1]] == 1 and "." in p.rsplit("/", 1)[-1]]
    if uniq:
        p = rng.choice(uniq)
        b = p.rstrip("/").rsplit("/", 1)[-1]
        q(qs, f"Give the path of the entry named `{b}` as the tool reported it, including every directory component.",
          p, "paths:full", "exact")
    p = rng.choice(paths)
    head, _, tail = p.rpartition("/")
    fake = f"{head}/zz_{tail}" if head else f"zz_{tail}"
    if fake not in paths:
        pair = [(p, "yes"), (fake, "no")]
        rng.shuffle(pair)
        for path, ans in pair:
            q(qs, f"Is the exact path `{path}` in the list? Answer yes or no.", ans, "paths:member", "enum")
    return qs


def gen_cargo_test(out, rng, sample):
    tests = [(m.group(1), m.group(2)) for m in (CARGO_T.match(l) for l in out.split("\n")) if m]
    res = [m for m in (CARGO_R.match(l) for l in out.split("\n")) if m]
    if not tests or not res:
        return []
    qs = []
    failed = [t for t, s in tests if s == "FAILED"]
    passed = [t for t, s in tests if s == "ok"]
    picks = ([rng.choice(failed)] if failed else []) + rng.sample(passed, min(2, len(passed)))
    for t in picks:
        q(qs, f"Did the test `{t}` pass or fail? Answer PASS or FAIL.", "FAIL" if t in failed else "PASS", "test:status", "enum")
    q(qs, "In total, how many tests failed?", sum(int(m.group(2)) for m in res), "test:nfail", "int")
    q(qs, "In total, how many tests passed?", sum(int(m.group(1)) for m in res), "test:npass", "int")
    return qs


def gen_pytest(out, rng, sample):
    v = [(m.group(1), m.group(2)) for m in (PYTEST_V.match(l) for l in out.split("\n")) if m]
    summ = [m.group(1) for m in (PYTEST_S.search(l) for l in out.split("\n")) if m]
    if not summ or "passed" not in summ[-1] and "failed" not in summ[-1]:
        return []
    qs = []
    s = summ[-1]
    for word in ("failed", "passed"):
        m = re.search(rf"(\d+) {word}", s)
        q(qs, f"How many tests {word}?", m.group(1) if m else 0, f"pytest:n{word}", "int")
    fails = re.findall(r"^FAILED (\S+::\S+)", out, re.M)
    if v:
        failed = [t for t, st in v if st == "FAILED"]
        passed = [t for t, st in v if st == "PASSED"]
        for t in ([rng.choice(failed)] if failed else []) + rng.sample(passed, min(1, len(passed))):
            q(qs, f"Did `{t}` pass or fail? Answer PASS or FAIL.", "FAIL" if t in failed else "PASS", "test:status", "enum")
    elif fails:
        t = rng.choice(fails)
        q(qs, f"Did `{t}` pass or fail? Answer PASS or FAIL.", "FAIL", "test:status", "enum")
    return qs


def gen_node(out, rng, sample):
    t = [(m.group(1), m.group(2)) for m in (NODE_T.match(l) for l in out.split("\n")) if m]
    if len(t) < 4:
        return []
    qs = []
    names = Counter(n for _, n in t)
    fails = [n for s, n in t if s == "✖" and names[n] == 1]
    oks = [n for s, n in t if s == "✔" and names[n] == 1 and not n.startswith(("cart", "checkout", "catalog")) or (s == "✔" and " handles case " in n)]
    for n in ([rng.choice(fails)] if fails else []) + rng.sample(oks, min(1, len(oks))):
        q(qs, f"Did the test `{n}` pass or fail? Answer PASS or FAIL.", "FAIL" if n in fails else "PASS", "test:status", "enum")
    m = re.search(r"^ℹ fail (\d+)", out, re.M)
    if m:
        q(qs, "How many tests failed?", m.group(1), "test:nfail", "int")
    return qs


def gen_compile(out, rng, sample):
    qs = []
    lines = out.split("\n")
    gcc = [m for m in (GCC_W.match(l) for l in lines) if m]
    if len(gcc) >= 3:
        uniq = Counter((m.group(1), m.group(5)) for m in gcc)
        cand = [m for m in gcc if uniq[(m.group(1), m.group(5))] == 1]
        if cand:
            m = rng.choice(cand)
            q(qs, f"In `{m.group(1)}`, on which line is the {m.group(4)} \"{m.group(5)}\" reported?", m.group(2), "gcc:line", "int")
        flags = Counter(m.group(6) for m in gcc if m.group(6))
        if flags:
            f, n = rng.choice(sorted(flags.items()))
            q(qs, f"How many diagnostics carry the flag `{f}`?", n, "gcc:count", "int")
    # rustc: diag line followed by --> loc
    diags = []
    for i, l in enumerate(lines):
        m = RUST_DIAG.match(l)
        if m and i + 1 < len(lines):
            loc = RUST_LOC.match(lines[i + 1])
            if loc:
                diags.append((m.group(1), m.group(3), m.group(4), loc.group(1), int(loc.group(2))))
    if len(diags) >= 2:
        errs = [d for d in diags if d[0] == "error"]
        if errs:
            d = errs[0]
            q(qs, "What is the file and line of the first error? Answer as path:line.", f"{d[3]}:{d[4]}", "rustc:errloc", "exact")
            if d[1]:
                q(qs, "What is the error code of the first error (e.g. E0000)?", d[1], "rustc:code", "exact")
        msgs = Counter(d[2] for d in diags if d[0] == "warning")
        um = [d for d in diags if d[0] == "warning" and msgs[d[2]] == 1]
        if um:
            d = rng.choice(um)
            q(qs, f"At which line of `{d[3]}` is the warning \"{d[2]}\"?", d[4], "rustc:wline", "int")
    m = re.search(r"generated (\d+) warnings?", out)
    if m:
        q(qs, "How many warnings did the build generate, according to its summary line?", m.group(1), "rustc:nwarn", "int")
    return qs


def gen_gitlog(out, rng, sample):
    lines = out.split("\n")
    commits = []
    for i, l in enumerate(lines):
        m = GITLOG_C.match(l)
        if m and i + 1 < len(lines) and lines[i + 1].startswith("Author: "):
            a = re.match(r"Author: (.+?) <(.+?)>", lines[i + 1])
            subj = next((x.strip() for x in lines[i + 3:i + 8] if x.startswith("    ") and x.strip()), None)
            if a and subj:
                commits.append((m.group(1), a.group(1), a.group(2), subj))
    if len(commits) >= 3:
        qs = []
        c = rng.sample(commits, 3)
        q(qs, f"Who is the author (name only) of commit {c[0][0][:10]}?", c[0][1], "git:author", "exact")
        q(qs, f"What is the first line of the commit message of commit {c[1][0][:10]}?", c[1][3], "git:subject", "line")
        q(qs, f"What is the author's email for commit {c[2][0][:10]}?", c[2][2], "git:email", "exact")
        return qs
    pipe = [l.split("|", 4) for l in lines if l.count("|") >= 4 and re.match(r"^[0-9a-f]{7,40}\|", l)]
    if len(pipe) >= 4:
        qs = []
        c = rng.sample(pipe, 4)
        q(qs, f"Who is the author of commit {c[0][0][:10]}?", c[0][1], "gitpipe:author", "exact")
        q(qs, f"What is the email for commit {c[1][0][:10]}?", c[1][2], "gitpipe:email", "exact")
        q(qs, f"What is the date of commit {c[2][0][:10]}? Answer exactly as shown.", c[2][3], "gitpipe:date", "line")
        q(qs, f"What is the subject of commit {c[3][0][:10]}?", c[3][4], "gitpipe:subject", "line")
        return qs
    one = [m for m in (ONELINE.match(l) for l in lines) if m]
    if len(one) >= 5 and len(one) > 0.6 * len([l for l in lines if l.strip()]):
        qs = []
        subj = Counter(m.group(2) for m in one)
        u = [m for m in one if subj[m.group(2)] == 1]
        for m in rng.sample(u, min(2, len(u))):
            q(qs, f"What is the subject of commit {m.group(1)}?", m.group(2), "oneline:subject", "line")
        return qs
    return []


def gen_json(out, rng, sample):
    s = out.strip()
    try:
        v = json.loads(s)
    except Exception:
        return []
    arrays = []

    def walk(x, path, depth=0):
        if depth > 6:
            return
        if isinstance(x, list) and len(x) >= 3 and all(isinstance(e, dict) for e in x):
            arrays.append((path, x))
        if isinstance(x, dict):
            for k, e in x.items():
                walk(e, f"{path}.{k}" if path else k, depth + 1)
        elif isinstance(x, list):
            for e in x[:50]:
                walk(e, path + "[]", depth + 1)
    walk(v, "")

    def id_key(arr):
        keys = Counter(k for o in arr for k in o)
        return next((k for k in ("id", "number", "name", "key", "sha", "title") if keys.get(k) == len(arr)
                     and len({json.dumps(o[k]) for o in arr}) == len(arr) and not isinstance(arr[0][k], (dict, list))), None)
    # largest array of objects that has a unique scalar id key; arrays nested
    # under another array ("a[].b") are ambiguous to name, so prefer unnested ones
    arrays.sort(key=lambda pa: ("[]" in pa[0], -len(pa[1])))
    pick = next(((p, a, id_key(a)) for p, a in arrays if id_key(a)), None)
    if not pick:
        return []
    path, arr, idk = pick
    where = f"the `{path}` array" if path else "the top-level array"
    qs = []
    scal = lambda o: [k for k, x in o.items() if k != idk and not isinstance(x, (dict, list)) and len(json.dumps(x)) < 80]

    def fmt(x):
        return "null" if x is None else ("true" if x is True else "false" if x is False else (x if isinstance(x, str) else json.dumps(x)))
    objs = rng.sample(arr, min(4, len(arr)))
    for o in objs[:3]:
        ks = scal(o)
        if ks:
            k = rng.choice(ks)
            ans = fmt(o[k])
            q(qs, f"In {where}, what is the value of `{k}` for the object whose `{idk}` is {json.dumps(o[idk])}? "
                  f"Answer with the bare value (use null for null, \"\" for an empty string).",
              '""' if ans == "" else ans, "json:field", "exact")
    # null vs empty string vs missing: the tabular-fold ambiguity
    for o in objs:
        for k, x in o.items():
            if x is None or x == "":
                q(qs, f"In {where}, what is the value of `{k}` for the object whose `{idk}` is {json.dumps(o[idk])}? "
                      f"Answer with the bare value (use null for null, \"\" for an empty string).",
                  "null" if x is None else '""', "json:nullish", "exact")
                break
        else:
            continue
        break
    if len(arr) <= 40:
        q(qs, f"How many objects are in {where}?", len(arr), "json:len", "int")
    return qs


def gen_status(out, rng, sample):
    if "Changes" not in out and "Untracked files" not in out:
        return []
    state, sec = {}, None
    for l in out.split("\n"):
        if l.startswith("Changes to be committed"):
            sec = "staged"
        elif l.startswith("Changes not staged"):
            sec = "modified"
        elif l.startswith("Untracked files"):
            sec = "untracked"
        elif sec and l.startswith("\t"):
            path = l.strip().split(":", 1)[-1].strip()
            state.setdefault(path, sec)
    if len(state) < 2:
        return []
    qs = []
    for p in rng.sample(sorted(state), min(3, len(state))):
        q(qs, f"Is `{p}` staged, modified (not staged), or untracked? Answer staged, modified or untracked.", state[p], "status:state", "enum")
    return qs


def gen_lines(out, rng, sample):
    """Generic: adjacency + repeated-run counts. Used for CONTENT/LOG/UNKNOWN text."""
    lines = out.split("\n")
    qs = []
    runs = []
    i = 0
    while i < len(lines):
        j = i
        while j + 1 < len(lines) and lines[j + 1] == lines[i]:
            j += 1
        if j > i and lines[i].strip() and len(lines[i]) < 160 and "`" not in lines[i]:
            runs.append((lines[i], j - i + 1))
        i = j + 1
    if runs:
        l, n = rng.choice(runs)
        if sum(1 for x, _ in runs if x == l) == 1:
            q(qs, f"How many times in a row does this exact line appear: `{l.strip()}`?", n, "lines:run", "int")
            qs[-1]["anchor"] = l.strip()
    idx = [k for k in range(len(lines) - 2) if lines[k].strip() and len(lines[k].strip()) > 12 and "`" not in lines[k]
           and lines.count(lines[k]) == 1]
    rng.shuffle(idx)
    for k in idx[:20]:
        # the next line, then the line after a blank gap: probes blank-run collapse
        nxt = next((x for x in lines[k + 1:k + 6] if x.strip()), None)
        if nxt is None or "`" in nxt or len(nxt) > 200 or len(re.sub(r"[\W_]", "", nxt)) < 4:
            continue
        gap = next(g for g in range(1, 7) if lines[k + g].strip())
        q(qs, f"Quote the next non-empty line after the line `{lines[k].strip()}`.", nxt.strip(), "lines:next", "line")
        qs[-1]["anchor"] = lines[k].strip()
        if gap > 1:
            q(qs, f"How many empty lines are there between `{lines[k].strip()}` and the next non-empty line?", gap - 1, "lines:gap", "int")
            qs[-1]["anchor"] = lines[k].strip()
        break
    gut = [m for m in (re.match(r"^\s*(\d+)\t(.*)$", l) for l in lines) if m]
    seq = sum(1 for a, b in zip(gut, gut[1:]) if int(b.group(1)) == int(a.group(1)) + 1)
    if len(gut) > 10 and seq >= 0.8 * (len(gut) - 1):
        cand = [m for m in gut if len(m.group(2).strip()) > 10 and "`" not in m.group(2)]
        if cand:
            m = rng.choice(cand)
            q(qs, f"What is the exact text of line {m.group(1)}? (without the line number)", m.group(2).strip(), "content:line", "line")
    return qs


def gen_pr_list(out, rng, sample):
    rows = [l.split("\t") for l in out.split("\n") if l.count("\t") >= 3 and re.match(r"^\d+\t", l)]
    if len(rows) < 4:
        return []
    qs = []
    for r in rng.sample(rows, 2):
        q(qs, f"What is the state of PR #{r[0]}?", r[3], "pr:state", "ci")
    return qs


B64_RUN = re.compile(r"[A-Za-z0-9+/_-]{256,}={0,2}")
B64_LINE = re.compile(r"^\s*[A-Za-z0-9+/]{40,}={0,2}$")


def gen_blob(out, rng, sample):
    """Long base64 values: their first characters (crush's blob marker keeps
    the first 16) and the text around them."""
    qs = []
    lines = out.split("\n")
    for i, l in enumerate(lines):
        m = B64_RUN.search(l)
        if m and re.search(r"[A-Z]", m.group(0)) and re.search(r"[a-z]", m.group(0)) and re.search(r"\d", m.group(0)):
            pre = l[:m.start()].strip()
            if 3 <= len(pre) <= 80 and "`" not in pre and sum(1 for x in lines if x.strip().startswith(pre)) == 1:
                q(qs, f"What are the first 12 characters of the long base64 value on the line that starts with `{pre}`?",
                  m.group(0)[:12], "blob:head", "exact")
                qs[-1]["anchor"] = pre
        if len(qs) >= 2:
            break
    for i in range(1, len(lines) - 3):
        if all(B64_LINE.match(lines[i + k]) for k in range(4)) and not B64_LINE.match(lines[i - 1]):
            prev = lines[i - 1].strip()
            if prev and "`" not in prev and lines.count(lines[i - 1]) == 1:
                q(qs, f"What are the first 12 characters of the base64 block that follows the line `{prev}`?",
                  lines[i].strip()[:12], "blob:head", "exact")
                qs[-1]["anchor"] = prev
                break
    return qs


def overlay(line: str) -> str:
    buf: list[str] = []
    for seg in line.split("\r"):
        for k, ch in enumerate(seg):
            if k < len(buf):
                buf[k] = ch
            else:
                buf.append(ch)
    return "".join(buf)


def gen_progress(out, rng, sample):
    """Lines redrawn with carriage returns: what do they finally show?"""
    qs = []
    lines = out.split("\n")
    cands = [i for i, l in enumerate(lines) if "\r" in l.rstrip("\r") and "\x1b" not in l and i > 0]
    rng.shuffle(cands)
    for i in cands:
        prev = lines[i - 1].strip()
        final = overlay(lines[i].rstrip("\r")).strip()
        if prev and "`" not in prev and lines.count(lines[i - 1]) == 1 and final and len(final) < 200:
            q(qs, f"The line right after `{prev}` is a progress line redrawn several times with carriage returns. "
                  "Quote what it finally shows (its last state).", final, "progress:final", "exact")
            qs[-1]["anchor"] = prev
        if len(qs) >= 2:
            break
    return qs


GENERATORS = [gen_cargo_test, gen_pytest, gen_node, gen_compile, gen_gitlog, gen_status, gen_json, gen_ls, gen_ps,
              gen_pr_list, gen_grep, gen_grep_single, gen_pathlist]
# always asked when they apply: they probe crush 2's generic transforms
ALWAYS = [gen_progress, gen_blob]


def gen_questions(sample: dict, max_q: int = 6) -> list[dict]:
    out = sample["output"]
    rng = _rng(sample)
    qs = []
    for g in GENERATORS:
        try:
            got = g(out, rng, sample)
        except Exception:
            got = []
        if got:
            qs.extend(got)
            break
    for g in ALWAYS:
        try:
            qs.extend(g(out, rng, sample))
        except Exception:
            pass
    try:
        extra = gen_lines(out, rng, sample)
    except Exception:
        extra = []
    # Always add the generic run/adjacency probes when the specific generator
    # left room (they target the text-chain transforms).
    qs.extend(extra[: max(0, max_q - len(qs))] if qs else extra)
    # dedupe and cap
    seen, uniq = set(), []
    for x in qs:
        if x["q"] not in seen:
            seen.add(x["q"])
            uniq.append(x)
    return uniq[:max_q]


# ── scoring ──────────────────────────────────────────────────────────────────

def _norm(s: str) -> str:
    s = s.strip()
    for _ in range(2):
        if len(s) >= 2 and s[0] == s[-1] and s[0] in "`'\"":
            s = s[1:-1].strip()
    s = s.rstrip(".") if not re.search(r"\.\w+$", s) else s
    return re.sub(r"\s+", " ", s)


def score(question: dict, given: str | None) -> bool:
    if given is None:
        return False
    want, got = question["answer"], given.strip()
    m = question["match"]
    if m == "int":
        g = re.search(r"-?\d+", got.replace(",", ""))
        return bool(g) and int(g.group(0)) == int(want)
    if m == "enum" or m == "ci":
        return _norm(got).lower() == _norm(want).lower() or _norm(got).lower().split()[:1] == [_norm(want).lower()]
    if want == '""':
        return got in ('""', "''", '``', "(empty string)", "empty string")
    return _norm(got) == _norm(want)


def fair_for(qs: list[dict], crushed: str) -> tuple[list[dict], int]:
    """Drop questions whose wording quotes an anchor line that no longer exists
    verbatim (modulo whitespace runs) in the crushed text: those test
    re-rendering of the original format, not access to the information."""
    flat = re.sub(r"[ \t]+", " ", crushed)
    keep = [x for x in qs if "anchor" not in x or re.sub(r"[ \t]+", " ", x["anchor"]) in flat]
    return keep, len(qs) - len(keep)
