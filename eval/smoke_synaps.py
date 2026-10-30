#!/usr/bin/env python3
"""End-to-end smoke test: crush-ext installed in a real Synaps (`synaps-dev rpc`).

Isolation, as in the accuracy harness (eval/accuracy/run_accuracy.py):
  - SYNAPS_BASE_DIR *and* HOME point at a private dir under $CRUSH_CORPUS_DIR;
  - its auth.json holds only the current access token (empty refresh), so
    nothing here can rotate the real credentials; the real
    ~/.synaps-cli/auth.json is hashed before and after (a change aborts);
  - config: one model, thinking off, every tool but `bash` disabled;
  - the plugin is installed the way /plugins does it: the source copied into
    `<base>/plugins/crush` and its `scripts/setup.sh` run there (release
    build, --locked);
  - everything is deleted at the end.

The model is asked to run three scripts with bash:
  1. ./wait.sh   a repeated-line run (4.6 KB)          -> crushed (repeats, lossless)
  2. ./build.sh  a 19 KB cargo-style build log         -> crushed (build_progress, lossy:
                                                           raw kept, marker points at it)
  3. ./flood.sh  400 KB of output (over the 256 KiB hook cap) -> passes through; Synaps spills it
Then the session history (`get_messages`), the raw store, Synaps' spill dir,
its intercept audit and crush's stats are checked. Writes eval/out/smoke.json
(no tokens, no real data: synthetic outputs only).
"""
from __future__ import annotations

import hashlib
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "lib"))
from common import CORPUS_DIR, OUT, REPO, private_dir  # noqa: E402

SYNAPS = os.environ.get("CRUSH_SMOKE_SYNAPS", "synaps-dev")
MODEL = os.environ.get("CRUSH_SMOKE_MODEL", "claude-haiku-4-5-20251001")
REAL_AUTH = Path.home() / ".synaps-cli" / "auth.json"
ALL_BUT_BASH = ("activate_tools,edit,find,forum_forget,forum_post,forum_read,grep,ls,memory_context,memory_fetch,"
                "memory_forget,memory_search,memory_store,read,search_tools,shell_end,shell_send,shell_start,subagent,"
                "subagent_collect,subagent_model_authorize,subagent_models,subagent_resume,subagent_start,"
                "subagent_status,subagent_steer,write")
SCRIPTS = {
    "wait.sh": 'for i in $(seq 1 80); do echo "waiting for lock /tmp/crush-smoke.lock (held by pid 4242)"; done\n'
               'echo acquired\n',
    "build.sh": 'for i in $(seq 0 599); do\n'
                '  printf "   Compiling crate-%03d v0.%d.%d\\n" $i $((i % 9)) $((i % 13))\n'
                '  if [ $((i % 150)) -eq 149 ]; then\n'
                '    printf "warning: unused variable: \\`x%d\\`\\n  --> src/lib.rs:%d:9\\n\\n" $i $i\n'
                '  fi\n'
                'done\n'
                'echo "    Finished \\`release\\` profile [optimized] target(s) in 41.07s"\n',
    "flood.sh": 'yes "filler line for the over-256-KiB smoke test, nothing to see here" | head -c 400000\n',
}
PROMPT = ("Run exactly these three commands with the bash tool, one tool call each, in this order, from the "
          "current directory: `./wait.sh`, then `./build.sh`, then `./flood.sh`. Don't run anything else and "
          "don't explain. After the third, reply with the single word DONE.")
PLUGIN_FILES = ["Cargo.toml", "Cargo.lock", "crates", ".synaps-plugin", "scripts", "LICENSE", "rustfmt.toml"]


def digest(p: Path) -> str:
    try:
        return hashlib.sha256(p.read_bytes()).hexdigest()
    except FileNotFoundError:
        return "missing"


results: list[dict] = []


def check(name, ok, detail=None):
    results.append({"check": name, "ok": bool(ok), "detail": detail})
    print(("  ok   " if ok else "  FAIL ") + name + ("" if ok or detail is None else f"  ({detail})"), file=sys.stderr)


class Rpc:
    def __init__(self, base: Path, cwd: Path):
        env = {"PATH": os.environ.get("PATH", "/usr/local/bin:/usr/bin:/bin"), "HOME": str(base),
               "SYNAPS_BASE_DIR": str(base), "LANG": "C.UTF-8"}
        self.p = subprocess.Popen([SYNAPS, "rpc", "-m", MODEL], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, env=env, text=True, encoding="utf-8", cwd=cwd)
        self.q: queue.Queue = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            try:
                self.q.put(json.loads(line))
            except Exception:
                pass
        self.q.put(None)

    def wait(self, pred, timeout, sink=None):
        end = time.time() + timeout
        while time.time() < end:
            try:
                e = self.q.get(timeout=max(0.1, end - time.time()))
            except queue.Empty:
                break
            if e is None:
                return None
            if sink:
                sink(e)
            if pred(e):
                return e
        return None

    def send(self, obj):
        self.p.stdin.write(json.dumps(obj) + "\n")
        self.p.stdin.flush()

    def close(self):
        try:
            self.send({"type": "shutdown"})
            self.p.wait(timeout=30)
        except Exception:
            self.p.kill()


