#!/usr/bin/env python3
r"""Unit tests for check_core_privacy — the "reuben-core is named by reuben-api alone" guard.

Fixture trees are built with tempfile; the guard is imported as a bare module (tests run from
`scripts/`, mirroring the other guards' idiom). Each test asserts the exact problem count, so a
regression that over- or under-reports is caught rather than just pass/fail.
"""
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import check_core_privacy


def write(root: Path, rel: str, body: str) -> None:
    p = root / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body, encoding="utf-8")


class CorePrivacyGuardTest(unittest.TestCase):
    def _problems(self, files: dict[str, str]) -> list[str]:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            for rel, body in files.items():
                write(root, rel, body)
            return check_core_privacy.collect_problems(str(root))

    def test_window_may_name_the_engine(self):
        problems = self._problems({
            "crates/reuben-api/Cargo.toml":
                '[package]\nname = "reuben-api"\n\n'
                '[dependencies]\nreuben-core = { path = "../reuben-core" }\n',
        })
        self.assertEqual(problems, [])

    def test_engine_manifest_naming_itself_is_not_an_edge(self):
        # `[package] name` is identity, not a dependency.
        problems = self._problems({
            "crates/reuben-core/Cargo.toml": '[package]\nname = "reuben-core"\n',
        })
        self.assertEqual(problems, [])

    def test_a_door_naming_the_engine_is_flagged(self):
        problems = self._problems({
            "crates/reuben-native/Cargo.toml":
                '[package]\nname = "reuben-native"\n\n'
                '[dependencies]\nreuben-core = { path = "../reuben-core" }\n',
        })
        self.assertEqual(len(problems), 1)
        self.assertIn("[dependencies]", problems[0])

    def test_a_renamed_dependency_is_flagged(self):
        # Cargo's rename is one line and reintroduces the edge under any name the author likes —
        # the one legal spelling a key-matching guard would wave through.
        problems = self._problems({
            "crates/reuben-native/Cargo.toml":
                '[package]\nname = "reuben-native"\n\n'
                '[dependencies]\nengine = { package = "reuben-core", path = "../reuben-core" }\n',
        })
        self.assertEqual(len(problems), 1)
        self.assertIn('engine = { package = "reuben-core" }', problems[0])

    def test_a_dependency_that_merely_shares_a_key_name_is_not_flagged(self):
        # `reuben-core` as a *rename target* of something else is a different crate entirely.
        problems = self._problems({
            "crates/x/Cargo.toml":
                '[package]\nname = "x"\n\n'
                '[dependencies]\nother = { package = "reuben-contract", path = "../y" }\n',
        })
        self.assertEqual(problems, [])

    def test_build_dependencies_count(self):
        problems = self._problems({
            "crates/x/Cargo.toml":
                '[package]\nname = "x"\n\n'
                '[build-dependencies]\nreuben-core = { path = "../reuben-core" }\n',
        })
        self.assertEqual(len(problems), 1)
        self.assertIn("[build-dependencies]", problems[0])

    def test_dev_dependencies_count(self):
        # A test that reaches the engine directly is exactly the case this guard exists for.
        problems = self._problems({
            "crates/reuben-mcp/Cargo.toml":
                '[package]\nname = "reuben-mcp"\n\n'
                '[dev-dependencies]\nreuben-core = { path = "../reuben-core" }\n',
        })
        self.assertEqual(len(problems), 1)
        self.assertIn("[dev-dependencies]", problems[0])

    def test_target_scoped_dependencies_count(self):
        problems = self._problems({
            "crates/reuben-native/Cargo.toml":
                '[package]\nname = "reuben-native"\n\n'
                "[target.'cfg(unix)'.dependencies]\n"
                'reuben-core = { path = "../reuben-core" }\n',
        })
        self.assertEqual(len(problems), 1)
        self.assertIn("target.cfg(unix).dependencies", problems[0])

    def test_workspace_dependencies_count(self):
        problems = self._problems({
            "Cargo.toml":
                '[workspace]\nmembers = ["crates/reuben-core"]\n\n'
                '[workspace.dependencies]\nreuben-core = { path = "crates/reuben-core" }\n',
        })
        self.assertEqual(len(problems), 1)
        self.assertIn("workspace.dependencies", problems[0])

    def test_a_source_path_naming_the_crate_directory_is_not_an_edge(self):
        # The operator scaffold writes into `crates/reuben-core/src`, and `bin/reuben.rs` defaults a
        # flag to it. A parser never opens a `.rs` file, so what this really guards is a rewrite to
        # a text scan — which is what this repo's sibling guards are, so it is a live risk rather
        # than a hypothetical one. It doubles as the clean-tree baseline.
        problems = self._problems({
            "crates/reuben-native/Cargo.toml":
                '[package]\nname = "reuben-native"\n\n'
                '[dependencies]\nreuben-api = { path = "../reuben-api" }\n'
                'clap = "4"\n',
            "crates/reuben-native/src/scaffold.rs":
                'const CORE_ROOT: &str = "crates/reuben-core/src";\n',
        })
        self.assertEqual(problems, [])

    def test_build_directories_are_skipped(self):
        problems = self._problems({
            "target/debug/build/x/Cargo.toml":
                '[dependencies]\nreuben-core = { path = "../reuben-core" }\n',
        })
        self.assertEqual(problems, [])

    def test_an_unreadable_manifest_is_reported_rather_than_passed(self):
        problems = self._problems({"crates/broken/Cargo.toml": "[dependencies\n"})
        self.assertEqual(len(problems), 1)
        self.assertIn("unreadable manifest", problems[0])


if __name__ == "__main__":
    unittest.main()
