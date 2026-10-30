# Changelog

## 2.0.0 (2026-10-30)

crush 2: a rewrite around what the model reads and copies back, gated on real
tokens, model accuracy and edit safety. Shipped as a Synaps extension
(`after_tool_call` → `replace`); no Synaps changes needed.

### Library (`crush-core`)
- Pure `compress(Input, Config) -> Outcome`: classifier (tool name, light shell
  parse, output sniffing) into CONTENT / SEARCH / LISTING / LOG / STRUCTURED /
  VCS / PROSE / UNKNOWN, transform pipeline per class, and an invariant gate
  every result passes: CONTENT byte-identical, strictly fewer estimated tokens
  under both tokenizer families, idempotent, deterministic, marker-like input
  passes through, panics caught.
- On by default (each at accuracy parity on Haiku 4.5 and Sonnet 4.6):
  identical-line runs (`repeats`, lossless), `\r` progress (`progress`), long
  blobs (`blobs`), over-long grep hit lines (`search_elide`), build-progress
  runs (`build_progress`). The lossy ones only run when the host keeps the raw.
- Built, gated and off: `search_group`, `ls_columns`, `test_pass`,
  `json_minify`, `test_group` (passing libtest names grouped by module, the
  `test_pass` redesign). Reasons in `eval/RESULTS.md`.
- Token estimator fitted on real `count_tokens` numbers (median error ~4%).

### Extension (`crush-ext`)
- Outputs Synaps truncated for the hook (`…[truncated, N total bytes]`, over
  256 KiB) pass through; replacing them dropped the tail Synaps still held.
- Classifies on `tool_runtime_name`, falling back to `tool_name`.
- Own raw store for lossy results: `~/.cache/crush/raw/<session>/<hash>.txt`
  (`$XDG_CACHE_HOME` if set), 0600 files in 0700 dirs, written atomically before
  the replacement is returned; a failed write passes the result through. The
  marker quotes the absolute path. Retention sweep at startup and hourly
  (7 days, 256 MiB by default). Lossy transforms are now on by default (v0.2
  shipped `args: []`, so they never ran).
- Spill-aware: a replacement is only returned when it doesn't make the
  post-spill history cost more than passing through (port of Synaps' #334
  preview); a LOG result that keeps failure lines the preview would cut is the
  one exception.
- Config through the manifest's declared `config` entries and a `/settings`
  category (master switch, per-class and per-transform switches, raw-store
  retention and cap, spill threshold); `/settings` edits apply without a restart.
- `/crush`: latest session and all sessions, by class and transform, pass-through
  reasons, raw-store size; persisted in `~/.cache/crush/stats.json`.
  `crush-ext --report` and `--sweep` for use outside the TUI.
- JSON-RPC framing hardened: bounded header lines, body allocation bounded by
  what arrived, a bad frame doesn't swallow the next one.

### Eval and release
- `eval/`: private real-session corpus, sanitised fixtures, `count_tokens`
  client, raw-vs-crushed accuracy harness through `synaps rpc` with isolated
  credentials, edit-safety and invariant re-checks, after-spill accounting,
  shim checks against the real binary, and an end-to-end Synaps smoke test
  (`make smoke`). `make gate` runs everything free.
- Fuzz targets for every transform and for the shim's request path.
- v0.1 docs scrubbed of a local username and home paths; the v0.1 test
  fixtures that leaked them were removed from the tree in the Phase 1 rewrite
  (they remain in git history).

## 0.2.0
- crush 2 Phase 1 workspace (`crush-core`, `crush`, `crush-ext`) and Phase 2
  transforms; not released.

## 0.1.0
- First extension: schema-folded `ls`/`ps`/JSON tables, measured in bytes. See
  `eval/BASELINE.md` for why it was replaced.
