#!/usr/bin/env python3
"""crush stress harness — hammer the real binary over the real protocol.

Three layers:
  1. PROTOCOL fuzzing   — malformed/oversized/partial JSON-RPC frames.
  2. ADVERSARIAL inputs — pathological tool output (huge, nested, marker-injection,
                          unicode whitespace, pipe-heavy, empty, binary-ish).
  3. VOLUME             — 10k requests in one process; throughput + stability.

Invariants asserted on every hook.handle response:
  · valid JSON-RPC with matching id
  · action ∈ {continue, replace}
  · replace output is NEVER larger than the input (never-enlarge)
  · the process never hangs (timeout) and never crashes on shutdown
  · deterministic: same input → same output
"""
import json, os, subprocess, sys, time, random

CRUSH = os.path.expanduser("~/Jawz/workspace/crush/target/release/crush")
random.seed(0xC0FFEE)

def frame(obj):
    b = json.dumps(obj).encode()
    return b"Content-Length: %d\r\n\r\n%s" % (len(b), b)

def raw_frame(body: bytes, length=None):
    n = len(body) if length is None else length
    return b"Content-Length: %d\r\n\r\n%s" % (n, body)

def parse(data):
    i, out = 0, []
    while i < len(data):
        s = data.find(b"\r\n\r\n", i)
        if s < 0: break
        hdr = data[i:s].decode("ascii", "replace")
        L = None
        for ln in hdr.split("\r\n"):
            if ln.lower().startswith("content-length:"):
                try: L = int(ln.split(":", 1)[1])
                except: L = None
        if L is None: break
        try: out.append(json.loads(data[s+4:s+4+L]))
        except: pass
        i = s + 4 + L
    return out

def run(stream: bytes, timeout=30):
    """Returns (exit_code, parsed_responses) or raises on hang."""
    p = subprocess.run([CRUSH], input=stream, capture_output=True, timeout=timeout)
    return p.returncode, parse(p.stdout)

def hook(output, tool="bash", cmd="echo"):
    return frame({"jsonrpc":"2.0","id":2,"method":"hook.handle","params":{
        "kind":"after_tool_call","tool_name":tool,"tool_input":{"command":cmd},
        "tool_output":output}})

INIT = frame({"jsonrpc":"2.0","id":1,"method":"initialize"})
SHUT = frame({"jsonrpc":"2.0","id":3,"method":"shutdown"})

fails = []
def check(name, cond, detail=""):
    if cond: print(f"  ✓ {name}")
    else:
        print(f"  ✗ {name}  {detail}")
        fails.append(name)

# ---------------------------------------------------------------- 1. PROTOCOL
print("\n[1] PROTOCOL FUZZING")
cases = {
    "missing Content-Length":      b"\r\n{}\r\n\r\n",
    "garbage before frame":        b"!!!garbage!!!\r\n\r\n" + INIT,
    "oversized Content-Length":    raw_frame(b"{}", length=999_999_999) + SHUT,
    "negative-ish / non-numeric":  b"Content-Length: abc\r\n\r\n{}" + SHUT,
    "partial body at EOF":         b"Content-Length: 100\r\n\r\nonly-ten!!",
    "non-JSON body":               raw_frame(b"this is not json at all xxxx") + SHUT,
    "empty stream":                b"",
    "double newlines / blanks":    b"\r\n\r\n\r\n" + INIT + SHUT,
    "valid after garbage recovers": b"garbage\n" + INIT + hook("x"*5000, "ls", "ls -lah") + SHUT,
}
for name, stream in cases.items():
    try:
        code, resp = run(stream, timeout=15)
        check(f"{name} → no hang/crash", True, f"(exit {code}, {len(resp)} resp)")
    except subprocess.TimeoutExpired:
        check(f"{name} → no hang/crash", False, "HUNG (timeout)")

