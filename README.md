<div align="center"><pre>
 ██████╗██████╗ ██╗   ██╗███████╗██╗  ██╗
██╔════╝██╔══██╗██║   ██║██╔════╝██║  ██║
██║     ██████╔╝██║   ██║███████╗███████║
██║     ██╔══██╗██║   ██║╚════██║██╔══██║
╚██████╗██║  ██║╚██████╔╝███████║██║  ██║
 ╚═════╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═╝
        tool-output compression for AI agents
</pre></div>

<p align="center"><strong>~29% fewer tokens on real tool output · tool-aware · lossless · zero dependencies · 416&nbsp;KB · sub-millisecond · one Rust binary</strong></p>

<p align="center">
  <img src="https://img.shields.io/badge/rust-stable-orange.svg" alt="Rust">
  <img src="https://img.shields.io/badge/tests-50%20passing-brightgreen.svg" alt="Tests">
  <img src="https://img.shields.io/badge/fuzzed-libFuzzer%20%2B%20ASAN-brightgreen.svg" alt="Fuzzed">
  <img src="https://img.shields.io/badge/runtime%20deps-0-blue.svg" alt="Zero deps">
  <img src="https://img.shields.io/badge/binary-416%20KB-blue.svg" alt="Binary size">
  <img src="https://img.shields.io/badge/license-MIT-lightgrey.svg" alt="License">
</p>

<p align="center">
  <a href="#get-started-60-seconds">Install</a> ·
  <a href="#how-it-works-30-seconds">How it works</a> ·
  <a href="#proof">Proof</a> ·
  <a href="#lossless--and-you-can-check-it">Lossless</a> ·
  <a href="#the-transforms">Transforms</a>
</p>

---

**crush** rewrites the bloated output of your agent's tools — `ls`, `ps`, `git log`, JSON dumps, build logs — into a dense, schema-coded form **before it reaches the model**. It runs as a [Synaps](https://github.com/HaseebKhalid1507/SynapsCLI) extension on the `after_tool_call` seam: the tool runs, crush folds the result, the folded result enters the model's context. The model reads less. The window lasts longer. **Nothing is lost** — every byte is recoverable.

## See it

A `ls -lah /usr/bin` listing — what the model used to read, vs. what it reads now:

```text
# before — raw ls (every row repeats perms, owner, group, the schema)
-rwxr-xr-x  1 root root    55K Apr 20 10:58 [
-rwxr-xr-x  1 root root    35K Mar 20 04:32 a52dec
lrwxrwxrwx  1 root root     30 Aug 13 19:11 androiddeployqt6 -> ../lib/qt6/bin/...
...3885 more rows...

# after — crushed (schema once, dictionaries for low-cardinality columns, constants factored)
[@crush/1 2709→1591 bytes (-41%)]
@crush/1.dict perms A=drwxr-xr-x B=-rwxr-xr-x C=lrwxrwxrwx
@crush/1.cols perms links owner=root group=root size month day time name
B 1 55K B 20 B [
B 1 35K C 20 C a52dec
C 1 30 A 13 Q androiddeployqt6 -> ../lib/qt6/bin/...
```

<sub>**41% fewer bytes, every field recoverable.** `owner`/`group` factored to the header (`=root`), `perms`/`month`/`time` dictionary-coded to one char, the alignment padding gone. The model reads `B 1 55K B 20 B [` and knows exactly what it means.</sub>

## What it does

- **Tool-aware columnar folding** — knows the shape of `ls -lah`, `ps aux`, `git log --pretty=…|…`. Factors constant columns, dictionary-codes low-cardinality ones, strips alignment padding. **Lossless.**
- **JSON tabular folding** — an array-of-objects (top-level *or* the largest one nested in a tree) becomes a header row + CSV body. Each key written once.
- **Log cleanup** — strips ANSI, trailing whitespace, collapses duplicate lines to `(×N)`, elides multi-KB base64/hex blobs, factors shared path prefixes.
- **Reversible** — `crush --unfold` reconstructs the original from a folded block. The lossless contract is *executable*, not just claimed.
- **Pass-through safe** — below a size floor, no win, an unrecognized tool, *any* internal error → the original output passes through untouched. A compressor that breaks a tool's output is worse than none.

## How it works (30 seconds)

```
  your agent's tool runs  (ls · ps · git log · grep · a JSON API · a build)
        │  raw output (can be hundreds of KB)
        ▼
  ┌──────────────────────────────────────────────────────────┐
  │  crush   (native Rust, runs in-process as a Synaps ext)   │
  │  ──────────────────────────────────────────────────────  │
  │  dispatch on tool_name / sniffed command                 │
  │     ├─ columnar fold   (ls, ps, git log — tool-aware)    │
  │     ├─ tabular fold    (JSON array-of-objects)           │
  │     └─ text cleanup    (ANSI · runs · blobs · prefixes)  │
  │                                                          │
  │  versioned @crush/1 wire format  ·  panic-firewalled     │
  └──────────────────────────────────────────────────────────┘
        │  folded output  (＋ a one-line provenance header)
        ▼
  model context   ←  via after_tool_call → HookResult::Replace
```

