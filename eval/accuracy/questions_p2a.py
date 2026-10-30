"""Phase 2A question generators (SEARCH over-long lines, ls -l columns).

Answers come from the RAW output and the command only (never from what crush
did). Registered in questions.py: `gen_grep_long` and `gen_ls_columns` are
asked whenever they apply, next to the regular grep / ls generators.

- gen_grep_long: hit lines over 1 KiB. The grep pattern is located with
  Python's `re` (BRE/ERE/-F/-i translated), independently of crush's literal
  search. Questions: the file and the line number of the match quoted by a
  snippet next to it, and the words right after the matched text.
- gen_ls_columns: ls -l rows. The group and the hard-link count of an entry,
  and the owner of an entry whose owner differs from most rows (the `..` row
  of a home listing), i.e. the values the owner/group note could blur.

Questions carry `needle` (the quoted text): run_accuracy marks a question
whose needle is not in the crushed text as `cut:<kind>` (its answer was moved
to the raw file), which the Phase 2A report shows separately.
"""
from __future__ import annotations

import re
import shlex
from collections import Counter

LONG = 1024
HIT = re.compile(r"^((?:[^\s:]+:)?[^\s:]{1,300}?):(\d+):(.*)$")
NUM = re.compile(r"^(\d+):(.*)$")
LS = re.compile(r"^([-dlcbps][rwxsStT-]{9}[.+@]?)\s+(\d+)\s+(\S+)\s+(\S+)\s+(\S+)\s+(.+)$")
DATE = re.compile(r"^(?:\w{3}\s+\d{1,2}\s+[\d:]{4,5}|\d{4}-\d{2}-\d{2} [\d:.]+(?: [+-]\d{4})?|\d{2}-\d{2} \d{2}:\d{2})\s+(.+)$")
GREPS = {"grep", "egrep", "fgrep", "rg", "zgrep"}
VALUE_SHORT = set("ABCmefdD")
VALUE_SHORT_RG = set("ABCmefgtTMjErd")


def q(qs, text, answer, kind, match="exact", needle=None):
    qs.append({"q": text, "answer": str(answer), "kind": kind, "match": match})
    if needle is not None:
        qs[-1]["needle"] = needle


def _bre_to_py(p: str) -> str:
    out, i = [], 0
    while i < len(p):
        c = p[i]
        if c == "\\" and i + 1 < len(p):
            n = p[i + 1]
            out.append(n if n in "|(){}+?" else "\\" + n)
            i += 2
            continue
        out.append("\\" + c if c in "|(){}+?" else c)
        i += 1
    return "".join(out)


def grep_regexes(command: str | None) -> list[re.Pattern]:
    """Compiled patterns of every non-inverted grep/rg stage (best effort)."""
    if not command:
        return []
    out = []
    try:
        lx = shlex.shlex(command.replace("\n", " ; "), posix=True, punctuation_chars=";&|()<>")
        lx.whitespace_split = True
        toks = list(lx)
    except ValueError:
        return []
    segs, cur = [], []
    for t in toks:
        if t and set(t) <= set(";&|()<>"):
            segs.append(cur)
            cur = []
        else:
            cur.append(t)
    segs.append(cur)
    for w in segs:
        while w and ("=" in w[0] and not w[0].startswith("-") or w[0] in ("sudo", "timeout", "nice", "env", "cd")):
            w = w[2:] if w[0] in ("timeout", "cd") else w[1:]
        if not w:
            continue
        prog = w[0].rsplit("/", 1)[-1]
        args = w[1:]
        if prog == "git" and "grep" in args:
            args = args[args.index("grep") + 1:]
            prog = "grep"
        if prog not in GREPS:
            continue
        pats, pos, flags = [], [], set()
        ere = prog in ("egrep", "rg")
        fixed = prog == "fgrep"
        vals = VALUE_SHORT_RG if prog == "rg" else VALUE_SHORT
        i = 0
        while i < len(args):
            a = args[i]
            i += 1
            if a == "--":
                pos += args[i:]
                break
            if a.startswith("--"):
                name, eq, v = a[2:].partition("=")
                flags.add(name)
                if name == "regexp":
                    if not v and i < len(args):
                        v, i = args[i], i + 1
                    pats.append(v)
                elif not eq and name in ("file", "after-context", "before-context", "context", "max-count", "include",
                                        "exclude", "exclude-dir", "glob", "type", "max-columns", "label"):
                    i += 1
                continue
            if a.startswith("-") and len(a) > 1:
                for k, ch in enumerate(a[1:]):
                    flags.add(ch)
                    if ch in vals:
                        v = a[2 + k:]
                        if not v and i < len(args):
                            v, i = args[i], i + 1
                        if ch == "e":
                            pats.append(v)
                        break
                continue
            pos.append(a)
        if "v" in flags or "invert-match" in flags or "f" in flags:
            continue
        if not pats and pos:
            pats = [pos[0]]
        ere = ere or bool(flags & {"E", "P", "extended-regexp", "perl-regexp"}) and prog != "rg"
        if prog == "rg":
            ere = True
        fixed = fixed or "F" in flags or "fixed-strings" in flags
        ci = bool(flags & {"i", "y", "ignore-case"})
        if prog == "rg" and ("S" in flags or "smart-case" in flags):
            ci = ci or not any(ch.isupper() for p in pats for ch in p)
        for p in pats:
            try:
                src = re.escape(p) if fixed else (p if ere else _bre_to_py(p))
                src = src.replace("\\<", r"\b").replace("\\>", r"\b")
                out.append(re.compile(src, re.I if ci else 0))
            except re.error:
                continue
    return out