def tool_results(messages):
    """(tool_use input command, result text) pairs from api messages, in order."""
    uses, out = {}, []
    for m in messages:
        content = m.get("content")
        if not isinstance(content, list):
            continue
        for b in content:
            if b.get("type") == "tool_use":
                uses[b.get("id")] = (b.get("input") or {}).get("command", "")
            elif b.get("type") == "tool_result":
                c = b.get("content")
                if isinstance(c, list):
                    c = "".join(x.get("text", "") for x in c if isinstance(x, dict))
                out.append((uses.get(b.get("tool_use_id"), ""), c or ""))
    return out


def main() -> int:
    if not shutil.which(SYNAPS):
        sys.exit(f"{SYNAPS} not on PATH")
    before = digest(REAL_AUTH)
    real = json.loads(REAL_AUTH.read_text())["anthropic"]
    if real.get("expires", 0) / 1000 - time.time() < 1800:
        sys.exit("Anthropic access token expires within 30 min; let Synaps refresh it and rerun "
                 "(this harness never refreshes tokens).")
    base = private_dir(CORPUS_DIR / f"smoke{os.getpid()}")
    summary: dict = {"synaps": SYNAPS, "model": MODEL}
    try:
        run(base, real, summary)
    finally:
        shutil.rmtree(base, ignore_errors=True)
        after = digest(REAL_AUTH)
        check("real auth.json unchanged", after == before)
        check("isolated dirs deleted", not base.exists())
    summary["checks"] = results
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "smoke.json").write_text(json.dumps(summary, indent=1))
    bad = [r for r in results if not r["ok"]]
    print(f"smoke: {len(results) - len(bad)} ok, {len(bad)} failed", file=sys.stderr)
    return 1 if bad else 0


