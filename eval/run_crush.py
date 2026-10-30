#!/usr/bin/env python3
"""Run crush 2 (crush-core) over a sample set and record what it did.

  --set corpus    the private real corpus (results stay in ~/.cache/crush-corpus)
  --set fixtures  the committed synthetic fixtures (results in eval/out/)

Two paths:
 1. in-process: eval/driver (links crush-core, calls `crush_core::compress`
    exactly as a host does) over EVERY sample -> class, transforms, lossless,
    bytes, estimated tokens, ns, panics, an outside idempotence check, and the
    crushed text.
 2. end-to-end: the real `crush-ext` binary as Synaps runs it (Content-Length
    JSON-RPC over stdio, one long-lived process) over a subset -> round-trip
    latency, protocol errors, and a consistency check against path 1.

Each result row carries `class` (the crush-core classifier, what decides
transforms) and `class_p0` (the Phase 0 Python classifier, kept as an
independent CONTENT check).
"""
from __future__ import annotations

import argparse
import json
import os
import random
import re
import subprocess
import sys
import tempfile
import threading
import time
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from common import (CORPUS_DIR, EVAL_CONFIG, FIXTURES, OUT, REPO, quantile, read_jsonl, tagged,  # noqa: E402
                    write_jsonl_private)

TARGET = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
DRIVER = TARGET / "release" / "crush-eval-driver"
EXT = TARGET / "release" / "crush-ext"
RAW_REF_RE = re.compile(r"raw output: [^\]]*\]")


def load_samples(which: str):
    if which == "fixtures":
        man = json.loads((FIXTURES / "manifest.json").read_text())
        for f in man["fixtures"]:
            out = (FIXTURES / f["file"]).read_bytes().decode("utf-8")  # keep CR bytes
            yield {"id": f["name"], "tool": f["tool"], "input": f["input"], "output": out,
                   "class": f["class"], "source": "fixture", "bytes": len(out.encode())}
    else:
        yield from read_jsonl(CORPUS_DIR / "corpus.jsonl")


def results_path(which: str) -> Path:
    return (CORPUS_DIR / tagged("crush2.results.jsonl")) if which == "corpus" else (OUT / tagged("fixtures.crush2.results.jsonl"))


def label(r) -> str | None:
    return "+".join(r.get("transforms") or []) or None


def run_driver(samples, reps: int, extra=()):
    cfg = ["--config", EVAL_CONFIG] if EVAL_CONFIG else []
    p = subprocess.Popen([str(DRIVER), "--reps", str(reps), *cfg, *extra], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         text=True, encoding="utf-8")
    res = []

    def rd():
        for line in p.stdout:
            res.append(json.loads(line))
    th = threading.Thread(target=rd)
    th.start()
    for s in samples:
        p.stdin.write(json.dumps({"id": s["id"], "tool": s["tool"], "input": s["input"], "output": s["output"]}) + "\n")
    p.stdin.close()
    th.join()
    rc = p.wait()
    if rc != 0:
        sys.exit(f"driver exited {rc}")
    return res


# ── end-to-end through the real binary ──────────────────────────────────────

def frame(obj) -> bytes:
    b = json.dumps(obj).encode()
    return b"Content-Length: %d\r\n\r\n%s" % (len(b), b)


def read_frame(f):
    length = None
    while True:
        line = f.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1])
    if length is None:
        return None
    return json.loads(f.read(length))


