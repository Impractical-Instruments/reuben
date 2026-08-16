#!/usr/bin/env python3
"""Tests for `scripts/hooks/dispatch`, the hook-set chainer.

Why this file exists: `dispatch` is the only thing standing between a registered check and never
running, and it runs in front of every commit and push in every clone. Every other guard in this
repo ships the tests that hold it; this one gates the guards themselves.

Each test builds a throwaway git repo with a synthetic registry, so nothing here depends on this
repo's own checks — which is also the property under test. `dispatch` is meant to know nothing
about the repository it dispatches for, and a suite that had to install a Rust toolchain to
exercise it would be evidence against that.
"""

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

DISPATCH = Path(__file__).resolve().parent.parent / "scripts/hooks" / "dispatch"


class DispatchHarness(unittest.TestCase):
    """A git repo whose `core.hooksPath` is a registry this test wrote."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="hook-dispatch-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.hooks = self.tmp / "scripts/hooks"
        (self.hooks / "pre-commit.d").mkdir(parents=True)
        shutil.copy2(DISPATCH, self.hooks / "dispatch")
        self.stub("pre-commit")
        self.run_git("init", "-q", ".")
        self.run_git("config", "core.hooksPath", "scripts/hooks")
        self.run_git("config", "user.email", "test@example.invalid")
        self.run_git("config", "user.name", "test")

    def stub(self, hook):
        path = self.hooks / hook
        path.write_text(
            '#!/bin/sh\nexec "$(dirname -- "$0")/dispatch" %s "$@"\n' % hook)
        path.chmod(0o755)

    def check(self, name, body, executable=True, hook="pre-commit"):
        """Register a check, and return the marker file it writes when it runs."""
        marker = self.tmp / ("ran-" + name.replace("/", "_"))
        path = self.hooks / (hook + ".d") / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/bin/sh\ntouch '%s'\n%s\n" % (marker, body))
        path.chmod(0o755 if executable else 0o644)
        return marker

    def env(self):
        """A git environment that ignores whoever is running the suite.

        Without this the tests inherit the developer's global config, and `commit.gpgsign = true`
        — an ordinary setting to have — fails most of them with a signing error that looks nothing
        like the thing under test.
        """
        return {**os.environ,
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_CONFIG_SYSTEM": os.devnull,
                "HOME": str(self.tmp)}

    def run_git(self, *args):
        return subprocess.run(["git", "-C", str(self.tmp), *args],
                              capture_output=True, text=True, env=self.env())

    def commit(self):
        return self.run_git("commit", "--allow-empty", "-q", "-m", "probe")

    def run_hook(self, hook="pre-commit", stdin="", args=()):
        """Invoke the stub the way git does: from the work tree, by relative path."""
        return subprocess.run([os.path.join("scripts/hooks", hook), *args],
                              cwd=self.tmp, input=stdin,
                              capture_output=True, text=True, env=self.env())


class TestEveryRegisteredCheckRuns(DispatchHarness):

    def test_all_checks_run_in_name_order(self):
        # The whole point of the change this file arrived with: a second check does not displace
        # the first, and the order is the filename's.
        order = self.tmp / "order"
        for name in ("30-c", "10-a", "20-b"):
            self.check(name, "printf '%s ' '{}' >> '{}'".format(name, order))
        self.assertEqual(self.commit().returncode, 0)
        self.assertEqual(order.read_text(), "10-a 20-b 30-c ")

    def test_an_empty_registry_is_a_no_op(self):
        self.assertEqual(self.commit().returncode, 0)

    def test_a_missing_registry_directory_is_a_no_op(self):
        shutil.rmtree(self.hooks / "pre-commit.d")
        self.assertEqual(self.commit().returncode, 0)

    def test_a_check_runs_from_the_work_tree_root(self):
        # Checks in this repo call `python3 scripts/...` on a relative path, so this is load-bearing.
        where = self.tmp / "where"
        self.check("10-pwd", "pwd > '%s'" % where)
        self.commit()
        self.assertEqual(Path(where.read_text().strip()).resolve(), self.tmp.resolve())

    def test_a_check_may_stage_a_file(self):
        # `30-rules-index` regenerates the derived index and re-stages it inside the commit.
        (self.tmp / "generated.txt").write_text("x\n")
        self.check("10-stage", "git add generated.txt")
        self.assertEqual(self.commit().returncode, 0)
        self.assertIn("generated.txt", self.run_git("show", "--name-only", "--format=").stdout)


class TestFailureStopsTheHook(DispatchHarness):

    def test_the_first_failure_aborts_and_later_checks_do_not_run(self):
        self.check("10-fail", "exit 1")
        later = self.check("20-later", "")
        result = self.commit()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(later.exists(), "a check after the failure ran")

    def test_the_failing_check_is_named(self):
        self.check("10-fail", "exit 1")
        self.assertIn("10-fail", self.commit().stderr)

    def test_a_read_only_check_failing_leaves_a_later_writer_untouched(self):
        # The reason the numeric prefix orders read-only checks below writers: an aborted commit
        # must not leave the author a staged file they never touched.
        self.check("10-fail", "exit 1")
        self.check("20-writer", "echo mutated > '%s'" % (self.tmp / "victim.txt"))
        self.commit()
        self.assertFalse((self.tmp / "victim.txt").exists())


class TestACheckThatCannotRunSaysSo(DispatchHarness):

    def test_a_non_executable_check_is_an_error_not_a_skip(self):
        # The defect this hook set exists to prevent, at fragment scale: a guard that never runs is
        # indistinguishable from one that passed.
        self.check("10-noexec", "", executable=False)
        result = self.commit()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not executable", result.stderr)
        self.assertIn("10-noexec", result.stderr)

    def test_a_non_executable_check_does_not_let_later_checks_mask_it(self):
        self.check("10-noexec", "", executable=False)
        later = self.check("20-later", "")
        self.commit()
        self.assertFalse(later.exists())

    def test_an_editor_backup_is_skipped_rather_than_run(self):
        # A `~` copy inherits the executable bit and would otherwise run as a stale duplicate.
        backup = self.check("10-real~", "")
        self.assertEqual(self.commit().returncode, 0)
        self.assertFalse(backup.exists())

    def test_a_directory_in_the_registry_is_not_a_check(self):
        (self.hooks / "pre-commit.d" / "10-adir").mkdir()
        self.assertEqual(self.commit().returncode, 0)

    def test_a_broken_symlink_is_an_error_not_a_skip(self):
        # It satisfies neither -f nor -e, so the obvious guard steps over it exactly the way it
        # steps over an empty registry — an entry plainly sitting there, silently never run.
        (self.hooks / "pre-commit.d" / "10-dangling").symlink_to("/nonexistent/nothing")
        later = self.check("20-later", "")
        result = self.commit()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("10-dangling", result.stderr)
        self.assertFalse(later.exists())

    def test_a_dot_prefixed_entry_is_an_error_not_an_off_switch(self):
        # The shell's glob cannot see it, so renaming a check to `.name` would disable it in
        # silence — the one thing a registry must not offer.
        hidden = self.hooks / "pre-commit.d" / ".10-hidden"
        hidden.write_text("#!/bin/sh\ntrue\n")
        hidden.chmod(0o755)
        result = self.commit()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("10-hidden", result.stderr)


class TestStdinIsReplayedToEveryCheck(DispatchHarness):

    def test_every_check_sees_the_same_stdin(self):
        # git hands pre-push its pushed refs on stdin, and stdin can only be read once. Without the
        # replay the second check reads EOF and quietly concludes there is nothing to inspect.
        a, b = self.tmp / "a.txt", self.tmp / "b.txt"
        self.check("10-a", "cat > '%s'" % a)
        self.check("20-b", "cat > '%s'" % b)
        payload = "refs/heads/x 1111111 refs/heads/x 0000000\n"
        result = self.run_hook(stdin=payload)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(a.read_text(), payload)
        self.assertEqual(b.read_text(), payload)

    def test_the_hooks_arguments_reach_every_check(self):
        a, b = self.tmp / "args-a", self.tmp / "args-b"
        self.check("10-a", "printf '%s' \"$*\" > '{}'".format(a))
        self.check("20-b", "printf '%s' \"$*\" > '{}'".format(b))
        self.run_hook(args=("origin", "git@example.invalid:x.git"))
        self.assertEqual(a.read_text(), "origin git@example.invalid:x.git")
        self.assertEqual(b.read_text(), a.read_text())

    def test_the_capture_file_exists_while_running_and_is_gone_after(self):
        # Asserting only "nothing left behind" passes just as well when the capture never happened,
        # which is every way this could break. Record what existed DURING the run too.
        seen = self.tmp / "seen-during"
        self.check("10-a", "ls .git/githook-stdin.* > '%s' 2>&1; cat > /dev/null" % seen)
        self.run_hook(stdin="payload\n")
        self.assertEqual(len(seen.read_text().split()), 1,
                         "expected exactly one capture file while the check ran")
        self.assertEqual(list((self.tmp / ".git").glob("githook-stdin.*")), [])

    def test_a_stale_capture_from_a_killed_run_is_reaped(self):
        # A SIGKILL cannot run a trap, so these would otherwise accumulate forever: `git gc` and
        # `git worktree prune` do not know about them.
        stale = self.tmp / ".git" / "githook-stdin.999999"
        stale.write_text("orphan\n")
        self.check("10-a", "cat > /dev/null")
        self.run_hook(stdin="payload\n")
        self.assertFalse(stale.exists())


class TestDispatchIsRepositoryAgnostic(DispatchHarness):

    def test_it_refuses_to_run_without_a_hook_name(self):
        result = subprocess.run([str(self.hooks / "dispatch")], cwd=self.tmp,
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("without a hook name", result.stderr)

    def test_a_new_hook_type_needs_only_a_stub_and_a_directory(self):
        # The property the extraction seam rests on: dispatch names no hook and no repo.
        (self.hooks / "commit-msg.d").mkdir()
        self.stub("commit-msg")
        marker = self.check("10-msg", "", hook="commit-msg")
        result = self.run_hook(hook="commit-msg", args=(".git/COMMIT_EDITMSG",))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(marker.exists())

    def test_it_carries_no_pointer_into_this_repository(self):
        # The first version of this test was a blocklist of six words, and it passed while
        # `dispatch` printed a hard-coded `./scripts/install-hooks.sh`. A guard that asks "does the
        # wrong word appear" is only ever as good as its enumeration. Assert the SHAPE instead: no
        # path that is not the dispatcher's own directory.
        text = DISPATCH.read_text()

        # Every path it may mention is either derived from a variable at run time, one of the two
        # absolute paths every POSIX system has, or a bare structural fragment of the registry
        # convention. A literal directory NAME is the thing that cannot travel.
        structural = {"", ".", "..", ".d"}
        for token in sorted(set(re.findall(r"[\w.$-]*/[\w./$-]*", text))):
            if "$" in token or token in ("/bin/sh", "/dev/null"):
                continue
            named = [seg for seg in token.split("/") if seg not in structural]
            self.assertEqual(
                named, [],
                "dispatch names the literal path `%s`; only its own directory travels" % token)

    def test_it_names_no_toolchain_or_check(self):
        text = DISPATCH.read_text().lower()
        for token in ("cargo", "python", "rust", "crates", "reuben", "docs"):
            self.assertNotIn(token, text,
                             "dispatch names `%s`; it must stay repo-agnostic" % token)


class TestASignalDoesNotMisblameTheNextCheck(DispatchHarness):

    def test_a_killed_check_exits_rather_than_blaming_its_successor(self):
        # Without the traps EXITING, cleanup deletes the replay file and the loop carries on — so
        # the NEXT check dies on a missing stdin and is reported as the failure, blamed for a run
        # it never had. Round one shipped that bug; nothing caught it until it was written down.
        self.check("10-slow", "kill -TERM $PPID; sleep 5")
        later = self.check("20-later", "")
        result = self.run_hook(stdin="refs/heads/x 1 refs/heads/x 0\n")
        self.assertEqual(result.returncode, 143,
                         "a TERMed hook must exit 143, not fall through the loop")
        self.assertFalse(later.exists(), "the check after the signal ran")
        self.assertNotIn("20-later", result.stderr, "the successor was blamed for the signal")
        self.assertEqual(list((self.tmp / ".git").glob("githook-stdin.*")), [])


if __name__ == "__main__":
    unittest.main()
