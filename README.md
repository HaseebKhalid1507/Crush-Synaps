# crush

Tool-output compression for AI agents: fewer input tokens, with no loss of model
accuracy and no broken edits.

crush 2 is a rewrite. v0.1 folded `ls`/`ps`/JSON into schema-coded tables and
measured itself in bytes; on real work it fired on 0.5% of results, saved 0.2% of
tokens, made Sonnet read worse (85% → 74%) and changed bytes the model later
quoted in edits ([`eval/BASELINE.md`](eval/BASELINE.md)). crush 2 starts from what
the model reads and copies back:

- **File views and diffs are never touched.** They are half of all tool-result
  tokens, and edits copy their `old_string` from them.
- **Every change is checked in real tokens**, not bytes, and must keep the
  model's answers at parity with the raw output (raw-vs-crushed accuracy test on
  Haiku and Sonnet, McNemar against the raw-vs-raw noise).
- **When in doubt, it passes the output through**, borrowed, untouched.

Results, gates and method: [`eval/RESULTS.md`](eval/RESULTS.md).

## What it does

| class | what | transform |
|---|---|---|
| CONTENT | `read`, `cat`, `sed -n`, `head`, `git show`/`diff` | none: byte-identical, enforced in code |
| SEARCH | `grep`, `rg`, `git grep` | hits grouped under a full-path heading (rg `--heading` layout); every `N:text` line byte-exact |
| LOG, LISTING, VCS, STRUCTURED, UNKNOWN | builds, tests, listings, git, JSON, everything else | `\r` progress collapsed to its final state; identical-line runs written once plus `[crush: previous line repeated 44 more times; 45 identical lines in total]`; long base64/hex/minified blobs elided with a marker pointing at the raw |
| PROSE | forum, memory, worker reports, web pages | none (documents and messages) |

```text
crates/agent-engine/src/runtime/stream.rs:1014:    let x = a();      crates/agent-engine/src/runtime/stream.rs
crates/agent-engine/src/runtime/stream.rs:1015-    let y = b();  ->  1014:    let x = a();
crates/agent-engine/src/runtime/stream.rs:1016:    let z = c();      1015-    let y = b();
src/main.rs:7:fn main() {                                             1016:    let z = c();

                                                                      src/main.rs
                                                                      7:fn main() {
```

Nothing is reordered or abbreviated: heading + `:`/`-` + line rebuilds every
original line, and the gate checks that on every result. No shared-prefix
factoring, no dictionary codes, no `(×N)`: those are what made v0.1 hard to read.

The transforms that lose information (progress frames, blobs) only run when the
host promises to keep the raw output (`Input::raw_ref`); their markers say where
it is.

## Layout

| crate | what |
|---|---|
| [`crates/crush-core`](crates/crush-core) | the library: classifier, transforms, gate, token estimator. Pure: no I/O, no clocks, no global state. |
| [`crates/crush`](crates/crush) | CLI: `cmd \| crush`, `crush classify`, `crush measure FILE…`, `crush --explain` |
| [`crates/crush-ext`](crates/crush-ext) | Synaps process-extension shim (`after_tool_call` JSON-RPC, `/crush` report) |
| [`eval/`](eval) | corpus, fixtures, real-token counting, accuracy harness, gates ([README](eval/README.md)) |
| [`fuzz/`](fuzz) | libFuzzer targets: `compress`, `search`, `shell` |

## Use it

**As a library** (how Synaps will call it, natively, before its spill step):

```rust
use crush_core::{compress, Config, Input};

let out = compress(&Input { tool: "bash", command: Some(cmd), output: &text, raw_ref: Some(&spill_path) }, &Config::default());
if !out.lossless {
    // must keep the raw at `spill_path` (the markers point there)
}
history.push(out.text); // Cow: borrowed (zero-copy) when nothing changed
```

`Outcome` carries the class, the transforms that ran, `lossless`, estimated
tokens before/after, and why it passed through when it did. `Config` is
serde-deserialisable: a master switch, per-class and per-transform switches,
size limits and the gain margin.

**As a CLI:**

```bash
cargo build --release
grep -rn foo src | target/release/crush --command 'grep -rn foo src' --explain
target/release/crush measure --command 'grep -rn foo .' out1.txt out2.txt
```

**As a Synaps extension** (until native integration): install the plugin; its
setup step builds `target/release/crush-ext`. `/crush` reports the session's
estimated savings. Lossy transforms stay off unless you pass
`--keep-raw-dir DIR` in the manifest's `args`.

## Guarantees (enforced by `compress`, not by convention)

- CONTENT comes back borrowed, byte for byte.
- Strictly fewer estimated tokens under both tokenizer families (4.x and 5.x),
  by at least 8 tokens and 2%, or the raw passes through. The estimator is fitted
  against real `count_tokens` numbers ([`eval/estimator/`](eval/estimator)).
- Idempotent: the result is a fixed point of `compress`, or the raw passes through.
- Deterministic; no clock, randomness or hash order.
- Output with crush/host markers or role/tool boundaries passes through.
- SEARCH results must rebuild every original line, or the raw passes through.
- A panic in a transform is caught; the raw passes through.
- Bounded time: under 1 ms on 256 KiB worst cases; outputs over 512 KiB pass through.

## Build, test, gate

```bash
make test        # unit + property tests (proptest)
make gate        # tests + the free eval: real-token counts, edit safety, latency, invariants
make eval        # the above + the accuracy test (<= 400 model calls)
make fuzz        # each fuzz target for FUZZ_SECONDS (nightly + cargo-fuzz)
```

The real-session corpus stays in `~/.cache/crush-corpus` and never enters the
repo; committed results are aggregates or synthetic fixtures.

## License

MIT