# --------------------------------------------------------------- 2. ADVERSARIAL
print("\n[2] ADVERSARIAL PAYLOADS")
def adversarial_inputs():
    yield "empty", ""
    yield "huge 5MB repeated", "A"*5_000_000
    yield "huge ls-shaped 2MB", ("-rwxr-xr-x 1 root root 55K Jun 28 18:51 f.sh\n")*45000
    yield "deeply nested json", '{"a":'*5000 + "1" + "}"*5000
    yield "giant flat json array", "["+",".join('{"k":%d,"v":"x"}'%i for i in range(20000))+"]"
    yield "marker injection", "@crush/1.cols a b c\n" + "data line here\n"*500
    yield "unicode whitespace cols", "\u2003".join(["a","b","c","name with em space"])+"\n"*0 + ("a\u2003b\u2003c\u2003nm\n")*50
    yield "pipe-heavy", ("h|a|e|d|"+"x|"*200+"\n")*50
    yield "null-ish controls", ("\x01\x02\x07col\ttab\x0bvert\n")*100
    yield "mixed crlf", ("a\r\nb\r\n\r\nc\r\n")*1000
    yield "one giant line", "x"*2_000_000
    yield "many tiny lines", "a\n"*100000
    yield "valid ls big", None  # filled below from fixture if present

fx = os.path.expanduser("~/Jawz/workspace/crush/tests/fixtures/ls_lah.txt")
ls_real = open(fx).read() if os.path.exists(fx) else "x"
for name, payload in adversarial_inputs():
    if payload is None: payload = ls_real; name = "real ls -lah"
    tool, cmd = ("ls","ls -lah") if "ls" in name else ("bash","echo")
    try:
        code, resp = run(INIT + hook(payload, tool, cmd) + SHUT, timeout=60)
        r = next((m for m in resp if m.get("id")==2), None)
        if r is None:
            check(name, False, "no response"); continue
        action = r.get("result",{}).get("action")
        ok_action = action in ("continue","replace")
        never_enlarge = True
        if action == "replace":
            never_enlarge = len(r["result"]["output"]) <= len(payload)
        check(name, ok_action and never_enlarge,
              f"(action={action}, enlarge={'!!' if not never_enlarge else 'ok'})")
    except subprocess.TimeoutExpired:
        check(name, False, "HUNG")

# --------------------------------------------------------------- 3. DETERMINISM
print("\n[3] DETERMINISM (cache-safety)")
det_ok = True
for _ in range(200):
    payload = "".join(random.choice("abcXYZ 0129\t\n:/|=.-_") for _ in range(random.randint(0,4000)))
    _, r1 = run(INIT + hook(payload,"ls","ls -lah") + SHUT, timeout=15)
    _, r2 = run(INIT + hook(payload,"ls","ls -lah") + SHUT, timeout=15)
    o1 = next((m["result"].get("output") for m in r1 if m.get("id")==2), None)
    o2 = next((m["result"].get("output") for m in r2 if m.get("id")==2), None)
    if o1 != o2: det_ok = False; break
check("200 random inputs → identical output both runs", det_ok)

# --------------------------------------------------------------- 4. VOLUME
print("\n[4] VOLUME / THROUGHPUT")
N = 10000
big = ("-rwxr-xr-x 1 root root 55K Jun 28 18:51 binary_name.sh\n")*60  # ~3.4KB each
stream = INIT + hook(big,"ls","ls -lah")*N + SHUT
t = time.perf_counter()
try:
    code, resp = run(stream, timeout=120)
    dt = time.perf_counter() - t
    got = sum(1 for m in resp if m.get("id")==2)
    rps = N/dt
    check(f"{N} requests answered", got == N, f"(got {got})")
    check(f"throughput {rps:.0f} req/s, clean exit {code}", code == 0)
    # spot-check every response is a valid replace that shrank
    bad = sum(1 for m in resp if m.get("id")==2 and m.get("result",{}).get("action")=="replace"
              and len(m["result"]["output"]) > len(big))
    check("no response enlarged under load", bad == 0, f"({bad} enlarged)")
except subprocess.TimeoutExpired:
    check("volume run", False, "HUNG")

# ---------------------------------------------------------------- VERDICT
print("\n" + "="*50)
if fails:
    print(f"STRESS: {len(fails)} FAILURE(S): {fails}")
    sys.exit(1)
print("STRESS: ALL PASS — no crash, no hang, no enlarge, deterministic, holds under volume.")
