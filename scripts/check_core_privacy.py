#!/usr/bin/env python3
r"""Core-privacy guard — the engine crates are named by `reuben-api` alone.

`reuben-api` is the one window between the engine and every consumer. Rust has no crate-level
visibility, so "the engine is private" cannot be a keyword: it means *no other crate's manifest
declares a dependency on it*, and a crate that cannot name an engine crate cannot reach past the
window by accident. This guard is that sentence, checked.

The engine is **two** crates: `reuben-core` (render) and `reuben-document` (authoring). Both are
private, and the split adds one legitimate edge between them — `reuben-document` sits above
`reuben-core` and names it. That one edge is allowed here and nowhere else; the reverse is not a
policy question but a build error, since it would be a Cargo cycle.

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

# The crates behind the window.
PRIVATE_CRATES = ("reuben-core", "reuben-document")
WINDOW_MANIFEST = "crates/reuben-api/Cargo.toml"
# Edges that are inside the engine rather than around the window: manifest -> what it may name.
# `reuben-document` is the authoring half and imports the render half upward, which is the whole
# shape of the split. see rules: execution-runtime
INTERNAL_EDGES = {"crates/reuben-document/Cargo.toml": {"reuben-core"}}

SKIP_DIRS = {".git", "target", "node_modules", "dist", "build"}
# A nested checkout carries a full copy of every workspace manifest, so a walk that reads it judges
# the same dependency edge twice — once at a path that is real and once at one that is discarded.
SKIP_PREFIXES = {(".claude", "worktrees")}

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

    def reachable(p: Path) -> bool:
        parts = p.relative_to(root).parts
        return not (set(parts) & SKIP_DIRS
                    or any(parts[:len(pre)] == pre for pre in SKIP_PREFIXES))

    manifests = sorted(p for p in root.rglob("Cargo.toml") if reachable(p))
    for path in manifests:
        rel = path.relative_to(root).as_posix()
        if rel == WINDOW_MANIFEST:
            continue
        allowed = INTERNAL_EDGES.get(rel, frozenset())
        try:
            manifest = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as e:
            problems.append(f"{rel}: unreadable manifest ({e})")
            continue
        # The engine's own manifest names itself in `[package]`, which is not a build edge.
        for table_name, deps in _dependency_tables(manifest):
            for key, spec in deps.items():
                named = key if key in PRIVATE_CRATES else _renamed_package(spec)
                if named not in PRIVATE_CRATES:
                    continue
                if named in allowed:
                    continue
                spelling = key if key == named else f'{key} = {{ package = "{named}" }}'
                problems.append(
                    f"{rel}: [{table_name}] names {named} (as `{spelling}`) — every "
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