def _first_match(text: str, rxs):
    best = None
    for rx in rxs:
        m = rx.search(text)
        if m and m.end() > m.start() and (best is None or m.start() < best.start()):
            best = m
    return best


def _near_snip(text: str, m, others: list[str], rng) -> str | None:
    """A short substring touching the match, unique among the hit texts."""
    lo, hi = max(0, m.start() - 60), min(len(text), m.end() + 60)
    win = text[lo:hi]
    words = [w for w in re.finditer(r"[A-Za-z_][A-Za-z0-9_]{3,}", win)]
    rng.shuffle(words)
    for w in words[:12]:
        s = w.group(0)
        if sum(o.count(s) for o in others) == 1 and "`" not in s:
            return s
    for n in (24, 32, 40):
        for start in range(max(0, m.start() - lo - n // 2), min(len(win) - n, m.start() - lo + 1) + 1, 4):
            s = win[start:start + n]
            if len(s.strip()) >= 12 and "`" not in s and sum(o.count(s) for o in others) == 1:
                return s.strip()
    return None


def gen_grep_long(out, rng, sample):
    rxs = grep_regexes((sample.get("input") or {}).get("command") if isinstance(sample.get("input"), dict) else None)
    if not rxs:
        return []
    hits = []
    for l in out.split("\n"):
        m = HIT.match(l)
        if m:
            hits.append((m.group(1), int(m.group(2)), m.group(3)))
            continue
        m = NUM.match(l)
        if m:
            hits.append((None, int(m.group(1)), m.group(2)))
    longs = [h for h in hits if len(h[2].encode()) > LONG]
    if not longs:
        return []
    texts = [h[2] for h in hits]
    qs = []
    rng.shuffle(longs)
    for path, line, text in longs:
        m = _first_match(text, rxs)
        if not m:
            continue
        s = _near_snip(text, m, texts, rng)
        if not s:
            continue
        if path:
            q(qs, f"Which file contains the match whose text includes `{s}`? Give the file's path as grep reported it, "
                  "including every directory component.", path, "elide:path", "exact", needle=s)
        q(qs, f"What line number is the match whose text includes `{s}`?", line, "elide:line", "int", needle=s)
        after = re.findall(r"\S+", text[m.end():m.end() + 200])[:4]
        if len(after) == 4 and "`" not in m.group(0) and len(m.group(0)) <= 80 and not any("`" in w for w in after):
            where = f"line {line} of `{path}`" if path else f"line {line}"
            q(qs, f"On {where}, what are the 4 whitespace-separated words that come right after the first occurrence of "
                  f"`{m.group(0)}`? Quote them exactly, separated by single spaces.", " ".join(after), "elide:after", "line",
              needle=m.group(0))
        if len(qs) >= 3:
            break
    return qs


def gen_ls_columns(out, rng, sample):
    rows = [m for m in (LS.match(l) for l in out.split("\n")) if m and re.match(r"^\d", m.group(5) or "")]
    if len(rows) < 4:
        return []
    def name(m):
        d = DATE.match(m.group(6))
        return d.group(1).split(" -> ")[0] if d else None
    names = Counter(name(m) for m in rows)
    uniq = [m for m in rows if name(m) and names[name(m)] == 1 and name(m) not in (".", "..") and "`" not in name(m)]
    qs = []
    if len(uniq) >= 2:
        a, b = rng.sample(uniq, 2)
        q(qs, f"Which group does `{name(a)}` belong to (the group column of the listing)?", a.group(4), "ls:group", "exact")
        q(qs, f"How many hard links does `{name(b)}` have (the number right after the permissions)?", b.group(2), "ls:links", "int")
    owners = Counter(m.group(3) for m in rows)
    top = owners.most_common(1)[0][0]
    odd = [m for m in rows if m.group(3) != top and name(m) and names[name(m)] == 1 and "`" not in name(m)]
    if odd:
        m = rng.choice(odd)
        q(qs, f"Which user owns `{name(m)}`?", m.group(3), "ls:owner_other", "exact")
    elif uniq:
        m = rng.choice(uniq)
        q(qs, f"Which user owns `{name(m)}`?", m.group(3), "ls:owner", "exact")
    return qs
