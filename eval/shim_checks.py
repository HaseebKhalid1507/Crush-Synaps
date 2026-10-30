#!/usr/bin/env python3
"""Gate checks for the crush-ext shim, run against the real binary over the
Synaps extension protocol (Content-Length framed JSON-RPC on stdio).

What crush-core can't check on its own, because it's the shim's job:

- host-truncated input (`…[truncated, N total bytes]`, Synaps' 256 KiB hook
  cut) passes through;
- the tool's runtime name is preferred over the API-safe one;
- a lossy result's raw is in the raw store, complete, 0600 in a 0700 dir,
  before the response arrives, and the marker quotes its absolute path; an
  unwritable store means pass-through;
- the spill rule: a crush that lands just under Synaps' spill threshold
  (costlier than the preview) passes through; one far under, and one still
  over it, replace; a test log whose failures the preview would drop keeps
  them (cargo workspace fixture, test_pass on);
- config: `initialize` values apply, a plugin-config edit applies without a
  restart;
- retention: an expired raw is swept at startup, a fresh one stays;
- `/crush` (command.invoke) reports, and the stats file survives shutdown;
- a malformed frame doesn't lose the next request;
- end-to-end time per request (round trip through the pipe), every fixture
  plus 256 KiB worst cases.

Writes eval/out/shim.json (no real data: fixtures and synthetic text only).
"""
from __future__ import annotations

import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
from common import FIXTURES, OUT, REPO, quantile  # noqa: E402
from spill import after_spill  # noqa: E402

TARGET = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
EXT = TARGET / "release" / "crush-ext"
FAIL = re.compile(r"FAILED|panicked at|✕|✖|--- FAIL|^FAIL\b|Traceback \(most recent|^error(\[E\d+\])?:|assertion|AssertionError")

checks: list[dict] = []


def check(name: str, ok: bool, detail=None):
    checks.append({"check": name, "ok": bool(ok), "detail": detail})
    print(("  ok   " if ok else "  FAIL ") + name + (f"  ({detail})" if detail is not None and not ok else ""), file=sys.stderr)


def frame(obj) -> bytes:
    b = json.dumps(obj).encode()
    return b"Content-Length: %d\r\n\r\n%s" % (len(b), b)


class Ext:
    def __init__(self, state: Path, *extra: str, home: Path | None = None):
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(home or state), "LANG": "C.UTF-8"}
        self.p = subprocess.Popen([str(EXT), "--state-dir", str(state), "--timing", *extra], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
        self.err: list[str] = []
        threading.Thread(target=lambda: self.err.extend(self.p.stderr.read().decode(errors="replace").splitlines()),
                         daemon=True).start()
        self.n = 0
        self.notes: list = []

    def _read(self):
        length = None
        while True:
            line = self.p.stdout.readline()
            if not line:
                return None
            line = line.strip()
            if not line:
                break
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":", 1)[1])
        return json.loads(self.p.stdout.read(length))

    def call(self, method, params=None, raw: bytes | None = None):
        self.n += 1
        msg = {"jsonrpc": "2.0", "id": self.n, "method": method}
        if params is not None:
            msg["params"] = params
        self.p.stdin.write((raw or b"") + frame(msg))
        self.p.stdin.flush()
        while True:
            r = self._read()
            if r is None or r.get("id") == self.n:
                return r
            # notifications (command.output) before the response
            self.notes.append(r)

    def hook(self, tool, command, output, session="sess-gate", runtime=None, extra=None):
        p = {"kind": "after_tool_call", "tool_name": tool, "tool_input": {"command": command} if command else {},
             "tool_output": output, "session_id": session}
        if runtime is not None:
            p["tool_runtime_name"] = runtime
        if extra:
            p.update(extra)
        t = time.perf_counter_ns()
        r = self.call("hook.handle", p)
        return (r or {}).get("result") or {}, time.perf_counter_ns() - t

    def close(self):
        try:
            self.call("shutdown")
            self.p.stdin.close()
        except (BrokenPipeError, ValueError):
            pass
        return self.p.wait(timeout=30)


