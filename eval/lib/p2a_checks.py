"""Independent checks for the Phase 2A transforms (no crush-core code shared).

- elide_ok(raw, out): every output line is its raw line verbatim, or the raw
  line's own prefix + `[crush: N chars cut]` + one verbatim slice +
  `[crush: M chars cut]` (either marker optional) where N / M are exactly the
  characters missing on each side; one end note line; nothing else. The slice
  is what a model can quote: it must be a byte-exact substring of the line.
- ls_restore(out): undo the owner/group factoring from the note alone; the
  caller compares it with the raw (lossless round trip).
"""
from __future__ import annotations

import re

CUT = re.compile(r"\[crush: (\d{1,3}(?:,\d{3})*) chars cut\]")
END = re.compile(r"^\[crush: (\d{1,3}(?:,\d{3})*) lines? over [\d,]+ bytes cut to the text around the match; full lines in raw output: .*\]$")
NOTE_ALL = re.compile(r"^\[crush: all (\d{1,3}(?:,\d{3})*) entries below are owned by user (\S+), group (\S+); "
                      r"their rows omit those two columns \(ls -go layout\)\]$")
NOTE_SOME = re.compile(r"^\[crush: (\d{1,3}(?:,\d{3})*) of the (\d{1,3}(?:,\d{3})*) entries below are owned by user (\S+), "
                       r"group (\S+); their rows omit those two columns \(ls -go layout\); the other "
                       r"(?:entry shows|[\d,]+ entries show) its own owner and group\]$")
MODE = re.compile(r"^[-dlcbps][rwxsStT-]{9}[.+@]? +\d+")
STRICT = re.compile(r"^[-dlcbps][rwxsStT-]{9}[.+@]? +\d+ +(\S+) +(\S+) +(?:\d+(?:\.\d+)?[KMGTPEk]?|\d+,) +\S")


def _n(s: str) -> int:
    return int(s.replace(",", ""))


def _lines(s: str):
    t = s.endswith("\n")
    return (s[:-1] if t else s).split("\n"), t


def cut_line_ok(src: str, out: str) -> bool:
    ms = list(CUT.finditer(out))
    if not ms or len(ms) > 2:
        return False
    first, last = ms[0], ms[-1]
    lead = trail = 0
    if len(ms) == 2:
        prefix, kept = out[:first.start()], out[first.end():last.start()]
        lead, trail = _n(first.group(1)), _n(last.group(1))
        if last.end() != len(out):
            return False
    elif first.end() == len(out):
        prefix, kept, trail = "", out[:first.start()], _n(first.group(1))
    else:
        prefix, kept, lead = out[:first.start()], out[first.end():], _n(first.group(1))
    if not src.startswith(prefix):
        return False
    text = src[len(prefix):]
    # characters = code points, as the markers count them
    if lead + len(kept) + trail != len(text):
        return False
    return text[lead:len(text) - trail] == kept and bool(kept)


def elide_ok(raw: str, out: str) -> tuple[bool, int, int]:
    """(ok, lines cut, lines kept verbatim)."""
    x, tx = _lines(raw)
    y, ty = _lines(out)
    if tx != ty or len(y) != len(x) + 1:
        return False, 0, 0
    cut = same = 0
    for a, b in zip(x, y):
        if a == b:
            same += 1
        elif cut_line_ok(a, b):
            cut += 1
        else:
            return False, cut, same
    m = END.match(y[-1])
    return bool(m and _n(m.group(1)) == cut and cut > 0), cut, same


def ls_restore(out: str) -> str | None:
    y, t = _lines(out)
    res, i = [], 0
    while i < len(y):
        l = y[i]
        m_all, m_some = NOTE_ALL.match(l), NOTE_SOME.match(l)
        if m_all or m_some:
            if m_all:
                k, owner, group = _n(m_all.group(1)), m_all.group(2), m_all.group(3)
                n = k
            else:
                k, n, owner, group = _n(m_some.group(1)), _n(m_some.group(2)), m_some.group(3), m_some.group(4)
            rows = y[i + 1:i + 1 + n]
            if len(rows) != n:
                return None
            got = 0
            for r in rows:
                # factored iff putting the pair back gives a row with that pair
                m = MODE.match(r)
                cand = r[:m.end()] + f" {owner} {group}" + r[m.end():] if m else None
                s2 = STRICT.match(cand) if cand else None
                if s2 and (s2.group(1), s2.group(2)) == (owner, group):
                    res.append(cand)
                    got += 1
                elif STRICT.match(r):
                    res.append(r)  # a row that shows its own pair
                else:
                    return None
            if got != k:
                return None
            i += 1 + n
            continue
        if l.startswith("[crush: "):
            return None
        res.append(l)
        i += 1
    s = "\n".join(res)
    return s + "\n" if t else s
