#!/bin/sh
# Point git at the version-controlled hooks. Run once per clone.
set -e
git config core.hooksPath scripts/hooks
chmod +x scripts/hooks/pre-commit
echo "hooks installed: core.hooksPath -> scripts/hooks"
