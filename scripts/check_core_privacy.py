#!/usr/bin/env python3
r"""Core-privacy guard — `reuben-core` is named by `reuben-api` alone.

`reuben-api` is the one window between the engine and every consumer. Rust has no crate-level
visibility, so "the engine is private" cannot be a keyword: it means *no other crate's manifest
declares a dependency on it*, and a crate that cannot name `reuben-core` cannot reach past the
window by accident. This guard is that sentence, checked.

It reads every `Cargo.toml` in the workspace and fails if any dependency table
(`[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, their `[target.*]` forms, and
`[workspace.dependencies]`) names `reuben-core` outside the window. Dev-dependencies count: a test
that reaches the engine directly proves the window is missing something, and reaching around it is
how that stops being visible.

What it does NOT flag: the source *path* `crates/reuben-core/src` (the operator scaffold writes
files there), doc references, and prose. Those name a directory or an idea, not a build edge.

Exit non-zero on any violation. Stdlib only. Runs unconditionally in CI, like the reference-linter
and the sample-alias guard: a reintroduced dependency edge can arrive in any change.

Usage: python3 scripts/check_core_privacy.py [root=.]
"""
from __future__ import annotations

import sys
import tomllib
from pathlib import Path

# The crate behind the window, and the one crate allowed to depend on it.
PRIVATE_CRATE = "reuben-core"
WINDOW_MANIFEST = "crates/reuben-api/Cargo.toml"

SKIP_DIRS = {".git", "target", "node_modules", "dist", "build"}

# Every table a build edge can be declared in. `target` and `workspace` are containers whose
# leaves are dependency tables, so they are walked rather than matched.
DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


def _dependency_tables(table: dict, trail: str = "") -> list[tuple[str, dict]]:
    """Every dependency table in a parsed manifest, paired with its dotted name."""
    found = []
    for key, value in table.items():
        if not isinstance(value, dict):
            continue
        name = f"{trail}{key}"
        if key in DEPENDENCY_TABLES:
            found.append((name, value))
        elif key in ("target", "workspace") or trail.startswith("target."):
            # `[target.'cfg(unix)'.dependencies]` — the middle level is the cfg expression, so
            # under `target.` every key is a container to walk rather than a name to match.
            found.extend(_dependency_tables(value, f"{name}."))
    return found


def _renamed_package(spec: object) -> str | None:
    """The real crate behind a renamed dependency (`engine = { package = "reuben-core", … }`).

    A key match alone is not the check: Cargo's rename is one line and reintroduces the edge under
    a name of the author's choosing, which is exactly the shape a guard that reads keys would wave
    through.
    """
    return spec.get("package") if isinstance(spec, dict) else None


def collect_problems(root_arg: str = ".") -> list[str]:
    root = Path(root_arg)
    problems: list[str] = []
    manifests = sorted(
        p for p in root.rglob("Cargo.toml")
        if not any(part in SKIP_DIRS for part in p.relative_to(root).parts)
    )
    for path in manifests:
        rel = path.relative_to(root).as_posix()
        if rel == WINDOW_MANIFEST:
            continue
        try:
            manifest = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as e:
            problems.append(f"{rel}: unreadable manifest ({e})")
            continue
        # The engine's own manifest names itself in `[package]`, which is not a build edge.
        for table_name, deps in _dependency_tables(manifest):
            for key, spec in deps.items():
                if key != PRIVATE_CRATE and _renamed_package(spec) != PRIVATE_CRATE:
                    continue
                spelling = key if key == PRIVATE_CRATE else f'{key} = {{ package = "{PRIVATE_CRATE}" }}'
                problems.append(
                    f"{rel}: [{table_name}] names {PRIVATE_CRATE} (as `{spelling}`) — every "
                    f"consumer reaches the engine through reuben-api, so take the window's "
                    f"dependency instead (add what is missing to reuben-api rather than "
                    f"reaching past it)"
                )
    return problems


def main(root_arg: str = ".") -> int:
    problems = collect_problems(root_arg)
    for p in problems:
        print(p, file=sys.stderr)
    print(f"check_core_privacy: {len(problems)} problem(s)", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:2]))
