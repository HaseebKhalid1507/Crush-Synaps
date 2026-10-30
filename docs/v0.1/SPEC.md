# crush — native tool-output compressor for Synaps

**Status:** spec'd S225 (2026-06-29, ~04:00 EDT). Replaces `headroom-bridge`.
**Decided by:** Haseeb. Two forks locked — (1) fix truncation ordering FIRST, (2) bolt-on extension.

---

## Why headroom-bridge dies

The Python extension at `~/Jawz/workspace/headroom-bridge/` is being scrapped. Reasons:

1. **22-dependency .venv** (pydantic, tiktoken, ast-grep-cli, rich, opentelemetry…) to run *one*
   transform. Heavy backpack for a shim.
2. **Black-box compression.** `headroom.compress()` only exposes SmartCrusher/JSON without the
   `[code,ml]` extras. We pay for a library, use a fraction.
3. **Wrong ruler.** Token accounting uses `tiktoken` (OpenAI tokenizer) to estimate savings on
   Claude output. Every reported % is approximate against the wrong tokenizer.
4. **Eats pre-truncated garbage** (the real killer — see Fork 1).

Proven baseline to beat: 61–66% on JSON tool output, live in a real session, lossless to model.
`crush` must match or beat that on the JSON case AND fix the truncation bug.

---

## Fork 1 — ✅ DONE (S225, PR #51) — fix the truncation ordering

> **SHIPPED.** `fix/tool-output-compress-then-truncate` → PR #51 (base = feat/after-tool-call-transform,
> stacks on #50). Split `max_tool_output` into `max_tool_buffer` (256KB safety) + `max_tool_output`
> (30KB context budget, applied AFTER the hook). Found a SECOND gate the spec missed — the hook-event
> preview cap (`MAX_HOOK_OUTPUT` 32KB→256KB). Both fixed. Behavior byte-identical with no extension.
> +4 tests, 856 passed 0 failed, clippy clean, workspace builds. RED proof: 150KB output reached hook
> as 30,029B before fix; full 150KB after. The compressor now gets whole food. crush skeleton UNBLOCKED.

## Build order

1. ✅ **[engine] Fork 1** — full-output hook + compress-then-truncate. **DONE — PR #51.**
2. ✅ **[crush] skeleton** — Rust process extension, JSON-RPC v1, pass-through safe. **DONE — slice 1.**
3. ✅ **[crush] tabular fold** — JSON array → schema+CSV. **DONE — slice 2.**
4. ✅ **[crush] text chain** — ANSI/whitespace strip + run collapse. **DONE — slice 3.**
5. ✅ **[crush] path fold + blob elision** — the long tail. **DONE — slice 4.**
6. ✅ **Measure** — crush vs headroom on real fixtures. **DONE — see RESULTS.md.**
   - Compression PARITY (41% vs 40% on JSON arrays, ~5% blended, 0% on text/columnar for both).
   - crush wins footprint 366× (416K vs 149M, 0 vs 44 deps) and speed 8–115×.
7. ⬜ **Retire headroom-bridge** — **HASEEB'S DECISION** (data in RESULTS.md). NOT done. crush
   not yet symlinked live; recommend one live shakeout session before retiring headroom.

### Original spec (for reference)


**The bug:** bash tool truncates output to ~30KB *before* the `after_tool_call` hook fires. So on
large outputs — exactly where compression matters — the compressor receives an already-chopped,
possibly-malformed string. Compression after truncation is theater.

**The fix:** the hook must see the FULL tool output, and truncation must happen AFTER the
transform (compress-then-truncate). This is a Synaps **engine** change. Lives next to #98 (Tool
Output Virtualization) and #164.

**Acceptance:** a 100KB JSON-array output reaches the extension intact (not pre-clipped to 30KB),
gets compressed, and only THEN is any length cap applied to the compressed form.

> No `crush` transform work ships until the engine feeds it whole food.

---

## Fork 2 (DECIDED) — bolt-on extension, not engine-native

`crush` stays a **process extension** (portable, toggleable, off-switch, not welded to the engine).
Same `after_tool_call` → `Replace` contract as headroom-bridge (the #166 primitive, PR #50).
Difference: a tiny **static Rust binary** instead of Python + 22 deps. Microsecond startup, zero
venv, zero pip.

Permissions (unchanged): `tools.intercept` (observe) + `tools.transform_output` (rewrite).

---

## The transform suite (we own every byte)

Reimplement SmartCrusher's real win natively, then stack transforms WE control:

| Transform | Win case |
|-----------|----------|
| **Tabular fold** | `[{a,b,c},{a,b,c}…]` → header row + CSV body. The 80% case. `ls -la`, `ps`, JSON list payloads. |
| **Run collapse** | repeated / near-identical lines → `(×N)`. Verbose build/test/log output. |
| **ANSI + whitespace strip** | color codes, trailing space, blank-line runs. |
| **Path folding** | shared path prefixes → `…/` once. |
| **Blob elision** | base64/hex chunks → `[blob 4.2KB sha:ab12]`. |

Savings measured against ACTUAL content (real ruler), not a foreign tokenizer.

**Safety invariant (inherited, non-negotiable):** any failure — parse, transform, panic — degrades
to pass-through `continue`. A compression layer must NEVER break or drop a tool's output.

---

## Build order

1. **[engine] Fork 1** — full-output hook + compress-then-truncate. Acceptance test above. *Blocks everything.*
2. **[crush] skeleton** — Rust process extension, JSON-RPC stdin/stdout v1, `initialize` /
   `hook.handle` / `shutdown`, pass-through `continue` for everything. Manifest + symlink. Proves the seam.
3. **[crush] tabular fold** — the headline transform. Beat headroom on the JSON case.
4. **[crush] run collapse + ANSI/whitespace strip** — the log case.
5. **[crush] path fold + blob elision** — the long tail.
6. **Measure** — real ROI on a real session vs headroom's 61–66% baseline. Honest numbers.
7. **Retire headroom-bridge** — remove symlink, archive the Python dir.

---

## Open questions for morning

- Where does the Rust binary live? New crate in `~/Projects/agent-runtime/` workspace, or
  standalone? (Leaning: standalone repo or `~/Jawz/tools/crush/` — it's an extension, not engine.)
- Reuse the `headroom-bridge` manifest shape verbatim, just swap `command` to the Rust binary.
- Length cap value post-compression — keep ~30KB, or raise it now that the compressor runs first?
