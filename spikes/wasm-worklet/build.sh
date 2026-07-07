#!/usr/bin/env bash
# Throwaway build wiring for the #223 spike (the real pipeline is P4).
# Builds the cdylib for wasm32-unknown-unknown in release (the AC measures headroom;
# panics still ship their message via the log import) and stages it next to the page.
set -euo pipefail
cd "$(dirname "$0")"

rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/reuben_wasm_worklet_spike.wasm web/
echo "staged web/reuben_wasm_worklet_spike.wasm ($(wc -c < web/reuben_wasm_worklet_spike.wasm) bytes)"
