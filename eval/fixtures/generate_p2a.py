#!/usr/bin/env python3
"""Phase 2A fixtures: shapes the SEARCH elision and ls -l factoring target,
which the Phase 0/1 set doesn't cover. Appends to manifest.json (replacing
entries of the same name) and reuses generate.py's runner, sanitiser and leak
check. Every output is a real tool run over generated scratch files.

- grep_long_lines   real `grep -rn` over a tree with a minified bundle (long
                    lines), a JSONL event log and long markdown paragraphs,
                    next to ordinary short source lines
- grep_n_jsonl      real `grep -n` on one JSONL file (single-file, `N:` hits)
- ls_la_scratch     real `ls -la` of a scratch dir in /tmp: every entry is the
                    user's, `..` is root's (the common home-listing shape)

No network. Usage: generate_p2a.py
"""
from __future__ import annotations

import json
import random
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from generate import MANIFEST, Gen  # noqa: E402

NAMES = ["grep_long_lines", "grep_n_jsonl", "ls_la_scratch"]
WORDS = ("user session token cache render stream buffer config route handler query index payload retry "
         "timeout socket worker schedule account profile invoice ledger").split()


def minified(rng: random.Random, n_funcs: int, needle_every: int) -> str:
    parts = []
    for i in range(n_funcs):
        a, b = rng.choice(WORDS), rng.choice(WORDS)
        body = ";".join(f"var {rng.choice('abcdefghjkmnpqrstuvwxyz')}{k}=e.{rng.choice(WORDS)}[{k}]||{rng.randint(0, 999)}"
                        for k in range(rng.randint(3, 9)))
        name = "fetchUser" if i % needle_every == 7 else f"{a}{b.title()}{i}"
        parts.append(f"function {name}(e,t){{{body};return t?e:{rng.choice(WORDS)}({i})}}")
    return "!function(){" + ";".join(parts) + "}();"


def main() -> int:
    scratch = Path(tempfile.mkdtemp(prefix="crush-fx2a-"))
    g = Gen(scratch)
    rng = random.Random(23)
    try:
        for d in ("dist", "src/api", "src/ui", "logs", "docs"):
            (scratch / d).mkdir(parents=True)
        (scratch / "dist" / "app.min.js").write_text("\n".join(minified(rng, 260, 90) for _ in range(3)) + "\n")
        (scratch / "dist" / "vendor.min.js").write_text(minified(rng, 400, 150) + "\n")
        for f in ("users.ts", "client.ts", "cache.ts"):
            lines = []
            for i in range(120):
                w = rng.choice(WORDS)
                if i % 17 == 3:
                    lines.append(f"  const {w} = await fetchUser(ctx, {rng.randint(1, 99)});")
                else:
                    lines.append(f"  const {w}{i} = {rng.choice(WORDS)}.{rng.choice(WORDS)}({i});")
            (scratch / "src" / "api" / f).write_text("export async function run(ctx) {\n" + "\n".join(lines) + "\n}\n")
        ev = []
        for i in range(160):
            rec = {"ts": f"2026-10-{1 + i % 28:02d}T{i % 24:02d}:{i % 60:02d}:07Z", "level": rng.choice(["info", "warn", "error"]),
                   "op": rng.choice(["fetchUser", "renderPage", "syncLedger"]), "req": f"r-{rng.getrandbits(48):012x}",
                   "attrs": {w: rng.choice(WORDS) + str(rng.randint(0, 9999)) for w in rng.sample(WORDS, 12)},
                   "trace": [{"span": f"s{k}", "ms": rng.randint(1, 900), "note": " ".join(rng.choices(WORDS, k=9))} for k in range(9)]}
            if rec["level"] == "error":
                rec["status"] = "error"
            ev.append(json.dumps(rec, separators=(",", ":")))
        (scratch / "logs" / "events.jsonl").write_text("\n".join(ev) + "\n")
        paras = []
        for i in range(30):
            s = " ".join(rng.choices(WORDS, k=rng.randint(150, 260)))
            if i % 4 == 1:
                k = rng.randint(20, 120)
                ws = s.split(" ")
                ws.insert(k, "fetchUser")
                s = " ".join(ws)
            paras.append(s.capitalize() + ".")
        (scratch / "docs" / "design.md").write_text("# Design\n\n" + "\n\n".join(paras) + "\n")

        cmd = "grep -rn 'fetchUser' ."
        rc, out = g.sh(cmd + " 2>&1", scratch)
        g.add("grep_long_lines", "SEARCH", cmd, out, note="real grep over a minified bundle, a JSONL log, long markdown lines and short TS lines")
        cmd = "grep -n '\"status\":\"error\"' logs/events.jsonl"
        rc, out = g.sh(cmd + " 2>&1 | head -n 40", scratch)
        g.add("grep_n_jsonl", "SEARCH", cmd + " | head -n 40", out, note="real single-file grep -n over JSONL (long lines)")
        for i in range(14):
            (scratch / f"note-{i:02d}.md").write_text("x" * rng.randint(10, 9000))
        cmd = "ls -la"
        rc, out = g.sh(cmd, scratch)
        g.add("ls_la_scratch", "LISTING", cmd, out, note="real ls -la of a scratch dir in /tmp (.. is root's)")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    man = json.loads(MANIFEST.read_text())
    man["fixtures"] = [f for f in man["fixtures"] if f["name"] not in NAMES] + g.fixtures
    MANIFEST.write_text(json.dumps(man, indent=2) + "\n")
    print(f"manifest: {len(man['fixtures'])} fixtures")
    return 0


if __name__ == "__main__":
    sys.exit(main())
