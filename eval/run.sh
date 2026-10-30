#!/usr/bin/env bash
# crush eval harness: one entry point for the crush 2 gate.
#
#   eval/run.sh                    everything (corpus + fixtures + tokens + accuracy + report)
#   eval/run.sh --no-accuracy      skip model calls (count_tokens is free; accuracy is not)
#   eval/run.sh --fixtures-only    no local Synaps corpus needed
#   eval/run.sh --refresh-corpus   re-extract ~/.cache/crush-corpus/corpus.jsonl
#
# Env: CRUSH_EVAL_DAYS (30) corpus window · CRUSH_EVAL_MAX_CALLS (400) model-call cap
#      CRUSH_EVAL_SAMPLES (80) real samples in the accuracy slice · CRUSH_EVAL_MODELS (haiku,sonnet)
#      CRUSH_EVAL_QUOTA (search_group:46,repeats:16,progress:10,blobs:8) accuracy quotas per transform
#      CRUSH_CORPUS_DIR (~/.cache/crush-corpus) · CARGO_TARGET_DIR (./target)
# Accuracy runs on the candidate variant (every transform on, incl. the ones that
# ship disabled), so a disabled transform is re-tested by every `make eval`.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

ACC=1 CORPUS=1 REFRESH=0
for a in "$@"; do
  case "$a" in
    --no-accuracy) ACC=0 ;;
    --fixtures-only) CORPUS=0 ;;
    --refresh-corpus) REFRESH=1 ;;
    -h|--help) sed -n 2,13p "$0"; exit 0 ;;
    *) echo "unknown flag: $a" >&2; exit 2 ;;
  esac
done
DAYS="${CRUSH_EVAL_DAYS:-30}"
CORPUS_DIR="${CRUSH_CORPUS_DIR:-$HOME/.cache/crush-corpus}"
[ -s "$CORPUS_DIR/corpus.jsonl" ] || [ "$REFRESH" = 1 ] || CORPUS=0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
export CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_INCREMENTAL=0
step() { printf '\n== %s\n' "$*"; }
SETS=fixtures; [ "$CORPUS" = 1 ] && SETS=fixtures,corpus

step "build crush 2 (release workspace) + eval driver"
cargo build --release --locked -q --workspace
cargo build --release --locked -q --manifest-path eval/driver/Cargo.toml

step "self-checks"
python3 eval/corpus/classify.py

step "fixtures: run crush-core + crush-ext, count tokens"
python3 eval/run_crush.py --set fixtures --reps 5 >/dev/null
python3 eval/count_tokens.py --set fixtures

if [ "$CORPUS" = 1 ]; then
  if [ "$REFRESH" = 1 ] || [ ! -s "$CORPUS_DIR/corpus.jsonl" ]; then
    step "extract the real corpus (last $DAYS days) -> $CORPUS_DIR (private)"
    python3 eval/corpus/extract.py --since-days "$DAYS"
  fi
  step "corpus: run crush-core + crush-ext, count tokens (composition + every fired sample)"
  python3 eval/run_crush.py --set corpus --reps 3 >/dev/null
  python3 eval/count_tokens.py --set corpus
  step "classifier report (crush-core vs Phase 0) + estimator check"
  if [ -s "$CORPUS_DIR/classifier-audit.txt" ]; then
    python3 eval/corpus/classifier_report.py --audit-score >/dev/null
    python3 eval/corpus/classifier_report.py --audit-score --classes-from crush2 >/dev/null
  else
    python3 eval/corpus/classifier_report.py --classes-from crush2 >/dev/null
  fi
  if [ -s "$CORPUS_DIR/estimator-ids.json" ]; then
    python3 eval/estimator/fit.py --check
  fi
fi

# Candidates that ship disabled are still measured: the same passes with every
# transform on (eval/configs/search_on.json), written under the `search_on` tag.
step "candidates (search_group on): run, count tokens"
VARIANT=(env CRUSH_EVAL_TAG=search_on CRUSH_EVAL_CONFIG=eval/configs/search_on.json)
"${VARIANT[@]}" python3 eval/run_crush.py --set fixtures --reps 5 >/dev/null
"${VARIANT[@]}" python3 eval/count_tokens.py --set fixtures
if [ "$CORPUS" = 1 ]; then
  "${VARIANT[@]}" python3 eval/run_crush.py --set corpus --reps 3 >/dev/null
  "${VARIANT[@]}" python3 eval/count_tokens.py --set corpus --skip-composition
fi

step "latency: 256 KiB worst cases"
mkdir -p eval/out && "$CARGO_TARGET_DIR/release/crush-eval-driver" --synthetic > eval/out/synthetic.json

if [ "$ACC" = 1 ]; then
  step "accuracy: raw vs crushed through synaps rpc (cap ${CRUSH_EVAL_MAX_CALLS:-400} model calls)"
  "${VARIANT[@]}" python3 eval/accuracy/run_accuracy.py --sets "$SETS" --models "${CRUSH_EVAL_MODELS:-haiku,sonnet}" \
    --max-calls "${CRUSH_EVAL_MAX_CALLS:-400}" --corpus-samples "${CRUSH_EVAL_SAMPLES:-80}" \
    --quota "${CRUSH_EVAL_QUOTA:-search_group:46,repeats:16,progress:10,blobs:8}"
fi

step "edit-safety gate (CONTENT byte-identical, SEARCH lines rebuild)"
python3 eval/edit_safety.py --sets "$SETS" --strict >/dev/null
"${VARIANT[@]}" python3 eval/edit_safety.py --sets "$SETS" --strict >/dev/null

step "gate + report"
python3 eval/gate.py
python3 eval/report.py
