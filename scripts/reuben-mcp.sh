#!/usr/bin/env bash
# Spawn the MCP sidecar for an editor session, building it first so a cold clone self-heals.
#
# A bare `target/debug/reuben-mcp` entry is what a hand-written config reaches for, and it fails
# invisibly: on a fresh clone the binary does not exist, the client's spawn fails, the tools never
# appear, and a skill that hard-requires them refuses to run with no legible cause. Building here
# makes the first spawn slow instead of broken, and every later spawn picks up the current roster
# rather than whatever was last built by hand.
#
# The build must stay off **stdout**: this process *is* the stdio JSON-RPC transport, and one stray
# byte ahead of the first frame desynchronizes the client. Cargo writes progress to stderr, which
# the client surfaces as server logs, and `-q` keeps that to errors.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build -q -p reuben-mcp --bin reuben-mcp >&2
exec "${CARGO_TARGET_DIR:-target}/debug/reuben-mcp"
