# crush Phase 2 — tool-aware transforms

**Status:** spec'd S225 (2026-06-29 ~05:10 EDT). Decided by Haseeb: scope now, build later.
**Premise:** crush is tool-*blind* today (generic tabular + text chain). The S225 measurement
(`RESULTS.md`) proved the biggest outputs — `ls` (239KB), `ps` (60KB) — compress 0% because they're
**columnar text**: structured to a human, invisible to the generic transforms. Tool-awareness is the
highest-value next move. This is the "better transforms, not engine swap" lever made concrete.

---

## Core insight — columnar fold, not N bespoke parsers

Don't write a separate parser for every tool. Write ONE **generic columnar-fold engine** and feed it
**per-tool schema hints**. The engine does the compression; the hint just says where the fixed
columns end and free-text begins.

Two compression mechanisms in the columnar engine:
1. **Constant-column factoring** — a column with the same value on every row is declared ONCE in a
   header, not repeated per line. (`ls -la`: `owner`/`group` are almost always identical → huge win.)
2. **Schema header + compact rows** — emit the column names once; rows carry only values.

Example — `ls -la` (the 239KB fixture):
```
drwxr-xr-x  2 haseeb haseeb   4096 Jun 29 04:29 main.py
-rw-r--r--  1 haseeb haseeb   5209 Jun 29 02:13 spec.md
```
folds to:
```
@crush.cols perms links size date name  [owner=haseeb group=haseeb]
drwxr-xr-x 2 4096 "Jun 29 04:29" main.py
-rw-r--r-- 1 5209 "Jun 29 02:13" spec.md
```
`owner`/`group` factored out (constant); schema declared once. Lossless — reconstructable by
re-expanding the constants and the schema.

---

## Dispatch architecture (slots into existing pipeline)

`compress(tool_input, output)` already RECEIVES `tool_name` + `tool_input` and ignores them. Add a
dispatch layer:

```
compress(tool_name, tool_input, output):
    1. resolve schema hint:
         - native tool? tool_name ∈ {ls, ps?, grep, find, read} → hint table
         - bash? sniff tool_input.command first token (ls / ps / docker ps / ...) → hint table
         - else → None
    2. if hint → columnar::fold(output, hint)   (tool-aware specialist)
    3. else or on failure → existing generic pipeline (tabular / text chain)
    4. keep the smallest; apply MIN_BYTES_SAVED gate + header as today
```

**Step 0 (verify first):** confirm the actual `tool_name` strings the engine sends in the
`after_tool_call` event for native tools vs bash. Grep `crates/agent-engine/src/extensions/hooks/
events.rs` + the tool registry. The dispatch keys MUST match reality — don't assume "ls".

---

## Per-tool schema hints

| tool | detect | fixed columns before free-text | constant-factor candidates |
|---|---|---|---|
| `ls -l/-la` | tool_name `ls` or bash `ls -l*` | perms, links, owner, group, size, month, day, time | owner, group |
| `ps aux` | bash `ps` | user, pid, %cpu, %mem, vsz, rss, tty, stat, start, time | user, tty |
| `grep -n/-rn` | tool_name `grep` | `file:line:` prefix | file (group consecutive same-file matches) |
| `find` | tool_name `find` | path list | shared dir prefix (reuse `fold_common_prefix`) |
| `read` | tool_name `read` | `NNN→` line-number gutter | the gutter (strip, reconstructable) |

`grep` is special: not fixed-width — it's `file:line:text`. Transform = group by file:
```
@crush.grep src/main.rs
  12: fn main() {
  40:     run();
@crush.grep src/lib.rs
  3: pub fn run() {}
```
factors the repeated `src/main.rs:` prefix once per group.

---

## Hard parts (why we scoped it instead of 5am-ing it)

- **`ls` filenames with spaces** and the symlink ` -> target` suffix. The `name` column is free-text
  and can contain anything. Parse the FIXED columns by field count from the left, then treat the
  rest of the line (after the Nth field) as the name verbatim. Never split the name.
- **Variable whitespace alignment.** Split on runs of whitespace for fixed columns, but preserve the
  name tail exactly. Use `splitn(N+1, whitespace)` semantics.
- **Lossless re-expansion.** A consumer (or a human) must be able to reconstruct the original. The
  header must fully declare the schema + constants. Round-trip test this.
- **Confidence gating.** Only fold when ≥ K rows share a consistent field count for the fixed
  columns. On ANY ambiguity, bail to the generic pipeline. Corruption is worse than no compression.

---

## Build order (each a TDD slice, commit green)

1. **[verify] tool_name reality** — confirm dispatch keys against the engine. Capture real fixtures:
   `ls -la /usr/bin`, `ps aux`, `grep -rn fn src/`, `find . -name '*.rs'`, a `read` of a big file.
2. **[columnar engine]** — `columnar::fold(text, hint) -> Option<String>`: constant-column factoring
   + schema header + safe field splitting + free-text tail preservation. Round-trip (reconstruct)
   test is mandatory. This is the core; build + test it in isolation first.
3. **[dispatch]** — wire `tool_name`/command sniff → hint → columnar; generic fallback. Unit-test
   dispatch routing.
4. **[grep grouping]** — the file-prefix-grouping transform (separate from fixed-width columnar).
5. **[read gutter strip]** + **[find → fold_common_prefix]** — the cheap ones.
6. **[re-measure]** — rerun `measure.py` with new fixtures. **Acceptance: `ls_bin.txt` and
   `ps_aux.txt` go from 0% to a meaningful win; blended total beats headroom.** Update RESULTS.md.

## Acceptance criteria
- `ls -la` (239KB fixture): meaningful compression (target ≥ 30% — owner/group/schema factoring).
- `ps aux`: meaningful compression.
- Every columnar fold round-trips losslessly in a test (reconstruct == original modulo whitespace).
- Any parse ambiguity → generic fallback, never corruption.
- Re-measured blended ratio beats headroom on the realistic mix → the real case to retire headroom.

## Non-goals (keep scope tight)
- No ML/heuristic column detection — schema hints only. If a tool isn't in the hint table, generic
  pipeline handles it. Add tools incrementally as the hint table grows.
- Still NOT going live without a shakeout session (carries over from Phase 1).
