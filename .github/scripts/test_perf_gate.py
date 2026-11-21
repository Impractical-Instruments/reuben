"""Tests for perf-gate.sh's handling of the working tree it swaps, and of the new-operator skip list.

The gate swaps its source closure to the baseline with `git checkout <ref> -- <paths>`, which
rewrites the working tree and the index at those paths without a prompt. So it must refuse to start
while anything there differs from HEAD, and leave exactly HEAD behind — on a normal exit and on an
interrupt. These tests build a throwaway repo shaped like the
swap set and run the real script in it, with a `cargo` stub on PATH so no bench is ever built.

Run from this directory, the way `scripts/`'s guard suites are:

    cd .github/scripts && python3 -m unittest test_perf_gate -v
"""

from __future__ import annotations

import json
import os
import pathlib
import signal
import subprocess
import tempfile
import time
import unittest

SCRIPT = pathlib.Path(__file__).with_name("perf-gate.sh").resolve()

# One file per swapped pathspec, so every member of the swap set is exercised. The baseline predates
# the kinds file, so its census is the `WORKLOADS` const the gate falls back to; HEAD carries the
# kinds file, or the gate stops at its census check before it reaches a swap.
_BASE_TREE = {
    "crates/reuben-document/src/lib.rs": "// document base\n",
    "crates/reuben-core/src/lib.rs": "// core base\n",
    "crates/reuben-core/src/bench_support.rs": 'pub const WORKLOADS: &[&str] = &[\n    "gain",\n];\n',
    "crates/reuben-core/src/retired.rs": "// only the baseline has this\n",
    "crates/reuben-macros/src/lib.rs": "// macros base\n",
    "crates/reuben-contract/src/lib.rs": "// contract base\n",
    "crates/reuben-document/benches/fixtures/pad.json": "{}\n",
    "instruments/pad.json": '{"v": 1}\n',
    ".cargo/config.toml": "# base\n",
    ".gitignore": "*.wav\nbench-history-record.jsonl\n",
    "README.md": "readme\n",
}
# Tracked at the baseline although the ignore rules cover it; HEAD deletes it.
_BASE_IGNORED = "instruments/take.wav"
_KINDS_FILE = "crates/reuben-document/benches/micro_kinds.txt"
_BENCH_SUPPORT = "crates/reuben-core/src/bench_support.rs"
_HEAD_CHANGES = {
    _KINDS_FILE: "# generated\ngain\n",
    "crates/reuben-core/src/lib.rs": "// core head\n",
    "crates/reuben-core/src/added.rs": "// only HEAD has this\n",
    "instruments/pad.json": '{"v": 2}\n',
}
_HEAD_DELETES = ("crates/reuben-core/src/retired.rs", _BASE_IGNORED)

# Each stub call logs its args and which of the two side-only files the tree holds at that moment, so
# a test can see what the baseline run was built from.
_STUB = """#!/bin/sh
cd "$PERF_GATE_TEST_REPO" || exit 1
seen=""
[ -e crates/reuben-core/src/retired.rs ] && seen="$seen retired"
[ -e crates/reuben-core/src/added.rs ] && seen="$seen added"
echo "$* |$seen" >>"$PERF_GATE_TEST_LOG"
[ -n "$PERF_GATE_TEST_SKIP_LOG" ] && echo "${REUBEN_MICRO_BENCH_SKIP-unset}" >>"$PERF_GATE_TEST_SKIP_LOG"
[ -n "$PERF_GATE_TEST_SLEEP" ] && sleep "$PERF_GATE_TEST_SLEEP"
exit 1
"""


