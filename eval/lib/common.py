"""Shared helpers for the crush eval harness (stdlib only).

Privacy model:
- Real session data lives ONLY under CORPUS_DIR (~/.cache/crush-corpus, mode 0700,
  files 0600). Nothing under it is ever written into the repo.
- Committed artefacts (fixtures, BASELINE.md) contain synthetic data or aggregate
  numbers only.
"""
from __future__ import annotations

import hashlib
import json
import os
import stat
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
EVAL = REPO / "eval"
FIXTURES = EVAL / "fixtures"
OUT = EVAL / "out"  # gitignored; aggregate-only json for the report

CORPUS_DIR = Path(os.environ.get("CRUSH_CORPUS_DIR", Path.home() / ".cache" / "crush-corpus"))
SYNAPS_BASE = Path(os.environ.get("CRUSH_SYNAPS_SOURCE", Path.home() / ".synaps-cli"))

CLASSES = ["CONTENT", "SEARCH", "LISTING", "LOG", "STRUCTURED", "VCS", "UNKNOWN"]

# A variant run (e.g. a disabled transform switched on): CRUSH_EVAL_CONFIG is a
# crush-core JSON config for the driver and crush-ext, CRUSH_EVAL_TAG names the
# variant and suffixes every results/out file it writes.
TAG = os.environ.get("CRUSH_EVAL_TAG", "")
EVAL_CONFIG = os.environ.get("CRUSH_EVAL_CONFIG", "")


def tagged(name: str) -> str:
    """`tokens.corpus.json` -> `tokens.corpus.<tag>.json` for a variant run."""
    if not TAG:
        return name
    stem, dot, ext = name.rpartition(".")
    return f"{stem}.{TAG}.{ext}" if dot else f"{name}.{TAG}"


def private_dir(p: Path) -> Path:
    """Create (or tighten) a 0700 directory."""
    p.mkdir(parents=True, exist_ok=True)
    os.chmod(p, 0o700)
    return p


def write_private(path: Path, data: bytes | str) -> None:
    """Atomically write a 0600 file."""
    if isinstance(data, str):
        data = data.encode()
    private_dir(path.parent) if str(path).startswith(str(CORPUS_DIR)) else path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + f".tmp{os.getpid()}")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        os.write(fd, data)
    finally:
        os.close(fd)
    os.replace(tmp, path)


def open_private_append(path: Path):
    private_dir(path.parent)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    return os.fdopen(fd, "a", encoding="utf-8")


def sha(s: str | bytes, n: int = 64) -> str:
    if isinstance(s, str):
        s = s.encode("utf-8", "surrogatepass")
    return hashlib.sha256(s).hexdigest()[:n]


def read_jsonl(path: Path):
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                yield json.loads(line)


def write_jsonl_private(path: Path, rows) -> int:
    n = 0
    tmp = path.with_name(path.name + f".tmp{os.getpid()}")
    private_dir(path.parent)
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        for r in rows:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")
            n += 1
    os.replace(tmp, path)
    return n


def assert_not_in_repo(path: Path) -> None:
    """Refuse to write real-data artefacts inside the git checkout."""
    rp = path.resolve()
    if str(rp).startswith(str(REPO.resolve()) + os.sep):
        sys.exit(f"refusing to write private corpus data inside the repo: {rp}")


def check_private_perms(p: Path) -> None:
    st = p.stat()
    if stat.S_IMODE(st.st_mode) & 0o077:
        sys.exit(f"{p} is group/world accessible (mode {oct(stat.S_IMODE(st.st_mode))}); refusing")


def pct(a: float, b: float) -> float:
    return 0.0 if not b else 100.0 * a / b


def quantile(xs, q):
    if not xs:
        return float("nan")
    xs = sorted(xs)
    k = (len(xs) - 1) * q
    f = int(k)
    c = min(f + 1, len(xs) - 1)
    return xs[f] + (xs[c] - xs[f]) * (k - f)
