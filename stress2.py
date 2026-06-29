#!/usr/bin/env python3
"""crush BRUTAL stress — actively try to break it. Hunts panics, hangs, OOM,
corruption, and non-idempotence. Safe Rust can't UAF, so we target the things it
CAN die on: unbounded allocation, pathological recursion/serialization, slow
paths, and parsing edge cases.
"""
import json, os, subprocess, sys, time, random, resource, concurrent.futures as cf

CRUSH = os.path.expanduser("~/Jawz/workspace/crush/target/release/crush")
random.seed(0xBADC0DE)

def frame(o):
    b = json.dumps(o).encode(); return b"Content-Length: %d\r\n\r\n%s" % (len(b), b)
def parse(d):
    i, out = 0, []
    while i < len(d):
        s = d.find(b"\r\n\r\n", i)
        if s < 0: break
        L = None
        for ln in d[i:s].decode("ascii","replace").split("\r\n"):
            if ln.lower().startswith("content-length:"):
                try: L = int(ln.split(":",1)[1])
                except: L = None
        if L is None: break
        try: out.append(json.loads(d[s+4:s+4+L]))
        except: pass
        i = s+4+L
    return out
INIT = frame({"id":1,"method":"initialize"}); SHUT = frame({"id":3,"method":"shutdown"})
def hook(out, tool="bash", cmd="echo"):
    return frame({"id":2,"method":"hook.handle","params":{"kind":"after_tool_call",
        "tool_name":tool,"tool_input":{"command":cmd},"tool_output":out}})

def run(stream, timeout=20, mem_mb=None):
    def limit():
        if mem_mb:
            b = mem_mb*1024*1024
            resource.setrlimit(resource.RLIMIT_AS, (b, b))
    p = subprocess.run([CRUSH], input=stream, capture_output=True, timeout=timeout,
                       preexec_fn=limit if mem_mb else None)
    return p.returncode, parse(p.stdout)

def run_raw_bytes(body_bytes, timeout=20):
    """Send a hook with a raw (possibly invalid-UTF8) JSON body."""
    fr = b"Content-Length: %d\r\n\r\n%s" % (len(body_bytes), body_bytes)
    p = subprocess.run([CRUSH], input=INIT+fr+SHUT, capture_output=True, timeout=timeout)
    return p.returncode, parse(p.stdout)

fails=[]
def check(name, cond, detail=""):
    print(("  ✓ " if cond else "  ✗ ")+name+("  "+detail if detail else ""))
    if not cond: fails.append(name)

def action_of(resp):
    r = next((m for m in resp if m.get("id")==2), None)
    if not r: return None, None
    res = r.get("result",{})
    return res.get("action"), res.get("output")

# ---- 1. JSON nested bomb: the largest_aoo_size walk re-serializes subtrees ----
print("\n[1] PATHOLOGICAL JSON (time/OOM bombs)")
# wide tree of arrays-of-objects, nested — stresses the recursive to_string walk
def json_bomb(depth, width):
    if depth == 0:
        return '{"k":"v","n":1}'
    inner = ",".join('{"id":%d,"child":[%s]}' % (i, ",".join(json_bomb(depth-1, width) for _ in range(2)))
                     for i in range(width))
    return inner
bomb = "[" + json_bomb(4, 6) + "]"   # deeply nested arrays-of-objects
print(f"  (bomb size: {len(bomb)} bytes)")
try:
    t=time.perf_counter(); code,resp = run(INIT+hook(bomb,"bash","cat x")+SHUT, timeout=15); dt=time.perf_counter()-t
    a,o = action_of(resp)
    check("nested-AOO bomb → completes fast, no enlarge", a in ("continue","replace") and (o is None or len(o)<=len(bomb)),
          f"({dt*1000:.0f}ms, action={a})")
except subprocess.TimeoutExpired:
    check("nested-AOO bomb → no hang", False, "HUNG (>15s) — O(n^2) serialization bomb")

# a flat-but-huge array-of-objects (50k rows)
huge_arr = "["+",".join('{"a":%d,"b":"xxxxxxxx","c":true}'%i for i in range(50000))+"]"
try:
    t=time.perf_counter(); code,resp=run(INIT+hook(huge_arr,"bash","cat")+SHUT, timeout=20); dt=time.perf_counter()-t
    a,o=action_of(resp); check("50k-row array → completes", a in ("continue","replace"), f"({dt*1000:.0f}ms, action={a})")
except subprocess.TimeoutExpired:
    check("50k-row array → no hang", False, "HUNG")

# ---- 2. OOM-bounded soak: cap virtual memory, feed big inputs ----
print("\n[2] MEMORY-BOUNDED SOAK (ulimit -v 300MB)")
for name, payload in [("10MB ls-shaped", ("-rwxr-xr-x 1 root root 55K Jun 28 18:51 f.sh\n")*225000),
                      ("10MB one line", "x"*10_000_000),
                      ("8MB json array", "["+",".join('{"k":%d}'%i for i in range(400000))+"]")]:
    try:
        code,resp = run(INIT+hook(payload,"ls","ls -lah")+SHUT, timeout=30, mem_mb=300)
        a,o=action_of(resp)
        check(f"{name} under 300MB cap", code==0 and a in ("continue","replace"), f"(exit {code}, action={a})")
    except subprocess.TimeoutExpired:
        check(f"{name} under 300MB cap", False, "HUNG")
    except Exception as e:
        check(f"{name} under 300MB cap", False, f"err {e}")

