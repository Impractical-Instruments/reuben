#!/usr/bin/env python3
r"""Unit tests for check_sample_alias — the Sample/AudioBuffer naming guard.

Fixture trees are built with tempfile; the guard is imported as a bare module (tests run from
`scripts/`, mirroring the engine's skill-test idiom). Each test asserts the exact problem count so a
regression that over- or under-reports is caught, not just pass/fail.

The forbidden buffer forms are assembled from pieces (e.g. "f" + "32") so this test file does not
itself contain the bare literals — belt-and-suspenders alongside the guard's own allowlist entry
for this path.
"""
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import check_sample_alias

F32 = "f" + "32"
SLICE = "&[" + F32 + "]"          # &[f32]
SLICE_MUT = "&mut [" + F32 + "]"  # &mut [f32]
VEC = "Vec<" + F32 + ">"          # Vec<f32>
ARRAY = "[" + F32 + "; 4]"        # [f32; 4] — a fixed array, must NOT be flagged


def write(root: Path, rel: str, body: str) -> None:
    p = root / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body, encoding="utf-8")


class SampleAliasGuardTest(unittest.TestCase):
    def _problems(self, files: dict[str, str]) -> list[str]:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            for rel, body in files.items():
                write(root, rel, body)
            return check_sample_alias.collect_problems(str(root))

    def test_empty_tree_is_green(self):
        self.assertEqual(self._problems({}), [])

    def test_raw_slice_in_ordinary_code_is_flagged(self):
        p = self._problems({"crates/reuben-core/src/graph.rs": f"fn f(x: {SLICE}) {{}}\n"})
        self.assertEqual(len(p), 1)
        self.assertIn("graph.rs:1", p[0])

    def test_raw_mut_slice_is_flagged(self):
        p = self._problems({"crates/reuben-core/src/graph.rs": f"fn f(x: {SLICE_MUT}) {{}}\n"})
        self.assertEqual(len(p), 1)

    def test_raw_vec_is_flagged(self):
        p = self._problems({"crates/reuben-core/src/graph.rs": f"let v: {VEC} = vec![];\n"})
        self.assertEqual(len(p), 1)

    def test_slice_and_vec_on_same_line_count_twice(self):
        body = f"fn f(x: {SLICE}) -> {VEC} {{ todo!() }}\n"
        p = self._problems({"crates/reuben-core/src/graph.rs": body})
        self.assertEqual(len(p), 2)

    def test_fixed_size_array_is_not_flagged(self):
        # [f32; 4] is a scalar-count array, not a buffer form — the alias does not apply.
        p = self._problems({"crates/reuben-core/src/graph.rs": f"let a: {ARRAY} = [0.0; 4];\n"})
        self.assertEqual(p, [])

    def test_bare_f32_scalar_is_not_flagged(self):
        p = self._problems({"crates/reuben-core/src/graph.rs": "fn f(x: f32) -> f32 { x }\n"})
        self.assertEqual(p, [])

    def test_naming_site_is_exempt(self):
        # sample.rs is allowed to name the raw forms — it is the one definition site.
        body = f"pub type AudioBuffer<'a> = &'a [{F32}];\n"
        p = self._problems({"crates/reuben-core/src/sample.rs": body})
        self.assertEqual(p, [])

    def test_allowlisted_file_is_exempt(self):
        p = self._problems({"crates/reuben-native/src/audio.rs": f"fn f(x: {SLICE}) {{}}\n"})
        self.assertEqual(p, [])

    def test_allowlisted_dir_prefix_is_exempt(self):
        # A dir-prefix entry (operators/) covers every file beneath it.
        p = self._problems({"crates/reuben-core/src/operators/newop.rs": f"let v: {VEC} = vec![];\n"})
        self.assertEqual(p, [])

    def test_skip_dir_is_not_scanned(self):
        p = self._problems({"target/debug/build/x.rs": f"fn f(x: {SLICE}) {{}}\n"})
        self.assertEqual(p, [])

    def test_non_code_extension_is_ignored(self):
        # Markdown is not code — a doc example is not a naming site violation.
        p = self._problems({"docs/notes.md": f"an example: {SLICE}\n"})
        self.assertEqual(p, [])

    def test_whitespace_variants_are_flagged(self):
        # The guard tolerates spacing the way `cargo fmt` would not emit but a human might.
        body = "fn f(x: &[ f32 ]) -> Vec< f32 > { todo!() }\n"
        p = self._problems({"crates/reuben-core/src/graph.rs": body})
        self.assertEqual(len(p), 2)


if __name__ == "__main__":
    unittest.main()
