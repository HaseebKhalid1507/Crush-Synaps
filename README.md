# crush

Tool-output compression for [Synaps](https://github.com/HaseebKhalid1507/SynapsCLI),
as an extension: fewer input tokens per session, with no measured loss of model
accuracy, no broken edits, and nothing thrown away.

Every tool result Synaps hands to the `after_tool_call` hook is classified
(file view, grep hits, listing, build/test log, JSON, git, prose, other). Only
transforms that passed a real-token, model-accuracy and edit-safety gate run on
it; the rest passes through untouched. When a transform drops something (a
progress redraw, a blob, a run of `Compiling` lines), the raw output is written
to disk first and the marker in the text says where it is.

crush 2 is a rewrite. v0.1 folded `ls`/`ps`/JSON into schema-coded tables and
measured itself in bytes; on real work it fired on 0.5% of results, saved 0.2%
of tokens, made Sonnet read worse (85% → 74%) and changed bytes the model later
quoted in edits ([`eval/BASELINE.md`](eval/BASELINE.md)).

## The honest numbers

Measured on 30 days of the author's own Synaps sessions (59,843 unique tool
results, kept private) with Anthropic's `count_tokens`, plus 63 committed
synthetic fixtures. Full tables: [`eval/RESULTS.md`](eval/RESULTS.md).

- **It saves little on this workload, by design.** The shipped config changes
  198 of 59,843 results (0.3%) and saves 158,414 tokens: **0.25% of all
  tool-result tokens**, 39% of the results it fires on.
- **After Synaps' spill step** (results over 12 KiB become a 7 KiB preview),
  the extension saves 109,298 tokens on those results (335,754 → 226,456, 5.x):
  about **0.20% of what tool results cost after spill**, and none of them costs
  more than the raw would have (a hook that always replaced made 12 costlier).
- Half of what an agent carries is file views and diffs, which crush never
  changes (edits copy their `old_string` from them). Another fifth is grep hits,
  whose text it keeps byte for byte.
- Where it does fire, it's worth it: build logs −43%, over-long grep lines −40%,
  identical-line runs, base64/hex blobs and `\r` progress −20–65%.
- **Accuracy**: every shipped transform was at parity with the raw output on
  Claude Haiku 4.5 and Sonnet 4.6 (McNemar p ≥ 0.05 and within the raw-vs-raw
  noise). Five candidates that saved more failed that test and ship off:
  grouping grep hits by file, `ls -l` columns, passing-test collapse, a JSON
  layout, and git layouts that didn't pay.

## What it does

| class | what | default |
|---|---|---|
| CONTENT | `read`, `cat`, `sed -n`, `head`, `git show`/`diff`/`log -p` | never touched: byte-identical, enforced in code |
| SEARCH | `grep`, `rg`, `git grep` | a hit line over 1 KiB keeps its own `path:N:` and ~400 verbatim bytes around the match, with exact cut counts (`search_elide`, lossy); every other line untouched |
| LOG | builds, tests, logs | runs of `Compiling` / `Downloaded` / pip `Collecting` lines become `[crush: 15 build-progress lines not shown: 14 Compiling, 1 Downloaded]` (`build_progress`, lossy); warnings, errors, failures, summaries kept as printed |
| LOG, LISTING, VCS, STRUCTURED, UNKNOWN | everything else | identical-line runs written once with a counted note (lossless); `\r` progress collapsed to its final state and long base64/hex/minified blobs elided (lossy) |
| PROSE | forum, memory, worker reports, web pages | off |

Built, gated and **off**: `search_group`, `ls_columns`, `test_pass`,
`json_minify`, `test_group` (see [`eval/RESULTS.md`](eval/RESULTS.md) for why).
Each is one config switch.

## Safety guarantees

In the library ([`crates/crush-core`](crates/crush-core), enforced by `compress`):

- CONTENT comes back borrowed, byte for byte.
- Strictly fewer estimated tokens under both tokenizer families (4.x and 5.x), by
  at least 8 tokens and 2% (64 and 10% when lossy), or the raw passes through.
- Idempotent and deterministic: no clock, randomness or hash order.
- Output carrying crush/host markers or role/tool boundaries passes through.
- A panic in a transform is caught; the raw passes through. Under 1 ms p99 on
  256 KiB worst cases; outputs over 512 KiB pass through.

In the extension ([`crates/crush-ext`](crates/crush-ext)):

