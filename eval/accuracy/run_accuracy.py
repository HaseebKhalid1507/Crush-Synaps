#!/usr/bin/env python3
"""Raw-vs-crushed reading accuracy, driven through Synaps headless (`synaps rpc`).

For each selected sample (crush fired on it), the same auto-generated
questions are asked about the RAW output and about the CRUSHED output (exactly
what crush v0.1 would hand the model), in separate fresh sessions, on each
model. A seeded subset also gets a second RAW ask: the raw-vs-raw disagreement
is the noise floor.

Isolation (nothing of the user's Synaps is touched):
  - SYNAPS_BASE_DIR *and* HOME point at a private temp dir under
    $CRUSH_CORPUS_DIR (api-log / trace live under ~/.cache/synaps, so HOME must
    move too); config = model + thinking off + telemetry full + every tool
    disabled; no plugins/extensions/MCP/memory.
  - auth.json there holds ONLY the current Anthropic access token (empty
    refresh), so this process can never rotate the real refresh token. If the
    token is close to expiry we stop.
  - `synaps rpc` doesn't use the session daemon. The temp dir is deleted after.
  - The real ~/.synaps-cli/auth.json is hashed before/after; a change aborts.

Budget: --max-calls (default 400) model API requests per run, counted from the
isolated api-log (includes retries). The plan is shrunk to fit before starting.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import queue
import random
import re
import shutil
import subprocess
import sys
import threading
import time
from collections import Counter, defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "lib"))
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE))
from common import CORPUS_DIR, OUT, private_dir, read_jsonl, write_jsonl_private  # noqa: E402
from questions import fair_for, gen_questions, score  # noqa: E402
from run_crush import load_samples, results_path  # noqa: E402

MODELS = {"haiku": "claude-haiku-4-5-20251001", "sonnet": "claude-sonnet-4-6"}
# list prices, $/MTok: input, output, cache read, 5m cache write, 1h cache write
PRICES = {"claude-haiku-4-5-20251001": (1.0, 5.0, 0.10, 1.25, 2.0),
          "claude-sonnet-4-6": (3.0, 15.0, 0.30, 3.75, 6.0)}
SYSTEM = ("You answer questions about the output of a tool call that was run during a software task. "
          "Use only the output shown. Never call tools. Be exact: copy values as they appear.")
ALL_TOOLS = ("activate_tools,bash,edit,find,forum_forget,forum_post,forum_read,grep,ls,memory_context,memory_fetch,"
             "memory_forget,memory_search,memory_store,read,search_tools,shell_end,shell_send,shell_start,subagent,"
             "subagent_collect,subagent_model_authorize,subagent_models,subagent_resume,subagent_start,subagent_status,"
             "subagent_steer,write")
REAL_AUTH = Path.home() / ".synaps-cli" / "auth.json"
MAX_PROMPT = 400_000  # rpc frames are capped at 1 MiB
DROPPED: dict[str, int] = {}
# a separate accuracy run (CRUSH_EVAL_ACC_TAG, e.g. a phase's candidates)
# keeps its own summary next to the main one
ACC_TAG = os.environ.get("CRUSH_EVAL_ACC_TAG", "")
SUMMARY = f"accuracy.summary.{ACC_TAG}.json" if ACC_TAG else "accuracy.summary.json"
FXREC = f"accuracy.fixtures.{ACC_TAG}.jsonl" if ACC_TAG else "accuracy.fixtures.jsonl"
QUOTA: dict[str, int] = {}
LABELS: set[str] = set()  # --labels: only samples one of these transforms fired on
COLLAPSE = ("test_pass", "build_progress")


def auth_digest() -> str:
    try:
        return hashlib.sha256(REAL_AUTH.read_bytes()).hexdigest()
    except FileNotFoundError:
        return "missing"


class Headless:
    def __init__(self, model: str):
        self.model = model
        self.base = private_dir(CORPUS_DIR / f"sb{os.getpid()}{model[7:12]}")
        real = json.loads(REAL_AUTH.read_text())["anthropic"]
        left = real.get("expires", 0) / 1000 - time.time()
        if left < 1800:
            raise SystemExit(f"Anthropic access token expires in {int(left)}s; let Synaps refresh it "
                             "(e.g. `synaps status`) and rerun. This harness never refreshes tokens.")
        fd = os.open(self.base / "auth.json", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        os.write(fd, json.dumps({"anthropic": {"type": "oauth", "refresh": "", "access": real["access"],
                                               "expires": real["expires"]}}).encode())
        os.close(fd)
        (self.base / "config").write_text(f"model = {model}\nthinking = off\ntelemetry = full\n"
                                          f"disabled_tools = {ALL_TOOLS}\n")
        env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": str(self.base), "SYNAPS_BASE_DIR": str(self.base),
               "LANG": "C.UTF-8"}
        self.p = subprocess.Popen(["synaps", "rpc", "-m", model, "-s", SYSTEM], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env, text=True,
                                  encoding="utf-8", cwd=self.base)
        self.q: queue.Queue = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        ready = self._wait(lambda e: e.get("type") == "ready", 60)
        if not ready:
            raise SystemExit("synaps rpc did not become ready")
        self.n = 0

    def _read(self):
        for line in self.p.stdout:
            try:
                self.q.put(json.loads(line))
            except Exception:
                pass
        self.q.put(None)

    def _wait(self, pred, timeout, sink=None):
        end = time.time() + timeout
        while time.time() < end:
            try:
                e = self.q.get(timeout=max(0.1, end - time.time()))
            except queue.Empty:
                break
            if e is None:
                return None
            if sink is not None:
                sink(e)
            if pred(e):
                return e
        return None

    def send(self, obj):
        self.p.stdin.write(json.dumps(obj) + "\n")
        self.p.stdin.flush()

    def ask(self, prompt: str, timeout: int = 240) -> dict:
        self.n += 1
        sid, pid = f"n{self.n}", f"p{self.n}"
        self.send({"type": "new_session", "id": sid})
        self._wait(lambda e: e.get("type") == "response" and e.get("id") == sid, 30)
        text, tools, usage, errors = [], 0, None, []

        def sink(e):
            nonlocal tools, usage
            if e.get("type") == "message_update":
                ev = e.get("event", {})
                if ev.get("type") == "text_delta":
                    text.append(ev.get("delta", ""))
                elif ev.get("type") == "toolcall_start":
                    tools += 1
                elif ev.get("type") == "response_reset":
                    text.clear()
            elif e.get("type") == "agent_end":
                usage = e.get("usage")
            elif e.get("type") == "error":
                errors.append(e.get("message", "")[:200])
        t = time.time()
        self.send({"type": "prompt", "id": pid, "message": prompt})
        r = self._wait(lambda e: e.get("type") == "response" and e.get("id") == pid, timeout, sink)
        return {"text": "".join(text), "ok": bool(r and r.get("ok")), "tool_calls": tools, "usage": usage,
                "errors": errors, "secs": round(time.time() - t, 2)}

    def api_log(self) -> list[dict]:
        p = self.base / ".cache" / "synaps" / "api-log.jsonl"
        return list(read_jsonl(p)) if p.exists() else []

    def close(self):
        try:
            self.p.stdin.write(json.dumps({"type": "shutdown"}) + "\n")
            self.p.stdin.flush()
            self.p.wait(timeout=20)
        except Exception:
            self.p.kill()
        log = self.api_log()
        shutil.rmtree(self.base, ignore_errors=True)
        return log


def build_prompt(sample: dict, text: str, qs: list[dict]) -> str:
    tool = sample["tool"]
    inp = sample.get("input") or {}
    if isinstance(inp, dict) and isinstance(inp.get("command"), str):
        head = f'<tool_call tool="{tool}">\n{inp["command"][:2000]}\n</tool_call>'
    else:
        head = f'<tool_call tool="{tool}">\n{json.dumps(inp)[:2000]}\n</tool_call>'
    qtext = "\n".join(f"{i + 1}. {x['q']}" for i, x in enumerate(qs))
    return (f"{head}\n<tool_output>\n{text}\n</tool_output>\n\n"
            "Answer the questions below using only the tool output above. Reply with exactly one line per "
            "question, in the form `N: answer`, and nothing else. If the output does not let you determine "
            "an answer, write `N: unknown`.\n\n" + qtext)


def parse_answers(text: str, n: int) -> list[str | None]:
    got: dict[int, str] = {}
    for line in text.split("\n"):
        m = re.match(r"^\s*\**(\d+)\**\s*[:.)]\s*(.*)$", line)
        if m and 1 <= int(m.group(1)) <= n and int(m.group(1)) not in got:
            got[int(m.group(1))] = m.group(2).strip()
    return [got.get(i + 1) for i in range(n)]


def select(which: str, n_corpus: int, seed: int, max_q: int):
    samples = {s["id"]: s for s in load_samples(which)}
    res = {r["id"]: r for r in read_jsonl(results_path(which))}
    for i, r in res.items():
        # crush-core's class decides what ran; keep the label with the sample
        samples[i]["class"] = r["class"]
        samples[i]["label"] = r["label"]
    cands = []
    for i, r in res.items():
        if not r["fired"]:
            continue
        s = samples[i]
        if len(s["output"]) > MAX_PROMPT or len(r["output"]) > MAX_PROMPT:
            continue
        if LABELS and not any(t in LABELS for t in (r["label"] or "").split("+")):
            continue
        qs, dropped = fair_for(gen_questions(s, max_q), r["output"])
        DROPPED[which] = DROPPED.get(which, 0) + dropped
        # a question quoting text crush cut (moved to the raw file) is still
        # asked, but marked: it measures what was moved out, not misreading
        for x in qs:
            if x.get("needle") and x["needle"] not in r["output"]:
                x["kind"] = "cut:" + x["kind"]
                x["cut"] = True
        if any(t in COLLAPSE for t in (r["label"] or "").split("+")):
            # "quote the next line" whose answer is a line the LOG collapse
            # removed (a passing test / progress line, counted in a note)
            # asks for dropped text verbatim; what it carried is probed
            # directly (test:status_run, build:ncompiled/ncollected)
            flat = re.sub(r"[ \t]+", " ", r["output"])
            keep = [x for x in qs if x["kind"] not in ("lines:next", "lines:gap")
                    or re.sub(r"[ \t]+", " ", x["answer"]) in flat]
            DROPPED[f"{which}:collapsed"] = DROPPED.get(f"{which}:collapsed", 0) + len(qs) - len(keep)
            qs = keep
        if qs:
            cands.append((s, r["output"], qs))
    if which == "fixtures":
        return cands
    # stratified round-robin over (class, label); with QUOTA, per-label quotas
    # first (round-robin over classes inside a label), then the rest
    rng = random.Random(seed)
    strata = defaultdict(list)
    for c in cands:
        strata[(c[0]["class"], res[c[0]["id"]]["label"])].append(c)
    for k in sorted(strata):
        rng.shuffle(strata[k])
    picked = []
    for label, want in QUOTA.items():
        keys = sorted(k for k in strata if k[1] == label)
        got = 0
        while got < want and any(strata[k] for k in keys):
            for k in keys:
                if strata[k] and got < want:
                    picked.append(strata[k].pop())
                    got += 1
    keys = sorted(strata)
    while len(picked) < n_corpus and any(strata.values()):
        for k in keys:
            if strata[k] and len(picked) < n_corpus:
                picked.append(strata[k].pop())
    return picked[:n_corpus]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", default="haiku,sonnet")
    ap.add_argument("--sets", default="fixtures,corpus")
    ap.add_argument("--corpus-samples", type=int, default=60)
    ap.add_argument("--replicate", type=float, default=0.25, help="fraction of samples asked RAW twice (noise floor)")
    ap.add_argument("--max-q", type=int, default=6)
    ap.add_argument("--max-calls", type=int, default=400, help="hard cap on model API requests for the whole run")
    ap.add_argument("--seed", type=int, default=3)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--only-samples", default="", help="comma list of sample ids/names to ask, or @file with one id per line")
    ap.add_argument("--quota", default="", help="per-transform corpus quotas, e.g. search_group:44,repeats:16")
    ap.add_argument("--labels", default="", help="only ask about samples one of these transforms fired on (comma list)")
    ap.add_argument("--merge", action="store_true", help="merge into the previous run's records/summary instead of replacing")
    ap.add_argument("--resummarize", action="store_true", help="rebuild the summary from stored records (no model calls)")
    args = ap.parse_args()
    if args.resummarize:
        prev = json.loads((OUT / SUMMARY).read_text())
        recs = list(read_jsonl(OUT / FXREC)) + list(read_jsonl(CORPUS_DIR / "accuracy" / f"{prev['run']}.jsonl"))
        prev.update(summarize(recs))
        (OUT / SUMMARY).write_text(json.dumps(prev, indent=1))
        print(json.dumps(prev["by_model_set"], indent=1))
        return 0
    models = [MODELS.get(m, m) for m in args.models.split(",")]
    LABELS.update(filter(None, args.labels.split(",")))
    for kv in filter(None, args.quota.split(",")):
        k, v = kv.split(":")
        QUOTA[k] = int(v)
    sets = args.sets.split(",")
    rng = random.Random(args.seed)

    plan_sets = {w: select(w, args.corpus_samples, args.seed, args.max_q) for w in sets}
    if args.only_samples:
        spec = args.only_samples
        only = set(Path(spec[1:]).read_text().split()) if spec.startswith("@") else set(spec.split(","))
        plan_sets = {w: [c for c in v if c[0]["id"] in only] for w, v in plan_sets.items()}

    def n_calls(ps):
        return sum(len(v) * 2 + int(round(len(v) * args.replicate)) for v in ps.values()) * len(models)
    # shrink the corpus slice until the plan fits the cap
    while n_calls(plan_sets) > args.max_calls and plan_sets.get("corpus"):
        plan_sets["corpus"] = plan_sets["corpus"][:-1]
    planned = n_calls(plan_sets)
    nq = {w: sum(len(c[2]) for c in v) for w, v in plan_sets.items()}
    print(f"plan: models={models} samples={ {w: len(v) for w, v in plan_sets.items()} } questions={nq} "
          f"planned_calls={planned} cap={args.max_calls}", file=sys.stderr)
    if planned > args.max_calls:
        sys.exit("plan exceeds --max-calls even without corpus samples")
    if args.dry_run:
        return 0

    jobs = []
    for w, cands in plan_sets.items():
        rep = set(rng.sample(range(len(cands)), int(round(len(cands) * args.replicate))))
        for k, (s, crushed, qs) in enumerate(cands):
            jobs.append((w, s, "raw", s["output"], qs))
            jobs.append((w, s, "crushed", crushed, qs))
            if k in rep:
                jobs.append((w, s, "raw2", s["output"], qs))
    before = auth_digest()
    records = []
    calls_used = 0
    totals = {}
    for model in models:
        order = jobs[:]
        random.Random(f"{args.seed}:{model}").shuffle(order)
        h = Headless(model)
        try:
            for j, (w, s, variant, text, qs) in enumerate(order):
                used_now = calls_used + len(h.api_log())
                if used_now >= args.max_calls:
                    print(f"  budget cap reached ({used_now})", file=sys.stderr)
                    break
                r = h.ask(build_prompt(s, text, qs))
                ans = parse_answers(r["text"], len(qs))
                for x, a in zip(qs, ans):
                    records.append({"set": w, "model": model, "sample": s["id"], "class": s["class"], "label": s.get("label"),
                                    "variant": variant, "kind": x["kind"], "q": x["q"], "want": x["answer"],
                                    "got": a, "correct": score(x, a), "tool_calls": r["tool_calls"], "ok": r["ok"],
                                    "cut": bool(x.get("cut"))})
                if (j + 1) % 10 == 0:
                    print(f"  {model}: {j + 1}/{len(order)} asks", file=sys.stderr)
                if auth_digest() != before:
                    raise SystemExit("real auth.json changed during the run; stopping (not caused by this harness "
                                     "unless proven otherwise)")
        finally:
            log = h.close()
        calls_used += len(log)
        u = Counter()
        for e in log:
            for k, v in (e.get("usage") or {}).items():
                if isinstance(v, (int, float)) and k != "hit_pct":
                    u[k] += v
        pr = PRICES.get(model)
        cost = None
        if pr:
            cost = (u["input"] * pr[0] + u["output"] * pr[1] + u["cache_read"] * pr[2] + u["cache_write_5m"] * pr[3]
                    + u["cache_write_1h"] * pr[4]) / 1e6
        totals[model] = {"api_requests": len(log), "usage": dict(u), "cost_usd_list": cost,
                         "stop_reasons": dict(Counter(e.get("stop_reason") for e in log))}
    stamp = time.strftime("%Y%m%d-%H%M%S")
    prev = json.loads((OUT / SUMMARY).read_text()) if args.merge and (OUT / SUMMARY).exists() else None
    if prev:
        redo = {(r["set"], r["sample"]) for r in records}
        old_fx = list(read_jsonl(OUT / FXREC)) if (OUT / FXREC).exists() else []
        old_cp = []
        for rn in prev.get("runs", [prev.get("run")]):
            f = CORPUS_DIR / "accuracy" / f"{rn}.jsonl"
            if f.exists():
                old_cp += list(read_jsonl(f))
        records = [r for r in old_fx + old_cp if (r["set"], r["sample"]) not in redo] + records
        for m, t in prev.get("per_model", {}).items():
            if m in totals:
                for k in ("api_requests",):
                    totals[m][k] += t[k]
                totals[m]["cost_usd_list"] = (totals[m]["cost_usd_list"] or 0) + (t["cost_usd_list"] or 0)
                u = Counter(totals[m]["usage"])
                u.update(t["usage"])
                totals[m]["usage"] = dict(u)
            else:
                totals[m] = t
        calls_used += prev.get("calls_used", 0)
        planned += prev.get("planned_calls", 0)
        for k, v in prev.get("questions_dropped_unfair", {}).items():
            if not args.only_samples:
                DROPPED[k] = DROPPED.get(k, 0) + v
            else:
                DROPPED.setdefault(k, v)
    # private per-answer records for real data; fixture records are committed-safe
    write_jsonl_private(CORPUS_DIR / "accuracy" / f"{stamp}.jsonl", [r for r in records if r["set"] == "corpus"])
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / FXREC).write_text("".join(json.dumps(r) + "\n" for r in records if r["set"] == "fixtures"))
    summary = summarize(records)
    # with --merge the new private records file holds old + new corpus records,
    # so it alone is the source for the next merge
    summary.update({"run": stamp, "runs": [stamp], "merged_runs": (prev or {}).get("merged_runs", [(prev or {}).get("run")] if prev else []) + [stamp], "questions_dropped_unfair": DROPPED, "models": models,
                    "planned_calls": planned, "cap": args.max_calls,
                    "calls_used": calls_used, "per_model": totals,
                    "cost_usd_list_total": sum(t["cost_usd_list"] or 0 for t in totals.values()),
                    "real_auth_unchanged": auth_digest() == before and (prev or {}).get("real_auth_unchanged", True)})
    (OUT / SUMMARY).write_text(json.dumps(summary, indent=1))
    print(json.dumps({k: summary[k] for k in ("calls_used", "cost_usd_list_total", "real_auth_unchanged")}, indent=1))
    print(json.dumps(summary["by_model_set"], indent=1))
    return 0


def lenient(r) -> bool:
    """Secondary score: strict match, or (for non-numeric answers) the expected
    value appears inside the answer line, e.g. the model decoded out loud
    instead of giving the bare value. Numeric answers stay strict."""
    if r["correct"]:
        return True
    want, got = (r["want"] or "").strip(), (r.get("got") or "")
    if not got or re.fullmatch(r"-?\d+", want) or len(want) < 2:
        return False
    norm = lambda x: re.sub(r"\s+", " ", x).strip().lower()
    return norm(want) in norm(got)


def summarize(records):
    """Aggregates only (safe to publish for real data)."""
    out = {"by_model_set": {}, "by_model_set_class": {}, "by_kind": {}}
    key = lambda r: f"{r['model']}|{r['set']}"
    groups = defaultdict(list)
    for r in records:
        groups[key(r)].append(r)
    for k, rs in groups.items():
        v = defaultdict(lambda: [0, 0])
        lv = Counter()
        for r in rs:
            v[r["variant"]][0] += r["correct"]
            v[r["variant"]][1] += 1
            lv[r["variant"]] += lenient(r)
        # paired discordance raw vs crushed, raw vs raw2 (same sample+question)
        idx = defaultdict(dict)
        for r in rs:
            idx[(r["sample"], r["q"])][r["variant"]] = r["correct"]
        disc = Counter()
        for d in idx.values():
            if "raw" in d and "crushed" in d:
                disc[("raw_only" if d["raw"] and not d["crushed"] else "crushed_only" if d["crushed"] and not d["raw"]
                      else "both" if d["raw"] else "neither")] += 1
            if "raw" in d and "raw2" in d:
                disc["rep_pairs"] += 1
                disc["rep_disagree"] += d["raw"] != d["raw2"]
        out["by_model_set"][k] = {"acc": {vv: {"correct": c, "n": n, "pct": round(100 * c / n, 1) if n else None,
                                               "lenient": lv[vv], "lenient_pct": round(100 * lv[vv] / n, 1) if n else None}
                                          for vv, (c, n) in v.items()},
                                  "paired": dict(disc),
                                  "samples": len({r["sample"] for r in rs}),
                                  "tool_call_attempts": sum(1 for r in rs if r["tool_calls"])}
        byc = defaultdict(lambda: defaultdict(lambda: [0, 0]))
        for r in rs:
            byc[r["class"]][r["variant"]][0] += r["correct"]
            byc[r["class"]][r["variant"]][1] += 1
        out["by_model_set_class"][k] = {c: {vv: f"{a}/{b}" for vv, (a, b) in d.items()} for c, d in byc.items()}
    kinds = defaultdict(lambda: defaultdict(lambda: [0, 0]))
    for r in records:
        kinds[f"{r['set']}|{r['kind']}"][r["variant"]][0] += r["correct"]
        kinds[f"{r['set']}|{r['kind']}"][r["variant"]][1] += 1
    out["by_kind"] = {k: {vv: f"{a}/{b}" for vv, (a, b) in d.items()} for k, d in kinds.items()}
    return out


if __name__ == "__main__":
    sys.exit(main())