def run(base: Path, real: dict, summary: dict):
    fd = os.open(base / "auth.json", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, json.dumps({"anthropic": {"type": "oauth", "refresh": "", "access": real["access"],
                                           "expires": real["expires"]}}).encode())
    os.close(fd)
    (base / "config").write_text(f"model = {MODEL}\nthinking = off\ntelemetry = full\n"
                                 f"disabled_tools = {ALL_BUT_BASH}\n")
    # install the plugin the way /plugins does: source in place, then its setup step
    plugin = base / "plugins" / "crush"
    plugin.mkdir(parents=True)
    for f in PLUGIN_FILES:
        src = REPO / f
        if src.is_dir():
            shutil.copytree(src, plugin / f, ignore=shutil.ignore_patterns("target"))
        elif src.exists():
            shutil.copy2(src, plugin / f)
    t = time.time()
    env = {k: v for k, v in os.environ.items() if k not in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR")}
    r = subprocess.run(["bash", "scripts/setup.sh"], cwd=plugin, env=env, capture_output=True, text=True)
    summary["setup_secs"] = round(time.time() - t, 1)
    check("scripts/setup.sh builds target/release/crush-ext (--locked)",
          r.returncode == 0 and (plugin / "target" / "release" / "crush-ext").exists(), r.stderr[-500:])
    shutil.rmtree(plugin / "target" / "release" / "build", ignore_errors=True)
    shutil.rmtree(plugin / "target" / "release" / "deps", ignore_errors=True)
    work = base / "work"
    work.mkdir()
    for name, body in SCRIPTS.items():
        p = work / name
        p.write_text("#!/usr/bin/env bash\n" + body)
        p.chmod(0o755)
    expected = {name: subprocess.run(["bash", str(work / name)], capture_output=True, text=True, cwd=work).stdout
                for name in SCRIPTS}

    rpc = Rpc(base, work)
    ready = rpc.wait(lambda e: e.get("type") == "ready", 60)
    check("synaps rpc ready (extension loaded)", ready is not None)
    session_id = (ready or {}).get("session_id")
    rpc.send({"type": "prompt", "id": "p1", "message": PROMPT})
    tools, errors = [], []

    def sink(e):
        if e.get("type") == "message_update" and e.get("event", {}).get("type") == "toolcall_start":
            tools.append(e)
        if e.get("type") == "error":
            errors.append(str(e.get("message", ""))[:200])
    done = rpc.wait(lambda e: e.get("type") == "response" and e.get("id") == "p1", 300, sink)
    check("prompt finished", bool(done and done.get("ok")), errors[:3])
    rpc.send({"type": "get_messages", "id": "m1"})
    msgs = rpc.wait(lambda e: e.get("type") == "response" and e.get("id") == "m1", 30)
    messages = (msgs or {}).get("messages") or (msgs or {}).get("data", {}).get("messages") or []
    rpc.close()
    pairs = tool_results(messages)
    summary["tool_calls"] = [c for c, _ in pairs]
    by = {}
    for cmd, text in pairs:
        for name in SCRIPTS:
            if name in cmd and name not in by:
                by[name] = text
    check("the model ran the three scripts", set(by) == set(SCRIPTS), summary["tool_calls"])

    raw_root = base / ".cache" / "crush" / "raw"
    # 1. repeated lines
    w = by.get("wait.sh", "")
    check("wait.sh: history holds crushed text (repeats)",
          "[crush: previous line repeated 79 more times; 80 identical lines in total]" in w
          and w.count("waiting for lock") == 1, w[:300])
    # 2. build log
    b = by.get("build.sh", "")
    m = re.search(r"raw output: (/[^\]\s]+\.txt)\]", b)
    raw = Path(m.group(1)) if m else None
    check("build.sh: history holds crushed text (build_progress)",
          "build-progress lines not shown" in b and not re.search(r"^ +Compiling ", b, re.M) and "Finished" in b
          and b.count("warning: unused variable") == 4, b[:400])
    check("build.sh: the marker's raw file exists in crush's raw store",
          bool(raw and raw.exists() and raw_root in raw.parents), str(raw))
    check("build.sh: the raw file is readable and byte-identical to the output",
          bool(raw and raw.exists() and raw.read_text() == expected["build.sh"]))
    check("build.sh: raw file 0600 in a 0700 session dir", bool(
        raw and raw.exists() and (raw.stat().st_mode & 0o777) == 0o600 and (raw.parent.stat().st_mode & 0o777) == 0o700))
    summary["raw_dir_session_component"] = raw.parent.name if raw else None
    summary["session_id"] = session_id
    summary["history_bytes"] = {k: len(v.encode()) for k, v in by.items()}
    summary["output_bytes"] = {k: len(v.encode()) for k, v in expected.items()}
    # 3. over 256 KiB: passes through, Synaps spills it
    f = by.get("flood.sh", "")
    spills = list((base / "spill").rglob("*.txt")) if (base / "spill").exists() else []
    check("flood.sh (>256 KiB): passed through (no crush marker)", "[crush:" not in f and "filler line" in f, f[:200])
    check("flood.sh: Synaps spilled it (preview + note in history, spill file on disk)",
          "[Output virtualised:" in f and len(spills) >= 1, (f[-300:], [s.name for s in spills]))
    big = max(spills, key=lambda p: p.stat().st_size) if spills else None
    summary["spill_file_bytes"] = big.stat().st_size if big else None
    check("Synaps' spill file holds the raw (bash-capped) output, not crush text",
          bool(big and "[crush:" not in big.read_text() and big.stat().st_size >= 256 * 1024))
    check("only flood.sh spilled (the crushed results fit)", len(spills) == 1, [s.name for s in spills])
    # Synaps' own audit of hook replacements
    audit = base / "extensions" / "intercept.jsonl"
    recs = [json.loads(l) for l in audit.read_text().splitlines()] if audit.exists() else []
    summary["intercept_audit"] = [{k: r.get(k) for k in ("plugin_id", "tool_name", "observed_len", "replacement_len")
                                   if k in r} for r in recs]
    check("Synaps' intercept audit: exactly two replacements, both by crush", len(recs) == 2
          and all("crush" in json.dumps(r) for r in recs), summary["intercept_audit"])
    # crush's stats (/crush isn't reachable over rpc: its backing file)
    ext = base / "plugins" / "crush" / "target" / "release" / "crush-ext"
    r = subprocess.run([str(ext), "--report"], capture_output=True, text=True,
                       env={"PATH": "/usr/bin:/bin", "HOME": str(base)})
    summary["crush_report"] = r.stdout
    st = json.loads((base / ".cache" / "crush" / "stats.json").read_text())
    fired = st["total"]["fired"]
    check("crush stats: 2 fired (UNKNOWN repeats, LOG build_progress), 1 host-truncated pass-through",
          sum(x["fires"] for x in fired.values()) == 2 and any("build_progress" in k for k in fired)
          and any("repeats" in k for k in fired) and st["total"]["passed"].get("host-truncated") == 1,
          {"fired": list(fired), "passed": st["total"]["passed"]})
    check("/crush report (crush-ext --report) shows the savings", "~" in r.stdout and "build_progress" in r.stdout,
          r.stdout[:300])
    log = base / ".cache" / "synaps" / "api-log.jsonl"
    summary["model_calls"] = sum(1 for _ in log.open()) if log.exists() else None


if __name__ == "__main__":
    sys.exit(main())
