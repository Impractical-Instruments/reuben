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

# ii:begin provenance — derived from .ii/repo.toml; do not hand-edit out of sync. Regenerate with `ii-generate --write .`. sha256=60fedd743c0a70807018fd783d71b42686d58eac6d914c651952b1e3e2eca895
# Source:   Impractical-Instruments/brain@440034f0365465428b89668736b6ae506e7c564c:machinery/rules/tests/support.py
# Fetched:  2026-08-16
# Refresh:  gh api 'repos/Impractical-Instruments/brain/contents/machinery/rules/tests/support.py?ref=main' --jq '.content' | base64 -d > scripts/tests/support.py && ii-generate --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
# ii:end provenance