- **dispatch** picks a tool-aware specialist when it recognizes the producer, else the generic pipeline
- every transform is **all-or-nothing** — it folds cleanly or it doesn't fire, so the wire format is never ambiguous
- the model reads the folded form directly; dictionaries + schema make it self-describing

## Get started (60 seconds)

```bash
# 1 — build (one static binary, no runtime deps)
git clone https://github.com/HaseebKhalid1507/Crush-Synaps.git crush
cd crush && cargo build --release

# 2 — install as a Synaps extension (manifest ships in .synaps-plugin/)
ln -s "$PWD" ~/.synaps-cli/plugins/crush
#    the manifest subscribes to after_tool_call with
#    tools.intercept + tools.transform_output

# 3 — start Synaps. tool output now arrives crushed.
```

**Decode any folded block yourself:**

```bash
echo '@crush/1.cols perms owner=root size name
B 4096 .
B  55K [' | ./target/release/crush --unfold
```

## Proof

Real tool output, measured in bytes the model actually receives — **not** a cherry-picked best case. These are the honest per-tool numbers and the **realistic blended mix**:

| Tool output                       |   raw |  crushed | saved |
|-----------------------------------|------:|---------:|------:|
| `ls -lah /usr/bin` (3,888 entries)| 224 KB|   127 KB | **44%** |
| `ps aux`                          |  60 KB|    43 KB | **28%** |
| `git log --pretty=…\|…\|…`         |  38 KB|    28 KB | **28%** |
| JSON array (200 objects)          |  83 KB|    50 KB | **41%** |
| **realistic mixed workload**      |   —   |    —     | **~29%** |

**Footprint & speed:**

| | crush |
|---|---|
| binary | **416 KB**, single static file |
| runtime dependencies | **0** |
| latency | **0.94 ms**/call warm · ~2 ms cold |
| tests | **50** passing, clippy clean |

> The wins concentrate where output is *structured* — columnar listings and JSON.
> Unstructured prose passes through honestly at ~0%. crush tells you the truth
> about what it can and can't compress; that's the point.

**Reproduce it:**

```bash
cargo build --release && python3 measure.py
```

## Lossless — and you can check it

Tool-aware folds are **byte-faithful on every field value and the tail** (only whitespace *padding* is normalized — it carries no information to an LLM). This isn't a promise, it's a test that runs in CI and a decoder that ships in the binary:

```bash
# fold an ls listing, decode it back, diff the fields — they match.
crush --unfold < folded_block.txt
```

The round-trip is verified field-by-field on thousands of real rows. JSON/log transforms are *meaning-preserving* (an LLM reads them faithfully); the columnar path is the one with the executable lossless contract.

## The transforms

| transform | input | mechanism |
|---|---|---|
| **columnar fold** | `ls`, `ps`, `git log` | constant-column factoring + per-column 1-char dictionary + padding removal |
| **tabular fold** | JSON array-of-objects (incl. nested) | schema header + CSV body, key written once |
| **run collapse** | logs | identical consecutive lines → `line (×N)` |
| **ANSI / whitespace** | colored output | strip escape codes + trailing whitespace |
| **blob elision** | embedded base64/hex >1 KiB | replace with `[@crush/1.blob N chars]` *(lossy, by design)* |
| **path prefix** | `find`, recursive listings | factor a shared directory prefix once |

New tools are a few lines: declare a `Schema` (columns, delimiter, tail), and the engine validates the shape and bails safely if the output doesn't match.

## Safety

A compression layer must never break or drop a tool's output. crush is built around that:

- **panic firewall** — any internal panic is caught at the hook boundary and degrades to pass-through
- **never enlarges** — the size gate accounts for its own header; output is only replaced on a real net win
- **no double-compression** — input already carrying `@crush/1` markers passes through untouched
- **deterministic** — same input → same bytes, so it never invalidates a provider's prompt cache
- **bounded** — a frame-size cap and depth limits guard against pathological input
- **fuzzed** — coverage-guided (libFuzzer + AddressSanitizer): **~2.75M executions across `compress`/`unfold`/`run`, zero crashes, zero panics, zero leaks**. 100% safe Rust — no `unsafe`, so memory corruption is impossible by construction.

## License

MIT.

<sub>Built as a native extension for <a href="https://github.com/HaseebKhalid1507/SynapsCLI">Synaps</a>. The model reads less; the work stays the same.</sub>
