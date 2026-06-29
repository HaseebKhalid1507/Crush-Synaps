# crush vs headroom-bridge — measurement results

**Run:** S225 (2026-06-29), Jade. Identical fixtures through both extensions over
the real Synaps JSON-RPC protocol. Bytes are what the model actually receives
(replace = compressed, continue = unchanged). Honest numbers.

> Reproduce: `cd ~/Jawz/workspace/crush && cargo build --release && python3 measure.py`

> **UPDATE (Phase 2, same night):** added tool-aware columnar transforms (`ls`, `ps`).
> The picture below is the **post-Phase-2** result. The original Phase-1 numbers (compression
> parity, ~5% blended) are preserved at the bottom for the record.

---

## 1. Compression — crush now WINS decisively (29% vs 4% blended)

Input capped at 256 KiB (the post-Fork-1 `max_tool_buffer`).

| fixture | bytes | crush | saved | headroom | saved |
|---|--:|--:|--:|--:|--:|
| build_verbose.txt | 7,043 | 7,043 | 0% | 7,043 | 0% |
| cargo_metadata.json (trunc) | 262,144 | 262,144 | 0% | 262,144 | 0% |
| **git_log.txt** | 38,439 | **27,902** | **28%** | 38,439 | 0% |
| json_array.json | 83,068 | 49,592 | 41% | 50,120 | 40% |
| **ls_bin.txt** | 239,643 | **132,842** | **45%** | 239,643 | 0% |
| **ls_lah.txt** | 224,088 | **126,688** | **44%** | 224,088 | 0% |
| **ps_aux.txt** | 60,106 | **43,467** | **28%** | 60,106 | 0% |
| **TOTAL** | 914,531 | 649,724 | **29%** | 881,583 | 4% |

**The read:** tool-awareness is the whole game. Columnar tool output (`ls`, `ps`)
— which BOTH tools compressed 0% in Phase 1 — now compresses 28–45% under crush
via per-tool schema folding (constant-column factoring + per-column dictionary
encoding + alignment-padding removal). headroom still gets 0% on all of it. On a
realistic mix, **crush 28% vs headroom 4% — 7× better**, and the gap grows with
every schema added (grep, find, read still on the table).

This is no longer parity. crush wins compression outright, AND keeps the
footprint/speed advantages below.

---

## 2. Footprint — crush wins by 366×

| | crush | headroom-bridge |
|---|---|---|
| artifact | **416 KB** single static binary | **149 MB** venv |
| runtime deps | **0** | 44 site-packages |

## 3. Speed — crush wins cold and warm

| metric | crush | headroom | crush advantage |
|---|--:|--:|--:|
| cold spawn→result | 2.0 ms | 236.8 ms | **115× faster** |
| warm steady-state /call | 0.94 ms | 7.42 ms | **7.9× faster** |

---

## 4. Verdict (recommendation — retire decision is yours)

Phase 2 changed the answer. It's no longer "tie on compression, lighter
everywhere else." It's **crush wins on compression (7× blended), AND is 366×
lighter and 8–115× faster.**

- ✅ **Beats headroom on compression** 28% vs 4% blended; 44–45% on `ls`, 28% on `ps`.
- ✅ **366× smaller, zero dependencies.**
- ✅ **8–115× faster.**
- ✅ **Honest, lossless** — every columnar fold round-trips in a test.
- ✅ **Extensible** — each new tool schema is a few lines; grep/find/read still unbanked.

**Recommendation: retire headroom-bridge for crush.** The Phase-1 caveat (low
realistic ceiling) is resolved — tool-awareness IS the better-transforms lever,
and it's built.

**Remaining caveat:** crush still hasn't run live in a real Synaps session. Do ONE
live shakeout before retiring headroom for real. crush is NOT yet symlinked into
`~/.synaps-cli/plugins`; headroom-bridge is untouched and still active.

---

## Appendix — Phase 1 results (tool-blind, for the record)

Before tool-aware transforms, compression was parity: crush ~5% vs headroom ~5%
blended, both 0% on `ls`/`ps` (columnar text the generic transforms can't see).
The "61–66%" from S224 was a favorable JSON-only sample. Phase 2 (tool-aware
columnar folding) is what turned ~5% into 28%.

