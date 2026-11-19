"""Tests for perf-gate.sh's refusal to run over uncommitted work.

The gate swaps its source closure to the baseline with `git checkout <ref> -- <paths>`, which
overwrites the working tree and the index at those paths without a prompt. Run by hand over a dirty
tree, that destroys edits no reflog can recover, so the script refuses a dirty swap set before the
first swap. These tests build a throwaway repo shaped like the swap set and run the real script in
it, with a `cargo` stub on PATH so no bench is ever built.

Run from this directory, the way `scripts/`'s guard suites are:

    cd .github/scripts && python3 -m unittest test_perf_gate -v
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("perf-gate.sh").resolve()

# One file per swapped pathspec, so every member of the swap set is exercised. The census file has to
# carry a `WORKLOADS` const, or the gate stops at its census check before it reaches a swap.
_BASE_TREE = {
    "crates/reuben-document/src/lib.rs": "// document base\n",
    "crates/reuben-core/src/lib.rs": "// core base\n",
    "crates/reuben-core/src/bench_support.rs": 'pub const WORKLOADS: &[&str] = &[\n    "gain",\n];\n',
    "crates/reuben-macros/src/lib.rs": "// macros base\n",
    "crates/reuben-contract/src/lib.rs": "// contract base\n",
    "crates/reuben-document/benches/fixtures/pad.json": "{}\n",
    "instruments/pad.json": '{"v": 1}\n',
    ".cargo/config.toml": "# base\n",
    "README.md": "readme\n",
}
_HEAD_CHANGES = {
    "crates/reuben-core/src/lib.rs": "// core head\n",
    "instruments/pad.json": '{"v": 2}\n',
}


def _git(repo: pathlib.Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout


class PerfGateDirtyTree(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self._tmp.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        _git(self.repo, "init", "-q")
        _git(self.repo, "config", "user.email", "test@example.invalid")
        _git(self.repo, "config", "user.name", "test")
        _git(self.repo, "config", "commit.gpgsign", "false")
        self._write(_BASE_TREE)
        _git(self.repo, "add", "-A")
        _git(self.repo, "commit", "-qm", "base")
        self.base = _git(self.repo, "rev-parse", "HEAD").strip()
        self._write(_HEAD_CHANGES)
        _git(self.repo, "commit", "-qam", "head")

        # The stub fails every call, so each layer's baseline run fails, its compile probe fails, and
        # the gate takes the "harness postdates the baseline" skip — a full swap-and-restore cycle
        # with nothing built. The log says whether the gate got past its preflight at all.
        bin_dir = root / "bin"
        bin_dir.mkdir()
        self.cargo_log = root / "cargo.log"
        stub = bin_dir / "cargo"
        stub.write_text(f'#!/bin/sh\necho "$*" >>"{self.cargo_log}"\nexit 1\n')
        stub.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GITHUB_")}
        self.env["PATH"] = f"{bin_dir}{os.pathsep}{self.env['PATH']}"

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _write(self, files: dict[str, str]) -> None:
        for rel, text in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)

    def _read(self, rel: str) -> str:
        return (self.repo / rel).read_text()

    def _run_gate(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(SCRIPT), self.base],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def _assert_refused(self, proc: subprocess.CompletedProcess[str], path: str) -> None:
        self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
        self.assertIn("Uncommitted changes in the swap set", proc.stderr)
        self.assertIn(path, proc.stderr)
        self.assertFalse(self.cargo_log.exists(), "the gate ran cargo before refusing")

    def test_unstaged_edit_is_refused_and_kept(self) -> None:
        self._write({"crates/reuben-core/src/lib.rs": "// work in progress\n"})
        proc = self._run_gate()
        self._assert_refused(proc, "crates/reuben-core/src/lib.rs")
        self.assertEqual(self._read("crates/reuben-core/src/lib.rs"), "// work in progress\n")

    def test_staged_edit_is_refused_and_kept_in_the_index(self) -> None:
        self._write({"crates/reuben-macros/src/lib.rs": "// staged work\n"})
        _git(self.repo, "add", "crates/reuben-macros/src/lib.rs")
        proc = self._run_gate()
        self._assert_refused(proc, "crates/reuben-macros/src/lib.rs")
        self.assertEqual(self._read("crates/reuben-macros/src/lib.rs"), "// staged work\n")
        self.assertEqual(
            _git(self.repo, "show", ":crates/reuben-macros/src/lib.rs"), "// staged work\n"
        )

    def test_untracked_file_in_the_swap_set_is_refused(self) -> None:
        self._write({"crates/reuben-contract/src/new_module.rs": "// not yet added\n"})
        proc = self._run_gate()
        self._assert_refused(proc, "crates/reuben-contract/src/new_module.rs")
        self.assertEqual(
            self._read("crates/reuben-contract/src/new_module.rs"), "// not yet added\n"
        )

    def test_each_swapped_pathspec_is_guarded(self) -> None:
        for rel in (
            "crates/reuben-document/src/lib.rs",
            "crates/reuben-core/src/bench_support.rs",
            "crates/reuben-document/benches/fixtures/pad.json",
            "instruments/pad.json",
            ".cargo/config.toml",
        ):
            with self.subTest(path=rel):
                original = self._read(rel)
                edited = original + "// dirty\n"
                self._write({rel: edited})
                proc = self._run_gate()
                self._assert_refused(proc, rel)
                self.assertEqual(self._read(rel), edited)
                self._write({rel: original})

    def test_dirt_outside_the_swap_set_does_not_block_the_gate(self) -> None:
        self._write({"README.md": "local notes\n"})
        proc = self._run_gate()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertTrue(self.cargo_log.exists(), "the gate never reached a bench run")
        self.assertEqual(self._read("README.md"), "local notes\n")

    def test_clean_tree_runs_and_is_restored_to_head(self) -> None:
        proc = self._run_gate()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertTrue(self.cargo_log.exists(), "the gate never reached a bench run")
        self.assertEqual(self._read("crates/reuben-core/src/lib.rs"), "// core head\n")
        self.assertEqual(self._read("instruments/pad.json"), '{"v": 2}\n')
        self.assertEqual(
            _git(self.repo, "status", "--porcelain", "--untracked-files=no"), ""
        )


if __name__ == "__main__":
    unittest.main()