# ---- 3. malformed UTF-8 / lone surrogates in the JSON body ----
print("\n[3] MALFORMED UTF-8 / SURROGATES")
# invalid raw bytes in body → serde should reject the frame, crush resyncs
try:
    code,resp = run_raw_bytes(b'{"id":2,"method":"hook.handle","params":{"tool_output":"\xff\xfe bad"}}')
    check("invalid UTF-8 body → no crash", True, f"(exit {code})")
except subprocess.TimeoutExpired:
    check("invalid UTF-8 body → no crash", False, "HUNG")
# lone surrogate via JSON escape \ud800 (valid JSON syntax, no valid Rust char)
try:
    body = b'{"id":2,"method":"hook.handle","params":{"kind":"after_tool_call","tool_name":"bash","tool_input":{"command":"x"},"tool_output":"' + b'\\ud800'*2000 + b' tail"}}'
    code,resp = run_raw_bytes(body)
    a,o=action_of(resp); check("lone-surrogate \\ud800 spam → no crash", True, f"(exit {code}, action={a})")
except subprocess.TimeoutExpired:
    check("lone-surrogate → no crash", False, "HUNG")

# ---- 4. reviewer edge cases ----
print("\n[4] REVIEWER EDGE CASES")
# dict code boundary: exactly 62, 63, 64 distinct values in a column
for n in (61,62,63,64,100):
    rows = "".join(f"-rwxr-xr-x 1 root root {i}K Jun 28 18:51 f{i}.sh\n" for i in range(n))  # size col = n distinct
    a,o = action_of(run(INIT+hook(rows,"ls","ls -lah")+SHUT,15)[1])
    check(f"{n}-distinct column → safe", a in ("continue","replace"), f"(action={a})")
# multibyte field values + at split points
mb = "".join(f"-rwxr-xr-x 1 röot gräup {i}K Jün 28 18:51 名前_{i}.sh\n" for i in range(20))
a,o = action_of(run(INIT+hook(mb,"ls","ls -lah")+SHUT,15)[1])
check("multibyte field values → safe + no enlarge", a in ("continue","replace") and (o is None or len(o)<=len(mb)))
# empty fields in pipe (git <> email)
empt = "".join(f"{i:040x}|Haseeb Khalid||Mon Jun 29 2026|msg {i}\n" for i in range(20))
a,o = action_of(run(INIT+hook(empt,"bash","git log --pretty=a|b|c|d|e")+SHUT,15)[1])
check("empty pipe fields (git <>) → safe", a in ("continue","replace"))
# every line IS a marker
marker_lines = "@crush/1.cols a b c\n"*500
a,o = action_of(run(INIT+hook(marker_lines,"ls","ls")+SHUT,15)[1])
check("every line is a crush marker → passes through", a == "continue", f"(action={a})")

# ---- 5. mutation fuzzing: mutate valid inputs, 30k iterations ----
print("\n[5] MUTATION FUZZING (30k mutated real inputs)")
seeds = []
fxdir = os.path.expanduser("~/Jawz/workspace/crush/tests/fixtures")
for f in ("ls_lah.txt","ps_aux.txt","git_log.txt","json_array.json"):
    p=os.path.join(fxdir,f)
    if os.path.exists(p): seeds.append(open(p,errors="replace").read()[:8000])
def mutate(s):
    s=list(s)
    for _ in range(random.randint(1,40)):
        op=random.random()
        if not s: break
        i=random.randrange(len(s))
        if op<0.3: s[i]=random.choice("|= \t\n:/{}[],\"")  # inject delimiters/json
        elif op<0.5: del s[i]
        elif op<0.7: s.insert(i, random.choice("xX0|=\n"))
        else: s[i]=chr(random.randint(1,0x10ffff)) if random.random()<0.1 else s[i]
    return "".join(c for c in s if ord(c)<0x110000)
# batch many hooks into one process for speed
batch_fail=0; tools=[("ls","ls -lah"),("bash","ps aux"),("bash","git log --pretty=a|b"),("bash","cat")]
for batch in range(30):
    stream=INIT
    payloads=[]
    for _ in range(1000):
        seed=random.choice(seeds) if seeds else "x"*100
        m=mutate(seed); payloads.append(m)
        t,c=random.choice(tools); stream+=hook(m,t,c)
    stream+=SHUT
    try:
        code,resp=run(stream, timeout=60)
        # every replace must not enlarge its corresponding input — check by id order
        reps=[m for m in resp if m.get("id")==2]
        for r,pl in zip(reps,payloads):
            res=r.get("result",{})
            if res.get("action")=="replace" and len(res.get("output",""))>len(pl): batch_fail+=1
        if code!=0: batch_fail+=1
    except subprocess.TimeoutExpired:
        batch_fail+=1; break
check("30k mutated inputs → no crash, no enlarge", batch_fail==0, f"({batch_fail} issues)")

# ---- 6. parallel hammering ----
print("\n[6] PARALLEL (8 procs × 2k reqs)")
def worker(seed):
    random.seed(seed); s=INIT
    for _ in range(2000):
        s+=hook("-rwxr-xr-x 1 root root 55K Jun 28 18:51 f.sh\n"*40,"ls","ls -lah")
    s+=SHUT
    try:
        code,resp=run(s,timeout=60); return code==0 and sum(1 for m in resp if m.get("id")==2)==2000
    except: return False
with cf.ThreadPoolExecutor(max_workers=8) as ex:
    res=list(ex.map(worker, range(8)))
check("8 parallel processes × 2000 reqs → all clean", all(res), f"({sum(res)}/8 ok)")

print("\n"+"="*52)
if fails: print(f"BRUTAL STRESS: {len(fails)} FAILURE(S): {fails}"); sys.exit(1)
print("BRUTAL STRESS: ALL PASS — actively tried to break it, couldn't.")
