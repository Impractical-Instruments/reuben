# Canonical sources

<!-- GENERATED from templates/canonical-sources.md + .ii/repo.toml by `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .` — edit the source, not this file. sha256=2bb76fb43e4129c1cde614474f1c8547fea72f0de8dd4b264c30cec0701458ad -->

The registry of what this repo takes from elsewhere. One entry per declared source: what it is, where it comes from, and how it stays current.

This file is the registry only. The rule these entries obey is stated in full in this repo's front door — `docs/rules/README.md` — and is deliberately not restated here.

## scripts/check_rules_derive.py

- **Kind** — `vendored`
- **Path** — `scripts/check_rules_derive.py`
- **Source** — `Impractical-Instruments/agent-tools@main:plugins/impractical-doctrine/rules/check_rules_derive.py`
- **Refresh** — `gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/check_rules_derive.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_derive.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`

Provenance for this copy:

```
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/check_rules_derive.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/check_rules_derive.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_derive.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
```

## scripts/check_rules_links.py

- **Kind** — `vendored`
- **Path** — `scripts/check_rules_links.py`
- **Source** — `Impractical-Instruments/agent-tools@main:plugins/impractical-doctrine/rules/check_rules_links.py`
- **Refresh** — `gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/check_rules_links.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_links.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`

Provenance for this copy:

```
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/check_rules_links.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/check_rules_links.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_links.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
```

## scripts/check_rules_refs.py

- **Kind** — `vendored`
- **Path** — `scripts/check_rules_refs.py`
- **Source** — `Impractical-Instruments/agent-tools@main:plugins/impractical-doctrine/rules/check_rules_refs.py`
- **Refresh** — `gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/check_rules_refs.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_refs.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`

Provenance for this copy:

```
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/check_rules_refs.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/check_rules_refs.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_refs.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
```

## scripts/ii_verify.py

- **Kind** — `vendored`
- **Path** — `scripts/ii_verify.py`
- **Source** — `Impractical-Instruments/agent-tools@main:plugins/impractical-doctrine/verifier/ii_verify.py`
- **Refresh** — `gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/verifier/ii_verify.py?ref=main' --jq '.content' | base64 -d > scripts/ii_verify.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`

Provenance for this copy:

```
# Source:   Impractical-Instruments/agent-tools@4c5f337afe09ba5eab5567c67918ea66f97f3ad7:plugins/impractical-doctrine/verifier/ii_verify.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/verifier/ii_verify.py?ref=main' --jq '.content' | base64 -d > scripts/ii_verify.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
```

## scripts/tests/support.py

- **Kind** — `vendored`
- **Path** — `scripts/tests/support.py`
- **Source** — `Impractical-Instruments/agent-tools@main:plugins/impractical-doctrine/rules/tests/support.py`
- **Refresh** — `gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/support.py?ref=main' --jq '.content' | base64 -d > scripts/tests/support.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`

Provenance for this copy:

```
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/tests/support.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/support.py?ref=main' --jq '.content' | base64 -d > scripts/tests/support.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
```

## scripts/tests/test_check_rules_derive.py

- **Kind** — `vendored`
- **Path** — `scripts/tests/test_check_rules_derive.py`
- **Source** — `Impractical-Instruments/agent-tools@main:plugins/impractical-doctrine/rules/tests/test_check_rules_derive.py`
- **Refresh** — `gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/test_check_rules_derive.py?ref=main' --jq '.content' | base64 -d > scripts/tests/test_check_rules_derive.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`

Provenance for this copy:

```
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/tests/test_check_rules_derive.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/test_check_rules_derive.py?ref=main' --jq '.content' | base64 -d > scripts/tests/test_check_rules_derive.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
```

## scripts/tests/test_check_rules_links.py

- **Kind** — `vendored`
- **Path** — `scripts/tests/test_check_rules_links.py`
- **Source** — `Impractical-Instruments/agent-tools@main:plugins/impractical-doctrine/rules/tests/test_check_rules_links.py`
- **Refresh** — `gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/test_check_rules_links.py?ref=main' --jq '.content' | base64 -d > scripts/tests/test_check_rules_links.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`

Provenance for this copy:

```
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/tests/test_check_rules_links.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/test_check_rules_links.py?ref=main' --jq '.content' | base64 -d > scripts/tests/test_check_rules_links.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
```

## scripts/tests/test_check_rules_refs.py

- **Kind** — `vendored`
- **Path** — `scripts/tests/test_check_rules_refs.py`
- **Source** — `Impractical-Instruments/agent-tools@main:plugins/impractical-doctrine/rules/tests/test_check_rules_refs.py`
- **Refresh** — `gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/test_check_rules_refs.py?ref=main' --jq '.content' | base64 -d > scripts/tests/test_check_rules_refs.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`

Provenance for this copy:

```
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/tests/test_check_rules_refs.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/test_check_rules_refs.py?ref=main' --jq '.content' | base64 -d > scripts/tests/test_check_rules_refs.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
```