def run_e2e(samples):
    raw_dir = tempfile.mkdtemp(prefix="crush-e2e-")
    cfg = ["--config", EVAL_CONFIG] if EVAL_CONFIG else []
    p = subprocess.Popen([str(EXT), "--keep-raw-dir", raw_dir, *cfg], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE)
    err_lines = []
    threading.Thread(target=lambda: err_lines.extend(p.stderr.read().decode(errors="replace").splitlines()), daemon=True).start()
    p.stdin.write(frame({"jsonrpc": "2.0", "id": 0, "method": "initialize"}))
    p.stdin.flush()
    init = read_frame(p.stdout)
    out = []
    for i, s in enumerate(samples, start=1):
        msg = frame({"jsonrpc": "2.0", "id": i, "method": "hook.handle",
                     "params": {"kind": "after_tool_call", "tool_name": s["tool"], "tool_input": s["input"],
                                "tool_output": s["output"]}})
        t = time.perf_counter_ns()
        p.stdin.write(msg)
        p.stdin.flush()
        r = read_frame(p.stdout)
        dt = time.perf_counter_ns() - t
        if r is None:
            out.append({"id": s["id"], "ok": False, "why": "no response (process died?)", "ns": dt})
            break
        res = r.get("result") or {}
        out.append({"id": s["id"], "ok": r.get("id") == i and res.get("action") in ("replace", "continue"),
                    "action": res.get("action"), "output": res.get("output"), "ns": dt, "error": r.get("error")})
    try:
        p.stdin.write(frame({"jsonrpc": "2.0", "id": 10**9, "method": "shutdown"}))
        p.stdin.flush()
        p.stdin.close()
    except BrokenPipeError:
        pass
    rc = p.wait(timeout=30)
    time.sleep(0.1)
    kept = len(os.listdir(raw_dir))
    for f in os.listdir(raw_dir):
        os.unlink(os.path.join(raw_dir, f))
    os.rmdir(raw_dir)
    return init, out, rc, err_lines, kept


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", choices=["corpus", "fixtures"], required=True)
    ap.add_argument("--e2e-sample", type=int, default=3000, help="random non-fired samples for the binary path")
    ap.add_argument("--reps", type=int, default=3, help="in-process timing: best of N")
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()
    for b in (DRIVER, EXT):
        if not b.exists():
            sys.exit(f"missing {b}; run eval/run.sh (it builds both)")

    samples = list(load_samples(args.set))
    t0 = time.time()
    res = run_driver(samples, args.reps)
    by_id = {r["id"]: r for r in res}
    print(f"[{args.set}] in-process: {len(res)} samples in {time.time() - t0:.1f}s", file=sys.stderr)

    rng = random.Random(args.seed)
    fired_ids = {r["id"] for r in res if r["fired"]}
    rest = [s for s in samples if s["id"] not in fired_ids]
    sub = [s for s in samples if s["id"] in fired_ids] + rng.sample(rest, min(args.e2e_sample, len(rest)))
    rng.shuffle(sub)
    init, e2e, rc, err_lines, kept = run_e2e(sub)
    mismatch = 0
    for e in e2e:
        r = by_id[e["id"]]
        want = r.get("output") if r["fired"] else None
        got = e.get("output") if e.get("action") == "replace" else None
        if want is not None:
            want = RAW_REF_RE.sub("raw output: R]", want)
        if got is not None:
            got = RAW_REF_RE.sub("raw output: R]", got)
        if want != got:
            mismatch += 1
    e2e_ns = [e["ns"] for e in e2e]
    big = {s["id"] for s in sub if s["bytes"] >= 2048}
    e2e_big = [e["ns"] for e in e2e if e["id"] in big]

    rows = []
    for s in samples:
        r = by_id[s["id"]]
        rows.append({"id": s["id"], "class": r["class"], "reason": r["reason"], "class_p0": s["class"],
                     "source": s["source"], "tool": s["tool"], "fired": r["fired"], "label": label(r),
                     "transforms": r.get("transforms") or [], "lossless": r.get("lossless", True), "why": r.get("why"),
                     "in_bytes": r["in_bytes"], "out_bytes": r["out_bytes"],
                     "est_v5_in": r["est_v5_in"], "est_v5_out": r.get("est_v5_out", r["est_v5_in"]),
                     "est_v4_in": r["est_v4_in"], "est_v4_out": r.get("est_v4_out", r["est_v4_in"]),
                     "ns": r["ns"], "panicked": r["panicked"], "idempotent": r.get("idempotent"),
                     "output": r.get("output")})
    write_jsonl_private(results_path(args.set), rows)

    ns_all = [r["ns"] for r in res]
    ns_big = [r["ns"] for r in res if r["in_bytes"] >= 2048]
    ns_fired = [r["ns"] for r in res if r["fired"]]
    summary = {
        "set": args.set, "samples": len(res),
        "fired": len(fired_ids), "panics": sum(r["panicked"] for r in res),
        "not_idempotent": sum(1 for r in res if r.get("idempotent") is False),
        "lossy": sum(1 for r in res if r["fired"] and not r.get("lossless", True)),
        "why": dict(Counter(r.get("why") for r in res)),
        "transforms": dict(Counter(label(r) for r in res if r["fired"])),
        "class_counts": dict(Counter(r["class"] for r in res)),
        "in_process_ns": {"p50_all": quantile(ns_all, .5), "p99_all": quantile(ns_all, .99), "max_all": max(ns_all),
                          "p50_ge2k": quantile(ns_big, .5), "p99_ge2k": quantile(ns_big, .99), "n_ge2k": len(ns_big),
                          "p50_fired": quantile(ns_fired, .5), "p99_fired": quantile(ns_fired, .99),
                          "max_fired": max(ns_fired) if ns_fired else None, "reps": args.reps},
        "e2e": {"initialize_ok": bool(init and init.get("result", {}).get("protocol_version") == 1),
                "requests": len(e2e), "ok": sum(e["ok"] for e in e2e), "exit_code": rc,
                "mismatch_vs_inprocess": mismatch, "raw_files_kept": kept,
                "stderr_panics": sum("panicked" in l for l in err_lines),
                "stderr_frame_errors": sum("frame error" in l for l in err_lines),
                "rtt_ns": {"p50": quantile(e2e_ns, .5), "p99": quantile(e2e_ns, .99), "max": max(e2e_ns) if e2e_ns else None,
                           "p50_ge2k": quantile(e2e_big, .5), "p99_ge2k": quantile(e2e_big, .99), "n_ge2k": len(e2e_big)}},
    }
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / tagged(f"{args.set}.run.json")).write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
