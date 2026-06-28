#!/bin/bash
# SessionStart hook for Claude Code on the web.
#
# The reuben CLI (reuben-native) links ALSA for audio output, so building or
# running it — including `reuben validate`/`describe`, which the patcher and
# control-surface skills depend on — needs the ALSA development headers. The
# remote container ships without them, so install them here. See README
# "Prerequisites".
set -euo pipefail

# Only run in the remote (Claude Code on the web) environment; local machines
# are expected to have their own toolchain set up per the README.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

# Idempotent: apt-get install is a no-op if the package is already present.
if ! dpkg -s libasound2-dev >/dev/null 2>&1; then
  if [ "$(id -u)" -eq 0 ]; then
    apt-get update -qq && apt-get install -y -qq libasound2-dev
  else
    sudo apt-get update -qq && sudo apt-get install -y -qq libasound2-dev
  fi
fi
