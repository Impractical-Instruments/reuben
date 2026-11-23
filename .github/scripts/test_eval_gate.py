"""Tests for eval-gate.sh's baseline handling.

The gate swaps the measured surface (`crates/`, `docs/agents/`, `instruments/`) to the baseline ref,
builds the sidecar there and measures it, then restores HEAD and measures again with `--compare`.
When the surface is identical across the two refs it measures HEAD once instead. These tests run the
real script in a throwaway repo, with a `cargo` stub on PATH and a stand-in `reuben_eval.gate` module
that logs each invocation, so nothing is built or measured.

    cd .github/scripts && python3 -m unittest test_eval_gate -v
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("eval-gate.sh").resolve()

_TREE = {
    "crates/reuben-mcp/src/lib.rs": "// mcp base\n",
    "docs/agents/authoring.md": "guide base\n",
    "instruments/pad.json": '{"v": 1}\n',
    "README.md": "readme\n",
    ".gitignore": "eval-*.json\neval-history-record.jsonl\n__pycache__/\n",
    "eval/reuben_eval/__init__.py": "",
    # Logs its argv and which side of the swap the tree is on, then writes any `--json` it was asked
    # for, so the HEAD run's `--compare` has a file to read.
    "eval/reuben_eval/gate.py": (
        "import os, pathlib, sys\n"
        "side = pathlib.Path('../crates/reuben-mcp/src/lib.rs').read_text().strip()\n"
        "with open(os.environ['EVAL_GATE_TEST_LOG'], 'a') as log:\n"
        "    log.write(' '.join(sys.argv[1:]) + ' | ' + side + '\\n')\n"
        "args = sys.argv[1:]\n"
        "if '--json' in args:\n"
        "    pathlib.Path(args[args.index('--json') + 1]).write_text('{}')\n"
    ),
}

_CARGO_STUB = "#!/bin/sh\nexit 0\n"


def _git(repo: pathlib.Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout


class EvalGateBaseline(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self._tmp.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        _git(self.repo, "init", "-q")
        _git(self.repo, "config", "user.email", "test@example.invalid")
        _git(self.repo, "config", "user.name", "test")
        _git(self.repo, "config", "commit.gpgsign", "false")
        self.base = self._commit(_TREE, "base")
        bin_dir = root / "bin"
        bin_dir.mkdir()
        stub = bin_dir / "cargo"
        stub.write_text(_CARGO_STUB)
        stub.chmod(0o755)
        self.log = root / "eval.log"
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GITHUB_")}
        self.env["PATH"] = f"{bin_dir}{os.pathsep}{self.env['PATH']}"
        self.env["EVAL_GATE_TEST_LOG"] = str(self.log)
        self.env["GITHUB_WORKSPACE"] = str(self.repo)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _commit(self, files: dict[str, str], message: str) -> str:
        for rel, text in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        _git(self.repo, "add", "-A")
        _git(self.repo, "commit", "-qm", message)
        return _git(self.repo, "rev-parse", "HEAD").strip()

    def _run(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(SCRIPT), self.base],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def _calls(self) -> list[str]:
        return self.log.read_text().splitlines() if self.log.exists() else []

    def _assert_clean(self) -> None:
        self.assertEqual(_git(self.repo, "status", "--porcelain"), "")

    def test_identical_surface_measures_head_once_without_a_baseline(self) -> None:
        self._commit({"README.md": "docs only\n", "eval/notes.txt": "harness side\n"}, "head")
        proc = self._run()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        calls = self._calls()
        self.assertEqual(len(calls), 1, calls)
        self.assertNotIn("--compare", calls[0])
        self.assertIn("--history", calls[0])
        self.assertIn("identical to the baseline", proc.stdout)
        self._assert_clean()

    def test_each_surface_path_breaks_identity(self) -> None:
        for rel in ("crates/reuben-mcp/src/lib.rs", "docs/agents/authoring.md", "instruments/pad.json"):
            with self.subTest(path=rel):
                self.base = _git(self.repo, "rev-parse", "HEAD").strip()
                if self.log.exists():
                    self.log.unlink()
                text = (self.repo / rel).read_text()
                # The stand-in reads this file to say which side it ran on, so keep its first line.
                self._commit({rel: text.strip() + " moved\n"}, f"move {rel}")
                proc = self._run()
                self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
                calls = self._calls()
                self.assertEqual(len(calls), 2, calls)
                self.assertIn("--no-gate", calls[0])
                self.assertIn("--compare", calls[1])
                self.assertNotIn("identical to the baseline", proc.stdout)
                self._assert_clean()

    def test_changed_surface_measures_the_baseline_tree_then_head(self) -> None:
        self._commit({"crates/reuben-mcp/src/lib.rs": "// mcp head\n"}, "head")
        proc = self._run()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        calls = self._calls()
        self.assertEqual(len(calls), 2, calls)
        self.assertTrue(calls[0].endswith("| // mcp base"), calls[0])
        self.assertTrue(calls[1].endswith("| // mcp head"), calls[1])
        self._assert_clean()


if __name__ == "__main__":
    unittest.main()