def fixtures():
    man = json.loads((FIXTURES / "manifest.json").read_text())
    for f in man["fixtures"]:
        yield f, (FIXTURES / f["file"]).read_bytes().decode("utf-8")


def fixture(name):
    return next(o for f, o in fixtures() if f["name"] == name)


def poll(n):
    return "waiting for the build to finish\n" + "pending (build 4711 queued behind 3 jobs)\n" * n + "done\n"


def just_over(before, run, after):
    s = "".join(f"{i:04} item-{i} state=ready owner=svc-{i % 7}\n" for i in range(before))
    s += "retrying connection to db-primary:5432 (connection refused)\n" * run
    return s + "".join(f"{i:04} tail-{i} state=done owner=svc-{i % 5}\n" for i in range(after))


def blob():
    b = "".join("QmFzZTY0IGlzIGZ1bjEyMw9xYZ+/"[i % 28] for i in range(2000))
    return f"token: {b}\n"


def build_log(size):
    lines, i = [], 0
    while sum(map(len, lines)) < size:
        lines.append(f"   Compiling crate-{i:05} v0.{i % 9}.{i % 13} (registry)\n")
        i += 1
        if i % 400 == 0:
            lines.append(f"warning: unused variable `x{i}`\n  --> src/lib.rs:{i}:9\n")
    return "".join(lines) + "    Finished `release` profile [optimized] target(s) in 41.07s\n"


def synaps_truncate(text: str, cap=256 * 1024) -> str:
    b = text.encode()
    if len(b) <= cap:
        return text
    cut = cap
    while cut > 0 and (b[cut] & 0xC0) == 0x80:
        cut -= 1
    return b[:cut].decode() + f"…[truncated, {len(b)} total bytes]"


def mode(p: Path) -> int:
    return stat.S_IMODE(p.stat().st_mode)


def main() -> int:
    if not EXT.exists():
        sys.exit(f"missing {EXT}; build the workspace first")
    root = Path(tempfile.mkdtemp(prefix="crush-shim-"))
    try:
        run(root)
    finally:
        shutil.rmtree(root, ignore_errors=True)
    OUT.mkdir(parents=True, exist_ok=True)
    out = {"checks": checks, "timing": TIMING}
    (OUT / "shim.json").write_text(json.dumps(out, indent=1))
    bad = [c for c in checks if not c["ok"]]
    print(f"shim: {len(checks) - len(bad)} ok, {len(bad)} failed", file=sys.stderr)
    return 1 if bad else 0


TIMING: dict = {}


