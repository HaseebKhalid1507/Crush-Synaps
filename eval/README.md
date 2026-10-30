# crush eval harness (Phase 0)

The yardstick every crush 2 phase is gated on: a private corpus of real tool
results, committed synthetic fixtures, real Claude token counts, a raw-vs-crushed
model-accuracy test, an edit-safety check, and a report. Results for crush v0.1:
[`BASELINE.md`](BASELINE.md).

```bash
make eval            # everything, incl. ≤400 capped model calls (≈$5–6 at list price)
make eval-free       # everything except model calls (count_tokens is free)
make eval-fixtures   # fixtures only; works on a clean checkout with no local Synaps data
eval/run.sh --help   # flags and env knobs
```

Needs: `cargo`, `python3` (stdlib only), `synaps` on `PATH` for the accuracy step,
and an Anthropic credential (below). Build output goes to `$CARGO_TARGET_DIR`
(default `./target`), release, no debug info.

## Layout

| path | what |
|---|---|
| `corpus/extract.py` | real tool results from `~/.synaps-cli/sessions/*.json` (`api_messages`) and worker transcripts `~/.synaps-cli/subagent-history/*/sa_*.json` (`history`). Pairs `tool_use`↔`tool_result` by id, keeps tool name + input (incl. bash `command`), output, salted-hash session id, day. Dedupes identical outputs. Writes `~/.cache/crush-corpus/corpus.jsonl` (dir 0700, file 0600). |
| `corpus/classify.py` | first-draft classifier → CONTENT / SEARCH / LISTING / LOG / STRUCTURED / VCS / UNKNOWN from tool name, a light shell parse (first substantive segment of an `&&`/`;` chain, first command of its pipeline, subcommand, flags) and output sniffing. `python3 corpus/classify.py` runs its self-check. |
| `corpus/classifier_report.py` | command-vs-sniff agreement, UNKNOWN families, and a hand audit (`--audit-sheet N` writes a private sheet to label, `--audit-score` scores it). |
| `corpus/edit_sources.py` | for every real `edit`, which earlier output its `old_string` was copied from → which classes must stay byte-exact. |
| `fixtures/generate.py` | regenerates `fixtures/data/*` + `manifest.json` by running real tools in a scratch dir (cargo build/test pass+fail+warnings+errors, cargo metadata, pytest, node --test, npm, gcc/make, git on a depth-limited public clone, grep, find, ls, ps, docker, gh). Sanitises home/user/host/paths, pseudonymises authors/logins, renames docker repos, drops login-user processes from `ps`, then leak-checks (extra private words can go in `~/.cache/crush-corpus/fixture-denylist.txt`). The committed snapshot is what `make eval` uses. |
| `driver/` | tiny Rust bin linking the crush library; runs `compress::compress_labeled` (what the hook calls) on every sample, catching panics, timing each call. |
| `run_crush.py` | runs the driver over corpus/fixtures, and the real `crush` binary over JSON-RPC (as Synaps runs it) on a subset for round-trip latency, protocol errors and a library-vs-binary consistency check. |
| `lib/tokens.py` | `count_tokens` client: sqlite cache by (model, sha256), shared token bucket (4 req/s), 429/5xx retry with backoff, framing overhead subtracted. |
| `count_tokens.py` | per-sample raw+crushed counts for fixtures and every fired corpus sample; class composition of the whole corpus counted in packed batches (with a measured batching error). |
| `accuracy/questions.py` | question generators with exact answers parsed from the RAW output (grep path/line/count, ls size/perms/mtime/owner/symlink, ps pid/%mem/stat/user/rss, cargo/pytest/node pass-fail and counts, rustc/gcc diagnostic locations, git author/email/subject/date, JSON field-by-id / null-vs-"" / length, git status state, repeated-run counts, adjacent lines) + the scorer. |
| `accuracy/run_accuracy.py` | asks each question set about RAW and CRUSHED in separate fresh sessions via `synaps rpc`, Haiku and Sonnet, plus a raw re-ask on a subset (noise floor). Hard cap `--max-calls` (default 400). |
| `report.py` | regenerates the marked sections of `BASELINE.md` (aggregates only for real data). |

## Privacy

- Real data never enters the repo: the corpus, per-sample results, token cache,
  accuracy answers on real data and the audit sheet live in `~/.cache/crush-corpus`
  (0700). `common.assert_not_in_repo` refuses to write corpus files inside the
  checkout and `.gitignore` guards the usual names.
- `BASELINE.md` carries per-item numbers only for the synthetic fixtures; real-data
  numbers are aggregates.
- Text sent off the machine: corpus samples go to Anthropic's `count_tokens` and
  (for ≤80 sampled real outputs) to the model, i.e. the same provider those
  sessions already ran on. Nothing goes anywhere else.

## Credentials

- **count_tokens**: `ANTHROPIC_API_KEY` if set; otherwise the OAuth access token
  Synaps keeps in `~/.synaps-cli/auth.json` (`anthropic.access`), read-only, sent
  as `Authorization: Bearer` with the headers Synaps uses for OAuth
  (`anthropic-beta: claude-code-20250219,oauth-2025-04-20`, Claude Code identity
  as the first system block). The harness never refreshes the token (a refresh
  rotates the refresh token Synaps depends on). If it expires within 10 min it
  stops and asks you to let Synaps refresh it (`synaps status`). Never printed or
  logged; the cache stores only hashes and integers.
- **accuracy (`synaps rpc`)**: an isolated `SYNAPS_BASE_DIR` **and** `HOME` under
  `~/.cache/crush-corpus/sb*` (telemetry lands in `$HOME/.cache/synaps`, so HOME
  has to move too or it would append to your real api-log). Its `auth.json` holds
  only the current access token with an **empty refresh token**, so it can't
  rotate your credentials; its config disables every tool and thinking, and has no
  plugins, extensions, MCP or memory. `synaps rpc` doesn't use the session daemon.
  The real `auth.json` is hashed before and after (abort on change) and the
  isolated dir is deleted at the end.

## Measuring a new crush

Point the driver at the new library (`eval/driver/Cargo.toml` path dep), rerun
`make eval`, diff `BASELINE.md`. Gates for Phase 2 transforms: tokens (not bytes)
down, accuracy at parity within the raw-vs-raw noise, zero CONTENT/SEARCH byte
changes.
