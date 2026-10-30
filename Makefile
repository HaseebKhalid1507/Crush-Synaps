# crush 2 — build, test and gate (see eval/README.md)
CARGO_TARGET_DIR ?= $(CURDIR)/target
export CARGO_TARGET_DIR
FUZZ_SECONDS ?= 600

.PHONY: build test gate eval eval-free eval-fixtures fuzz
build:           ## release workspace: crush-core, crush CLI, crush-ext shim
	cargo build --release --locked --workspace
test:            ## unit + property tests (release: proptest runs thousands of cases)
	cargo test --release --locked --workspace
gate: test       ## everything free: tests, eval without model calls, edit safety, latency, invariants
	eval/run.sh --no-accuracy
eval:            ## the full gate, incl. <= 400 capped model calls for accuracy
	eval/run.sh
eval-free:       ## same as eval, without model calls (count_tokens is free)
	eval/run.sh --no-accuracy
eval-fixtures:   ## fixtures only; no local Synaps corpus needed
	eval/run.sh --fixtures-only --no-accuracy
fuzz:            ## each libFuzzer target for FUZZ_SECONDS (needs nightly + cargo-fuzz)
	cd fuzz && for t in compress search shell search_elide listing log json; do \
	  cargo +nightly fuzz run -O $$t -- -max_total_time=$(FUZZ_SECONDS) -max_len=65536 || exit 1; done