def run(root: Path):
    state = root / "state"
    base = root / "base"
    plugin = base / "plugins" / "crush"
    plugin.mkdir(parents=True)
    # retention: one expired raw and one fresh, before the shim starts
    old = state / "raw" / "sess-old" / ("0" * 16 + ".txt")
    fresh = state / "raw" / "sess-old" / ("1" * 16 + ".txt")
    old.parent.mkdir(parents=True)
    old.write_text("old raw\n")
    fresh.write_text("fresh raw\n")
    t = time.time() - 8 * 86400
    os.utime(old, (t, t))

    e = Ext(state, home=root)
    r = e.call("initialize", {"plugin_id": "crush", "plugin_root": str(plugin),
                              "config": {"class_prose": False, "raw_retention_days": 7, "transform_test_pass": True}})
    check("initialize: protocol_version 1", (r or {}).get("result", {}).get("protocol_version") == 1, r)

    # host truncation
    big = build_log(300 * 1024)
    res, _ = e.hook("bash", "cargo build", synaps_truncate(big))
    check("host-truncated (>256 KiB) output passes through", res.get("action") == "continue", res.get("action"))
    res, _ = e.hook("bash", "cargo build", build_log(200 * 1024))
    check("the same shape under 256 KiB is crushed", res.get("action") == "replace", res.get("action"))

    # runtime name
    res, _ = e.hook("ext__fs__x", None, poll(60), runtime="ext__web-tools__fetch")
    check("tool_runtime_name preferred (MCP web fetch -> PROSE, left alone)", res.get("action") == "continue")
    res, _ = e.hook("ext__fs__x", None, poll(60))
    check("tool_name fallback without a runtime name", res.get("action") == "replace")

    # lossy: raw kept first, marker quotes it
    b = blob()
    res, _ = e.hook("bash", "curl -s https://example.com/token", b)
    text = res.get("output") or ""
    m = re.search(r"(/[^\s\]]+\.txt)", text)
    path = Path(m.group(1)) if m else None
    check("lossy result replaced", res.get("action") == "replace", res.get("action"))
    check("marker quotes an absolute raw path", bool(path and path.is_absolute()), text[-200:])
    if path:
        check("raw present when the response arrives, byte-identical", path.exists() and path.read_text() == b)
        check("raw file 0600, its dirs 0700",
              path.exists() and mode(path) == 0o600 and mode(path.parent) == 0o700 and mode(path.parent.parent) == 0o700,
              path.exists() and oct(mode(path)))
        check("raw store under <state>/raw/<session>/", path.parent == state / "raw" / "sess-gate", str(path))
    res2, _ = e.hook("bash", "curl -s https://example.com/token", b)
    check("deterministic: same result, same replacement", res2.get("output") == text)

    # spill rule
    raw = just_over(150, 50, 135)
    res, _ = e.hook("bash", "./poll.sh", raw)
    check("spill: crushed just under the threshold (costlier than the preview) passes through",
          len(raw.encode()) > 12288 and res.get("action") == "continue", (len(raw), res.get("action")))
    raw = just_over(40, 400, 40)
    res, _ = e.hook("bash", "./poll.sh", raw)
    check("spill: crushed far under the preview replaces", res.get("action") == "replace")
    raw = just_over(250, 300, 150)
    res, _ = e.hook("bash", "./poll.sh", raw)
    out = res.get("output") or ""
    check("spill: both over the threshold, the crushed text is handed over",
          res.get("action") == "replace" and len(out.encode()) > 12288, (res.get("action"), len(out)))
    ws = fixture("cargo_workspace_test_fail")
    res, _ = e.hook("bash", "cargo test --workspace", ws)
    out = res.get("output") or ws
    fl = lambda t: sum(1 for l in after_spill(t).split("\n") if FAIL.search(l) and not l.startswith("[crush:"))
    vis_raw, vis_crushed, total = fl(ws), fl(out), sum(1 for l in ws.split("\n") if FAIL.search(l))
    check("spill: 17.9 KB cargo workspace log, test_pass on: failures visible after spill (raw vs crushed)",
          res.get("action") == "replace" and vis_crushed == total and vis_crushed > vis_raw,
          {"raw": vis_raw, "crushed": vis_crushed, "of": total})
    TIMING["workspace_failures_visible"] = {"raw_after_spill": vis_raw, "crushed_after_spill": vis_crushed, "of": total}

    # live config
    res, _ = e.hook("bash", "./poll.sh", poll(60), session="sess-cfg")
    on_before = res.get("action")
    (plugin / "config").write_text("# crush\nenabled = false\n")
    time.sleep(1.2)
    res, _ = e.hook("bash", "./poll.sh", poll(60), session="sess-cfg")
    check("plugin config edit applies without a restart", on_before == "replace" and res.get("action") == "continue",
          (on_before, res.get("action")))
    (plugin / "config").write_text("# crush\nenabled = true\n")
    time.sleep(1.2)
    res, _ = e.hook("bash", "./poll.sh", poll(60), session="sess-cfg")
    check("... and back", res.get("action") == "replace")

    # malformed frame, then a good request
    e.notes = []
    r = e.call("hook.handle", {"kind": "after_tool_call", "tool_name": "bash", "tool_output": "x"},
               raw=b"Content-Length: 5\r\n\r\n{oops")
    check("a malformed frame doesn't lose the next request", (r or {}).get("result", {}).get("action") == "continue")

    # /crush
    e.notes = []
    r = e.call("command.invoke", {"command": "crush", "args": [], "request_id": "rq"})
    body = next((n["params"]["event"].get("content", "") for n in e.notes
                 if n.get("method") == "command.output" and n["params"]["event"].get("kind") == "system"), "")
    check("/crush reports the latest session, all sessions, raw store",
          (r or {}).get("result", {}).get("ok") is True and "latest session" in body and "all sessions" in body
          and "raw store:" in body and "passed through:" in body, body[:300])
    TIMING["crush_report_sample"] = body
    check("/crush is short (<= 24 lines)", 0 < len(body.splitlines()) <= 24, len(body.splitlines()))

    # retention (the startup sweep runs on a background thread)
    deadline = time.time() + 5
    while old.exists() and time.time() < deadline:
        time.sleep(0.05)
    check("startup sweep deleted the expired raw, kept the fresh one", not old.exists() and fresh.exists())

    # timing: every fixture, then 256 KiB worst cases
    ns_all, ns_big = [], []
    for _ in range(10):
        for f, o in fixtures():
            _, dt = e.hook(f["tool"], f["input"].get("command"), o, runtime=f["tool"])
            ns_all.append(dt)
            if len(o.encode()) >= 2048:
                ns_big.append(dt)
    worst = {"build_log_256k": build_log(256 * 1024 - 200), "poll_256k": poll(5000)[: 256 * 1024],
             "blob_256k": (blob() * 200)[: 256 * 1024]}
    for k, o in worst.items():
        ts = sorted(e.hook("bash", "./x", o)[1] for _ in range(50))
        TIMING[k] = {"bytes": len(o.encode()), "p50_ms": quantile(ts, .5) / 1e6, "p99_ms": quantile(ts, .99) / 1e6}
    TIMING["fixtures"] = {"requests": len(ns_all), "p50_ms": quantile(ns_all, .5) / 1e6,
                          "p99_ms": quantile(ns_all, .99) / 1e6, "p50_ge2k_ms": quantile(ns_big, .5) / 1e6,
                          "p99_ge2k_ms": quantile(ns_big, .99) / 1e6}
    rc = e.close()
    check("clean shutdown", rc == 0, rc)
    # in-process time per hook (the shim's own work: parse to response written)
    inproc = [int(l.split()[-1]) for l in e.err if l.startswith("[crush] timing hook.handle ")]
    TIMING["in_process_hook"] = {"requests": len(inproc), "p50_ms": quantile(inproc, .5) / 1e6,
                                 "p99_ms": quantile(inproc, .99) / 1e6, "max_ms": max(inproc) / 1e6 if inproc else None}
    check("no panics on stderr", not any("panicked" in l for l in e.err), [l for l in e.err if "panicked" in l][:3])
    st = json.loads((state / "stats.json").read_text()) if (state / "stats.json").exists() else {}
    check("stats.json persisted (0600) with per-session tallies",
          st.get("total", {}).get("results", 0) > 0 and "sess-gate" in st.get("sessions", {})
          and mode(state / "stats.json") == 0o600)
    r = subprocess.run([str(EXT), "--state-dir", str(state), "--report"], capture_output=True, text=True,
                       env={"PATH": os.environ.get("PATH", ""), "HOME": str(root)})
    check("crush-ext --report renders the persisted stats", r.returncode == 0 and "all sessions" in r.stdout)

    # an unwritable raw store: lossy passes through, lossless still works
    bad = root / "bad"
    (bad / "raw").mkdir(parents=True)
    (bad / "raw" / "sess-gate").write_text("a file where the session dir goes")
    e2 = Ext(bad, home=root)
    e2.call("initialize", {})
    res, _ = e2.hook("bash", "curl -s x", blob())
    check("unwritable raw store: the lossy result passes through", res.get("action") == "continue")
    res, _ = e2.hook("bash", "./poll.sh", poll(60))
    check("unwritable raw store: lossless results still replace", res.get("action") == "replace")
    e2.close()
    print(json.dumps(TIMING, indent=1), file=sys.stderr)


if __name__ == "__main__":
    sys.exit(main())