- **The raw is on disk before a lossy replacement leaves.** It goes to
  `~/.cache/crush/raw/<session>/<hash>.txt` (`$XDG_CACHE_HOME` if Synaps forwards
  it; it doesn't by default), files 0600, directories 0700, written atomically
  (exclusive temp file, then a hard link that never overwrites). If the write
  fails, the result passes through. The marker quotes the absolute path, so the
  model can `read` it. A retention sweep (startup, then hourly) deletes raws
  older than 7 days and keeps the store under 256 MiB, oldest first; it only
  ever touches file names it created.
- **It never makes the post-spill history cost more.** Synaps spills whatever
  the hook returns. A crush that pulls a 13–16 KiB result just under the
  12 KiB threshold would land whole in history instead of a 7 KiB preview, so
  the shim passes those through (12 of 198 fired results on the real corpus).
  When both versions still spill, it hands over the crushed text: the same
  preview budget then covers more of the output, and a later `read` of the
  spill file is cheaper. The one exception: it keeps a replacement that shows
  test failures the preview would have cut. The preview math is a byte-exact
  port of Synaps' (`runtime/tool_spill.rs`).
- **Truncated input passes through.** Synaps hands hooks at most 256 KiB plus
  `…[truncated, N total bytes]`; replacing that would drop the tail Synaps
  still has, so crush leaves outputs over 256 KiB alone.
- It classifies on the tool's runtime name (`tool_runtime_name`), so MCP and
  plugin tools are recognised by their real names.
- Any parse error, panic or I/O problem means `continue`: the original output.

## What only Synaps can fix

- **Subagent workers don't run hooks.** Workers get an empty hook bus
  (`host.rs:416`), so crush never sees their tool results. That's a Synaps-side
  decision.
- **Outputs over 256 KiB** reach the hook as a prefix, so crush can't touch them
  (and doesn't). Their tail, where summaries and late failures are, is only in
  Synaps' own spill file.
- **`/crush` isn't reachable over `synaps rpc`**: there's no command call in the
  RPC protocol. Use `crush-ext --report` there.
- The hook event has no `tool_use_id`, so raw files are named by a content hash;
  Synaps clears the extension's environment, so `XDG_CACHE_HOME` isn't seen.
- The extension can't read Synaps' spill settings. If you change
  `tool_output_spill_bytes`, set crush's `spill_threshold_bytes` to match.

## Install

Needs a Rust toolchain (`cargo`) and a Synaps build with `after_tool_call`
`Replace` support (the `tools.transform_output` permission).

```bash
git clone https://github.com/HaseebKhalid1507/Crush-Synaps ~/.synaps-cli/plugins/crush
cd ~/.synaps-cli/plugins/crush && bash scripts/setup.sh    # release build, --locked
```

Or install it through `/plugins`, which runs the same `scripts/setup.sh`. Start
a new Synaps session; the extension loads with it. The manifest asks for
`tools.intercept` (see tool results) and `tools.transform_output` (replace
them) and subscribes to `after_tool_call` only.

- **`/crush`** shows what it saved: the latest session and all sessions, by
  class and transform, pass-through reasons, and the raw store's size.
  Outside the TUI: `target/release/crush-ext --report`.
- **Settings**: `/settings` → crush (master switch, per-class switches, raw-store
  retention and cap, spill threshold); edits apply without a restart. Every key,
  including the per-transform switches, is also settable in
  `~/.synaps-cli/plugins/crush/config` (`key = value`) or with
  `SYNAPS_EXTENSION_CRUSH_<KEY>` (read at startup). Keys and defaults:
  [`.synaps-plugin/plugin.json`](.synaps-plugin/plugin.json).
- **Raw store**: `~/.cache/crush/raw`; `crush-ext --sweep` runs the retention
  sweep now.
- **Uninstall**: remove `~/.synaps-cli/plugins/crush` and `~/.cache/crush`.

## Layout

| crate | what |
|---|---|
| [`crates/crush-core`](crates/crush-core) | the library: classifier, transforms, gate, token estimator. Pure: no I/O, no clocks, no global state. |
| [`crates/crush`](crates/crush) | CLI: `cmd \| crush`, `crush classify`, `crush measure FILE…`, `crush --explain` |
| [`crates/crush-ext`](crates/crush-ext) | the Synaps extension: JSON-RPC shim, raw store, spill rule, config, `/crush` |
| [`eval/`](eval) | corpus, fixtures, real-token counting, accuracy harness, gates, Synaps smoke test ([README](eval/README.md)) |
| [`fuzz/`](fuzz) | libFuzzer targets for the library and the shim |

The library can be used on its own:

```rust
use crush_core::{compress, Config, Input};

let out = compress(&Input { tool: "bash", command: Some(cmd), output: &text, raw_ref: Some(&raw_path) }, &Config::default());
if !out.lossless {
    // must keep the raw at `raw_path` (the markers point there)
}
```

## Build, test, gate

```bash
make test        # unit + property tests
make gate        # tests + the free eval: real-token counts, edit safety, latency, invariants, shim checks
make eval        # the above + the accuracy test (<= 400 model calls)
make smoke       # install into an isolated Synaps (synaps-dev rpc) and run three commands (a few model calls)
make fuzz        # each fuzz target for FUZZ_SECONDS (nightly + cargo-fuzz)
```

The real-session corpus stays in `~/.cache/crush-corpus` and never enters the
repo; committed results are aggregates or synthetic fixtures.

## License

MIT
