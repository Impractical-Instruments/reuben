"""Shared scaffolding for the rules-system suite: where the payload is, and proof that the
modules under test are the ones beside this directory.

The suite is offline by construction. Nothing here reaches the network, and the only external
process any test spawns is `git`, which `check_rules_refs.py` uses to enumerate a tree.
"""
from __future__ import annotations

import sys
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
RULES_DIR = TESTS_DIR.parent
TEMPLATES = RULES_DIR / "_templates"

sys.path.insert(0, str(RULES_DIR))

import check_rules_derive  # noqa: E402
import check_rules_links  # noqa: E402
import check_rules_refs  # noqa: E402

# The insert above is what actually wins against a vendored copy on PYTHONPATH — it goes to the
# front, so nothing later can shadow these names, and a payload module's name stops being unique on
# disk the moment repos adopt this system and carry their own `scripts/check_rules_*.py`.
#
# This assertion guards the case the insert cannot: a name already in `sys.modules` before this
# module is reached, and the insert itself being weakened or dropped by a later edit. It has caught
# the second one. Keep both — the insert is the mechanism, this is the alarm on it.
for _mod in (check_rules_derive, check_rules_links, check_rules_refs):
    _expected = RULES_DIR / (_mod.__name__ + ".py")
    assert Path(_mod.__file__).resolve() == _expected, (
        f"{_mod.__name__} was imported from {_mod.__file__}, not {_expected}")

# ii:begin provenance — derived from .ii/repo.toml; do not hand-edit out of sync. Regenerate with `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`. sha256=26e4570049b157b79eb89935fe8e6aa8fce5f4176b4a7610f0559720b1ec9e9d
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/tests/support.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/support.py?ref=main' --jq '.content' | base64 -d > scripts/tests/support.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
# ii:end provenance