def _git(repo: pathlib.Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout


class PerfGateWorkingTree(unittest.TestCase):
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
        self._write({_BASE_IGNORED: "base take\n"})
        _git(self.repo, "add", "-A")
        _git(self.repo, "add", "-f", _BASE_IGNORED)
        _git(self.repo, "commit", "-qm", "base")
        self.base = _git(self.repo, "rev-parse", "HEAD").strip()
        self._write(_HEAD_CHANGES)
        _git(self.repo, "rm", "-q", *_HEAD_DELETES)
        _git(self.repo, "add", "-A")
        _git(self.repo, "commit", "-qm", "head")
        self.head_tree = self._tracked_state()

        # The stub fails every call, so each layer's baseline run fails, its compile probe fails, and
        # the gate takes the "harness postdates the baseline" skip — a full swap-and-restore cycle
        # with nothing built. The log also says whether the gate got past its preflight at all.
        bin_dir = root / "bin"
        bin_dir.mkdir()
        self.cargo_log = root / "cargo.log"
        stub = bin_dir / "cargo"
        stub.write_text(_STUB)
        stub.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GITHUB_")}
        self.env["PATH"] = f"{bin_dir}{os.pathsep}{self.env['PATH']}"
        self.env["PERF_GATE_TEST_REPO"] = str(self.repo)
        self.env["PERF_GATE_TEST_LOG"] = str(self.cargo_log)
        self.env.pop("PERF_GATE_TEST_SLEEP", None)
        self.env.pop("PERF_GATE_TEST_SKIP_LOG", None)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _write(self, files: dict[str, str]) -> None:
        for rel, text in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)

    def _read(self, rel: str) -> str:
        return (self.repo / rel).read_text()

    def _tracked_state(self) -> str:
        """Every tracked file's index entry plus its working-tree hash: equal means same tree."""
        index = _git(self.repo, "ls-files", "-s")
        names = _git(self.repo, "ls-files").split()
        hashes = _git(self.repo, "hash-object", "--", *names) if names else ""
        return index + hashes

    def _assert_exactly_head(self) -> None:
        self.assertEqual(_git(self.repo, "status", "--porcelain", "--untracked-files=all"), "")
        self.assertEqual(self._tracked_state(), self.head_tree)
        self.assertFalse((self.repo / "crates/reuben-core/src/retired.rs").exists())
        self.assertTrue((self.repo / "crates/reuben-core/src/added.rs").exists())

    def _cargo_calls(self) -> list[str]:
        return self.cargo_log.read_text().splitlines() if self.cargo_log.exists() else []

    def _run_gate(self, base: str | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(SCRIPT), base or self.base],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def _assert_refused(
        self, proc: subprocess.CompletedProcess[str], path: str, title: str
    ) -> None:
        self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
        self.assertIn(title, proc.stderr)
        self.assertIn(path, proc.stderr)
        self.assertEqual(self._cargo_calls(), [], "the gate ran cargo before refusing")

    # -- refusal ------------------------------------------------------------------------------------

    def test_unstaged_edit_is_refused_and_kept(self) -> None:
        self._write({"crates/reuben-core/src/lib.rs": "// work in progress\n"})
        proc = self._run_gate()
        self._assert_refused(proc, "crates/reuben-core/src/lib.rs", "Uncommitted changes")
        self.assertEqual(self._read("crates/reuben-core/src/lib.rs"), "// work in progress\n")

    def test_staged_edit_is_refused_and_kept_in_the_index(self) -> None:
        self._write({"crates/reuben-macros/src/lib.rs": "// staged work\n"})
        _git(self.repo, "add", "crates/reuben-macros/src/lib.rs")
        proc = self._run_gate()
        self._assert_refused(proc, "crates/reuben-macros/src/lib.rs", "Uncommitted changes")
        self.assertEqual(self._read("crates/reuben-macros/src/lib.rs"), "// staged work\n")
        self.assertEqual(
            _git(self.repo, "show", ":crates/reuben-macros/src/lib.rs"), "// staged work\n"
        )

    def test_untracked_file_in_the_swap_set_is_refused(self) -> None:
        self._write({"crates/reuben-contract/src/new_module.rs": "// not yet added\n"})
        proc = self._run_gate()
        self._assert_refused(proc, "crates/reuben-contract/src/new_module.rs", "Uncommitted changes")
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
                self._assert_refused(proc, rel, "Uncommitted changes")
                self.assertEqual(self._read(rel), edited)
                self._write({rel: original})

    def test_edit_hidden_by_an_index_flag_is_refused_and_kept(self) -> None:
        for flag in ("--assume-unchanged", "--skip-worktree"):
            with self.subTest(flag=flag):
                rel = "crates/reuben-document/src/lib.rs"
                _git(self.repo, "update-index", flag, rel)
                self._write({rel: "// hidden work\n"})
                self.assertEqual(_git(self.repo, "status", "--porcelain"), "")
                proc = self._run_gate()
                self._assert_refused(proc, rel, "Hidden changes")
                self.assertEqual(self._read(rel), "// hidden work\n")
                _git(self.repo, "update-index", flag.replace("--", "--no-", 1), rel)
                _git(self.repo, "checkout", "--", rel)

    def test_ignored_file_at_a_baseline_tracked_path_is_refused_and_kept(self) -> None:
        self._write({_BASE_IGNORED: "my own recording\n"})
        self.assertEqual(_git(self.repo, "status", "--porcelain"), "")
        proc = self._run_gate()
        self._assert_refused(proc, _BASE_IGNORED, "Ignored files")
        self.assertEqual(self._read(_BASE_IGNORED), "my own recording\n")

    # -- runs that go ahead -------------------------------------------------------------------------

    def test_ignored_file_the_baseline_does_not_track_runs_and_survives(self) -> None:
        self._write({"instruments/scratch.wav": "scratch\n"})
        proc = self._run_gate()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertNotEqual(self._cargo_calls(), [])
        self.assertEqual(self._read("instruments/scratch.wav"), "scratch\n")

    def test_dirt_outside_the_swap_set_does_not_block_the_gate(self) -> None:
        self._write({"README.md": "local notes\n"})
        proc = self._run_gate()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertNotEqual(self._cargo_calls(), [], "the gate never reached a bench run")
        self.assertEqual(self._read("README.md"), "local notes\n")

    def test_clean_run_benches_the_baseline_and_ends_exactly_at_head(self) -> None:
        proc = self._run_gate()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        calls = self._cargo_calls()
        self.assertNotEqual(calls, [], "the gate never reached a bench run")
        # Every stub call is a baseline run or its compile probe, both made over the swapped tree, so
        # the file only the baseline has is there. The file only HEAD has stays too: the swap to the
        # baseline is an overlay, which keeps a fixture the PR adds available to HEAD's harness.
        for call in calls:
            self.assertTrue(call.endswith("| retired added"), call)
        self._assert_exactly_head()
        # A second run over the tree the first one left must not refuse it.
        self.cargo_log.unlink()
        self.assertEqual(self._run_gate().returncode, 0)
        self._assert_exactly_head()

    def test_interrupted_run_restores_head(self) -> None:
        self.env["PERF_GATE_TEST_SLEEP"] = "2"
        for sig in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=sig.name):
                if self.cargo_log.exists():
                    self.cargo_log.unlink()
                proc = subprocess.Popen(
                    ["bash", str(SCRIPT), self.base],
                    cwd=self.repo,
                    env=self.env,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                deadline = time.monotonic() + 30
                while not self.cargo_log.exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertTrue(self.cargo_log.exists(), "the gate never reached a bench run")
                self.assertTrue((self.repo / "crates/reuben-core/src/retired.rs").exists())
                proc.send_signal(sig)
                self.assertNotEqual(proc.wait(timeout=30), 0)
                self.assertEqual(len(self._cargo_calls()), 1, "the gate ran on past the signal")
                self._assert_exactly_head()

    def test_baseline_missing_a_required_path_fails_without_writing(self) -> None:
        _git(self.repo, "rm", "-rq", "instruments")
        _git(self.repo, "commit", "-qm", "drop instruments")
        no_instruments = _git(self.repo, "rev-parse", "HEAD").strip()
        _git(self.repo, "checkout", "-q", "HEAD~1", "--", "instruments")
        _git(self.repo, "commit", "-qm", "restore instruments")
        self.head_tree = self._tracked_state()
        proc = self._run_gate(no_instruments)
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("Baseline snapshot failed", proc.stdout)
        self.assertEqual(self._cargo_calls(), [])
        self.assertEqual(_git(self.repo, "status", "--porcelain", "--untracked-files=all"), "")
        self.assertEqual(self._tracked_state(), self.head_tree)


def _kinds_file(*kinds: str) -> str:
    return "# Generated. Do not hand-edit.\n" + "".join(f"{k}\n" for k in kinds)


def _workloads(*kinds: str) -> str:
    """A `bench_support.rs` whose `WORKLOADS` names `kinds`, with the prose and the trailing quoted
    strings the scan has to stay clear of. `overhead` is named by its const, as the real table did."""
    lines = "".join(
        "    w(overhead::KIND, Recipe::Default),\n" if k == "overhead" else f'    w("{k}", Recipe::Default),\n'
        for k in kinds
    )
    return (
        "//! `pub const WORKLOADS` is mentioned in prose too.\n"
        f"pub const WORKLOADS: &[Workload] = &[\n{lines}];\n"
        'pub const SAMPLE_LOOP: &str = "not_a_kind";\n'
    )


def _legacy_skip(head: list[str], base: list[str]) -> str:
    """The skip list the gate computed before the kinds file existed, when both sides' `WORKLOADS`
    named every benched kind: HEAD's kinds minus the baseline's, sorted, comma-joined. The scan never
    saw `overhead`, which both sides named by its const."""
    return ",".join(sorted((set(head) - set(base)) - {"overhead"}))


class PerfGateNewOperatorSkip(unittest.TestCase):
    """`REUBEN_MICRO_BENCH_SKIP`: the kinds HEAD benches that the baseline cannot register.

    Each case writes the census each side carries, runs the real gate with the `cargo` stub, and
    reads the value every bench run received. The expectation is always `_legacy_skip` over the full
    kind sets the two sides bench — what the gate computed when `WORKLOADS` still listed every kind.
    """

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        root = pathlib.Path(self._tmp.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        _git(self.repo, "init", "-q")
        _git(self.repo, "config", "user.email", "test@example.invalid")
        _git(self.repo, "config", "user.name", "test")
        _git(self.repo, "config", "commit.gpgsign", "false")
        bin_dir = root / "bin"
        bin_dir.mkdir()
        stub = bin_dir / "cargo"
        stub.write_text(_STUB)
        stub.chmod(0o755)
        self.skip_log = root / "skip.log"
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GITHUB_")}
        self.env["PATH"] = f"{bin_dir}{os.pathsep}{self.env['PATH']}"
        self.env["PERF_GATE_TEST_REPO"] = str(self.repo)
        self.env["PERF_GATE_TEST_LOG"] = str(root / "cargo.log")
        self.env["PERF_GATE_TEST_SKIP_LOG"] = str(self.skip_log)
        self.env.pop("PERF_GATE_TEST_SLEEP", None)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _commit(self, files: dict[str, str], message: str) -> str:
        for rel, text in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        _git(self.repo, "add", "-A")
        _git(self.repo, "commit", "-q", "--allow-empty", "-m", message)
        return _git(self.repo, "rev-parse", "HEAD").strip()

    def _run(self, base: dict[str, str], head: dict[str, str]) -> subprocess.CompletedProcess[str]:
        """Commit `base`, then `head` over it, and run the gate from HEAD against `base`."""
        tree = {k: v for k, v in _BASE_TREE.items() if k != _BENCH_SUPPORT}
        base_sha = self._commit({**tree, **base}, "base")
        self._commit(head, "head")
        return subprocess.run(
            ["bash", str(SCRIPT), base_sha],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def _skip(self, base: dict[str, str], head: dict[str, str]) -> str:
        """Run the gate over `base` and `head` (see `_run`) and return the one skip list every
        bench run was handed."""
        proc = self._run(base, head)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        seen = set(self.skip_log.read_text().splitlines())
        self.assertEqual(len(seen), 1, f"bench runs were handed different skip lists: {seen}")
        return seen.pop()

    # -- baseline that has the kinds file --------------------------------------------------------

    def test_baseline_with_the_kinds_file_and_no_new_operator(self) -> None:
        kinds = ["chord", "gain", "overhead"]
        census = {_KINDS_FILE: _kinds_file(*kinds), _BENCH_SUPPORT: _workloads("chord")}
        self.assertEqual(self._skip(census, {}), _legacy_skip(kinds, kinds))
        self.assertEqual(_legacy_skip(kinds, kinds), "")

    def test_baseline_with_the_kinds_file_and_a_new_operator(self) -> None:
        base = ["chord", "gain", "overhead"]
        head = ["chord", "delay", "gain", "overhead"]
        # The baseline's `WORKLOADS` names only its recipe overrides. Reading it instead of the kinds
        # file would skip `gain` and `overhead` too, which the baseline benches.
        skip = self._skip(
            {_KINDS_FILE: _kinds_file(*base), _BENCH_SUPPORT: _workloads("chord")},
            {_KINDS_FILE: _kinds_file(*head)},
        )
        self.assertEqual(skip, _legacy_skip(head, base))
        self.assertEqual(skip, "delay")

    # -- baseline from before the kinds file: the `WORKLOADS` fallback ----------------------------

    def test_baseline_without_the_kinds_file_and_no_new_operator(self) -> None:
        kinds = ["chord", "gain", "overhead", "sample_loop"]
        skip = self._skip(
            {_BENCH_SUPPORT: _workloads(*kinds)},
            {_KINDS_FILE: _kinds_file(*kinds), _BENCH_SUPPORT: _workloads("chord", "sample_loop")},
        )
        self.assertEqual(skip, _legacy_skip(kinds, kinds))
        self.assertEqual(skip, "")

    def test_baseline_without_the_kinds_file_and_new_operators(self) -> None:
        base = ["chord", "gain", "overhead", "sample_loop"]
        head = ["add_f32_value", "chord", "gain", "overhead", "sample_loop", "zigzag"]
        skip = self._skip(
            {_BENCH_SUPPORT: _workloads(*base)},
            {_KINDS_FILE: _kinds_file(*head), _BENCH_SUPPORT: _workloads("chord", "sample_loop")},
        )
        self.assertEqual(skip, _legacy_skip(head, base))
        self.assertEqual(skip, "add_f32_value,zigzag")

    # -- the head census guard ----------------------------------------------------------------------

    def _assert_census_refused(self, head: dict[str, str]) -> None:
        proc = self._run({_BENCH_SUPPORT: _workloads("gain")}, head)
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("Operator census not found", proc.stdout)
        self.assertIn("gen_bench_census", proc.stdout)
        self.assertFalse(self.skip_log.exists(), "the gate benched without a census")

    def test_head_without_the_kinds_file_fails_before_any_bench(self) -> None:
        self._assert_census_refused({})

    def test_head_kinds_file_with_no_kinds_fails_before_any_bench(self) -> None:
        self._assert_census_refused({_KINDS_FILE: "# comments only\n"})

# Captured gungraun 0.19 compare-run output, ANSI escapes and all, the way CI's colored log holds it.
_E = "\x1b"
_HARVEST_LOG = "\n".join(
    [
        # A workload case: string-literal args, description wrapped in one extra pair of parens.
        f"{_E}[32mmacro_iai::macro_render::render{_E}[0m {_E}[36mauto_filter{_E}[0m:{_E}[1;34m(build_state(\"auto-filter\")){_E}[0m",
        f"{_E}[90m  {_E}[0mBaselines:                               |base",
        f"{_E}[90m  {_E}[0mInstructions:                    {_E}[1m43605487{_E}[0m|43605487             (No change)",
        f"{_E}[90m  {_E}[0mL1 Hits:                         58748398|58748398             (No change)",
        # A breached case whose normal line is absent: only the regression line carries its Ir.
        f"{_E}[32mmicro_iai::micro::process{_E}[0m {_E}[36mreverb{_E}[0m:{_E}[1;34m(setup(\"reverb\")){_E}[0m",
        f"{_E}[90m  {_E}[0mBaselines:                               |base",
        f"Performance has {_E}[1;91mregressed{_E}[0m: Instructions (11 -> {_E}[1m10891636{_E}[0m) regressed by {_E}[1;91m+99014773%{_E}[0m (>{_E}[90m+10.0000%{_E}[0m)",
        # A bench-id case: non-literal args, two of them, so the args hold a space.
        f"{_E}[32mconstruct_iai::construct_group::build{_E}[0m {_E}[36mwide_n2048{_E}[0m:{_E}[1;34m(wide(1024, 2)){_E}[0m",
        f"{_E}[90m  {_E}[0mInstructions:                    {_E}[1m53847764{_E}[0m|53844271             (+0.00649%)",
        # The end-of-run block names cases again; it must not record a second time.
        "Regressions:",
        "",
        "  micro_iai::micro::process reverb:",
        "    Callgrind: Instructions (11 -> 10891636): +99014773% exceeds limit of +10.0000%",
        "",
    ]
)


class PerfGateHarvest(unittest.TestCase):
    """`harvest_history` reads case names and Ir from the compare log's text. A header rule that stops
    matching drops cases from the history record while the gate stays green, so pin it to real output."""

    def test_every_case_records_once_under_its_id(self) -> None:
        src = SCRIPT.read_text()
        start = src.index("harvest_history() {")
        body = src[start : src.index("\n}\n", start) + 3]
        with tempfile.TemporaryDirectory() as tmp:
            log = pathlib.Path(tmp) / "compare.log"
            log.write_text(_HARVEST_LOG)
            record = pathlib.Path(tmp) / "record.jsonl"
            record.write_text("")
            prog = f'{body}\nRECORD="$1"; H_SHA=s; H_FULL=f; H_DATE=d; harvest_history "$2" "$3"\n'
            subprocess.run(["bash", "-c", prog, "harvest", str(record), "macro_iai", str(log)], check=True)
            rows = [json.loads(line) for line in record.read_text().splitlines()]
        self.assertEqual(
            [(r["case"], r["ir"]) for r in rows],
            [("auto-filter", 43605487), ("reverb", 10891636), ("wide_n2048", 53847764)],
        )


if __name__ == "__main__":
    unittest.main()
