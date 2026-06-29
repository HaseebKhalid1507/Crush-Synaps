#!/usr/bin/env python3
"""crush vs headroom — honest, apples-to-apples compression measurement.

Frames each fixture as a real after_tool_call hook event, pipes it through the
crush binary AND the headroom-bridge Python extension over the same JSON-RPC
protocol, and reports what each actually returns to the model (replace=compressed,
continue=unchanged). Real bytes. No synthetic numbers.
"""
import glob
import json
import os
import subprocess
import sys

CRUSH = os.path.expanduser("~/Jawz/workspace/crush/target/release/crush")
HR_DIR = os.path.expanduser("~/Jawz/workspace/headroom-bridge")
HR_PY = os.path.join(HR_DIR, ".venv/bin/python")
HR_MAIN = os.path.join(HR_DIR, "main.py")
FIXDIR = os.path.expanduser("~/Jawz/workspace/crush/tests/fixtures")

# Production reality: post-Fork-1 the engine caps tool output at max_tool_buffer
# (256 KiB) before the hook sees it. Measure what crush actually receives.
MAX_BUFFER = 256 * 1024


def frame(obj):
    body = json.dumps(obj).encode("utf-8")
    return b"Content-Length: %d\r\n\r\n%s" % (len(body), body)


def parse_frames(data):
    """Yield JSON objects from a Content-Length framed byte stream."""
    i = 0
    while i < len(data):
        # find header end
        sep = data.find(b"\r\n\r\n", i)
        if sep == -1:
            break
        header = data[i:sep].decode("ascii", "replace")
        length = None
        for line in header.split("\r\n"):
            if line.lower().startswith("content-length:"):
                length = int(line.split(":", 1)[1].strip())
        if length is None:
            break
        start = sep + 4
        body = data[start:start + length]
        try:
            yield json.loads(body)
        except Exception:
            pass
        i = start + length


def run_ext(cmd, output):
    """Pipe one after_tool_call through an extension; return the bytes the model
    would receive (compressed if replace, original if continue), or None on error."""
    stream = (
        frame({"jsonrpc": "2.0", "id": 1, "method": "initialize"})
        + frame({"jsonrpc": "2.0", "id": 2, "method": "hook.handle",
                 "params": {"kind": "after_tool_call",
                            "tool_input": {"command": "x"},
                            "tool_output": output}})
        + frame({"jsonrpc": "2.0", "id": 3, "method": "shutdown"})
    )
    try:
        p = subprocess.run(cmd, input=stream, capture_output=True, timeout=120)
    except Exception as e:
        return None, f"err:{e}"
    for msg in parse_frames(p.stdout):
        if msg.get("id") == 2:
            result = msg.get("result", {})
            action = result.get("action")
            if action == "replace":
                return result.get("output", ""), "replace"
            return output, "continue"
    return output, "no-resp"


def main():
    fixtures = sorted(glob.glob(os.path.join(FIXDIR, "*")))
    fixtures = [f for f in fixtures if os.path.isfile(f)]
    have_hr = os.path.exists(HR_PY) and os.path.exists(HR_MAIN)

    rows = []
    for path in fixtures:
        name = os.path.basename(path)
        with open(path, "r", errors="replace") as fh:
            raw = fh.read()
        # simulate the production 256 KiB pre-hook cap
        output = raw[:MAX_BUFFER]
        before = len(output)
        if before < 512:
            continue

        c_out, c_act = run_ext([CRUSH], output)
        c_after = len(c_out) if c_out is not None else before
        c_ratio = 100 - (c_after * 100 // before)

        if have_hr:
            h_out, h_act = run_ext([HR_PY, HR_MAIN], output)
            h_after = len(h_out) if h_out is not None else before
            h_ratio = 100 - (h_after * 100 // before)
        else:
            h_after, h_ratio, h_act = before, 0, "n/a"

        rows.append((name, before, c_after, c_ratio, c_act, h_after, h_ratio, h_act))

    # ---- report ----
    print(f"{'fixture':<22} {'bytes':>9} | {'crush':>9} {'saved':>6} {'act':>8} | "
          f"{'headroom':>9} {'saved':>6} {'act':>8}")
    print("-" * 96)
    tot_before = tot_crush = tot_hr = 0
    for (name, before, c_after, c_ratio, c_act, h_after, h_ratio, h_act) in rows:
        tot_before += before
        tot_crush += c_after
        tot_hr += h_after
        print(f"{name:<22} {before:>9} | {c_after:>9} {c_ratio:>5}% {c_act:>8} | "
              f"{h_after:>9} {h_ratio:>5}% {h_act:>8}")
    print("-" * 96)
    if tot_before:
        cr = 100 - (tot_crush * 100 // tot_before)
        hr = 100 - (tot_hr * 100 // tot_before)
        print(f"{'TOTAL':<22} {tot_before:>9} | {tot_crush:>9} {cr:>5}% {'':>8} | "
              f"{tot_hr:>9} {hr:>5}% {'':>8}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
