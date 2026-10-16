#!/usr/bin/env bash
# Post-install build step. Synaps runs it as `bash scripts/setup.sh` from the plugin
# dir after a /plugins install or update (manifest `extension.setup`), and the
# plugin's `extension.command` (target/release/crush) must exist afterwards.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
if ! command -v cargo >/dev/null 2>&1; then
  echo "crush: cargo not found on PATH. Install the Rust toolchain (https://rustup.rs) and reinstall the plugin." >&2
  exit 1
fi
if [ ! -f Cargo.lock ]; then
  echo "crush: Cargo.lock is missing; refusing an unlocked build." >&2
  exit 1
fi
cargo build --release --locked
test -x target/release/crush
