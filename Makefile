# crush — eval entry points (see eval/README.md)
.PHONY: eval eval-free eval-fixtures
eval:            ## full Phase 0 baseline: corpus + fixtures + tokens + accuracy (model calls, capped) + report
	eval/run.sh
eval-free:       ## same, without model calls (count_tokens only)
	eval/run.sh --no-accuracy
eval-fixtures:   ## fixtures only; no local Synaps corpus needed
	eval/run.sh --fixtures-only --no-accuracy
