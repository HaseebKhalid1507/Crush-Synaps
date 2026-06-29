# crush vs headroom-bridge — measurement results

**Run:** S225 (2026-06-29), Jade. Identical fixtures through both extensions over
the real Synaps JSON-RPC protocol. Bytes are what the model actually receives
(replace = compressed, continue = unchanged). Honest numbers — no tiktoken
estimates, no synthetic ratios.

> Reproduce: `cd ~/Jawz/workspace/crush && cargo build --release && python3 measure.py`

---

## 1. Compression — crush ≈ headroom (parity, crush marginally ahead)

Input capped at 256 KiB (the post-Fork-1 `max_tool_buffer` — what crush actually
sees in production).

| fixture | bytes | crush | saved | headroom | saved |
|---|--:|--:|--:|--:|--:|
| build_verbose.txt | 7,043 | 7,043 | 0% | 7,043 | 0% |
| cargo_metadata.json (trunc) | 262,144 | 262,144 | 0% | 262,144 | 0% |
| git_log.txt | 38,439 | 38,439 | 0% | 38,439 | 0% |
| **json_array.json** | 83,068 | **49,592** | **41%** | 50,120 | 40% |
| ls_bin.txt | 239,643 | 239,643 | 0% | 239,643 | 0% |
| ps_aux.txt | 60,106 | 60,106 | 0% | 60,106 | 0% |
| **TOTAL** | 690,443 | 656,967 | **5%** | 657,495 | 5% |

**The honest read:** the wins live almost entirely on **structured JSON arrays**,
where crush (41%) edges headroom (40%). On everything else — columnar text
(`ls`, `ps`), pipe-delimited logs, truncated/non-array JSON — **both compress
nothing**, correctly. The "61–66%" from S224 was a favorable JSON-only sample,
not a representative mix. On a realistic spread, blended savings are ~5% and the
two tools are at **parity**.

This kills the "crush wins on ratio" thesis. Ratio is a wash. The case for crush
is everything below.

---

## 2. Footprint — crush wins by 366×

| | crush | headroom-bridge |
|---|---|---|
| artifact | **416 KB** single static binary | **149 MB** venv |
| runtime deps | **0** | 44 site-packages (pydantic, tiktoken, ast-grep-cli, rich…) |
| install | drop a binary | `pip install` + venv management |

## 3. Speed — crush wins cold and warm

| metric | crush | headroom | crush advantage |
|---|--:|--:|--:|
| cold spawn→result | 2.0 ms | 236.8 ms | **115× faster** |
| warm steady-state /call | 0.94 ms | 7.42 ms | **7.9× faster** |

Cold = per-session process spawn cost (Python interpreter + heavy imports). Warm
= steady-state per-call in a long-lived process (the production case). crush wins
both; the dominant, unambiguous win is footprint + zero-dep + cold start.

---

## 4. Verdict (recommendation — the retire decision is yours)

On the numbers: **compression is a tie; everything else favors crush.**

- ✅ **Parity** on compression (marginally ahead on JSON arrays).
- ✅ **366× smaller**, **zero dependencies** (no venv, no pip, no 22-package backpack).
- ✅ **8–115× faster**, microsecond-class native startup.
- ✅ **Honest accounting** — real bytes, not a foreign tokenizer's estimate.
- ✅ **We own every transform** — extend/audit without a black box.

**Recommendation:** retire headroom-bridge in favour of crush. We give up nothing
on compression and shed 149 MB + 44 dependencies + ~99% of the latency.

**But two honest caveats before you pull the trigger (step 5 — your call):**
1. **The ceiling is low on a realistic mix (~5%).** Neither tool is a big context
   saver across typical tool output. If the goal is real context savings, the
   lever is *better transforms* (e.g. tabular-ize columnar text like `ls`/`ps`,
   near-identical line collapse for timestamped logs), not switching engines.
   crush is the right *foundation* to build those on; headroom is a dead end for it.
2. **crush has not run live in a real Synaps session yet.** It's measured in
   isolation and unit-tested (31 tests), but never symlinked into `~/.synaps-cli/plugins`.
   Recommend one live shakeout session before retiring headroom for real.

**Not done (deliberately):** crush is NOT installed. headroom-bridge is untouched
and still active. Nothing in your live environment changed tonight.
