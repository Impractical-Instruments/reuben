#!/usr/bin/env python3
r"""Unit tests for check_adr_numbers — the "a number is issued once" guard.

Two layers, because the guard has two halves that fail differently.

`analyse()` is pure — enumerated history in, findings out — so most cases are a three-line fixture
and assert an exact problem count. A guard that over-reports gets switched off and one that
under-reports is decoration, so the counts are asserted rather than truthiness.

The rest builds real throwaway git repositories, because the half that reads git is the half with
the interesting failures: rename detection (the defect the guard exists not to inherit), renames
hidden inside merge commits, and the shallow clone (the failure that would otherwise be silent
green). Those cannot be faked with a fixture list — faking them is what would let them rot.

`MainTest` covers the entry point, because two mutations survive a suite that only tests the
internals: an exit code wired to 0, and a baseline reported as the highest rather than the next.
The first is a guard that does not guard.

Decision-record numbers are written as bare four-digit strings throughout; the reference-linter
bans the spelled-out token in code, and this file is code.
"""
from __future__ import annotations

import contextlib
import io
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import check_adr_numbers


def adr(n: int, slug: str) -> str:
    return f"{n:04d}-{slug}.md"


class AnalyseTest(unittest.TestCase):
    """The pure half: history and live listing in, findings out."""

    def analyse(self, renames=(), additions=(), live=()):
        return check_adr_numbers.analyse(list(renames), list(additions), list(live))

    def test_empty_corpus_is_green(self):
        problems, highest = self.analyse()
        self.assertEqual(problems, [])
        self.assertIsNone(highest)

    def test_a_clean_corpus_is_green(self):
        names = [adr(1, "first"), adr(2, "second"), adr(3, "third")]
        problems, highest = self.analyse(additions=names, live=names)
        self.assertEqual(problems, [])
        self.assertEqual(highest, 3)

    def test_an_absorbed_adr_still_counts_toward_the_baseline(self):
        # The fold deletes the file. "Highest on disk" would say 2 here, which is the wrong
        # baseline and the reason the rule is stated in terms of history at all.
        problems, highest = self.analyse(
            additions=[adr(1, "a"), adr(2, "b"), adr(3, "c")], live=[adr(2, "b")])
        self.assertEqual(problems, [])
        self.assertEqual(highest, 3)

    def test_two_live_files_on_one_number_are_flagged_twice(self):
        names = [adr(78, "one"), adr(78, "two")]
        problems, _ = self.analyse(additions=names, live=names)
        self.assertEqual(len(problems), 2)
        self.assertIn("2 live files", problems[0])
        self.assertIn("2 distinct ADRs", problems[1])

    def test_a_renumber_clears_the_finding(self):
        # The repair: one of the pair moves to a fresh number. History keeps both additions
        # forever, so a guard keyed on "ever carried" would stay red here permanently.
        problems, highest = self.analyse(
            renames=[(adr(78, "one"), adr(83, "one"))],
            additions=[adr(78, "one"), adr(78, "two"), adr(83, "one")],
            live=[adr(78, "two"), adr(83, "one")])
        self.assertEqual(problems, [])
        self.assertEqual(highest, 83)

    def test_reusing_a_folded_number_is_flagged(self):
        # The case the ticket calls the one that matters: 60's file was absorbed away, so nothing
        # on disk says the number is taken — but a rationale may already point at it.
        problems, _ = self.analyse(
            additions=[adr(60, "absorbed"), adr(60, "brand-new")],
            live=[adr(60, "brand-new")])
        self.assertEqual(len(problems), 1)
        self.assertIn("distinct ADRs", problems[0])
        self.assertIn("(not in this working tree)", problems[0])

    def test_a_renamed_destination_counts_as_issued(self):
        # 74 exists ONLY as a rename destination. An additions-only enumeration never sees it, so
        # this is the rename-blindness case, isolated.
        problems, highest = self.analyse(
            renames=[(adr(68, "swap"), adr(74, "swap"))],
            additions=[adr(68, "swap")],
            live=[adr(74, "swap")])
        self.assertEqual(problems, [])
        self.assertEqual(highest, 74)

    def test_taking_a_renamed_destinations_number_after_a_fold_is_flagged(self):
        # 74's ADR was absorbed; a new decision takes 74. Only a rename-aware enumeration knows
        # 74 was ever issued at all.
        problems, _ = self.analyse(
            renames=[(adr(68, "swap"), adr(74, "swap"))],
            additions=[adr(68, "swap"), adr(74, "newcomer")],
            live=[adr(74, "newcomer")])
        self.assertEqual(len(problems), 1)
        self.assertIn("0074", problems[0])

    def test_a_vacated_number_still_raises_the_baseline(self):
        # 95 was carried, then moved down to 92. The next ADR takes 96, not 93.
        _, highest = self.analyse(
            renames=[(adr(95, "x"), adr(92, "x"))],
            additions=[adr(95, "x")], live=[adr(92, "x")])
        self.assertEqual(highest, 95)

    def test_a_known_collision_is_explained(self):
        names = sorted(check_adr_numbers.KNOWN_COLLISIONS[31])
        problems, _ = self.analyse(additions=names, live=[])
        self.assertEqual(problems, [])

    def test_a_new_file_on_a_known_collision_number_is_still_flagged(self):
        # The entry names an exact set. It grandfathers what happened; it does not open the number.
        names = sorted(check_adr_numbers.KNOWN_COLLISIONS[31]) + [adr(31, "newcomer")]
        problems, _ = self.analyse(additions=names, live=[adr(31, "newcomer")])
        self.assertEqual(len(problems), 1)
        self.assertIn("distinct ADRs", problems[0])

    def test_a_known_collision_never_exempts_two_live_files(self):
        # Check 1 is exception-free by construction: the same two filenames, both on disk.
        names = sorted(check_adr_numbers.KNOWN_COLLISIONS[35])
        problems, _ = self.analyse(additions=names, live=names)
        self.assertEqual(len(problems), 1)
        self.assertIn("2 live files", problems[0])

    def test_a_number_carried_by_one_adr_twice_is_not_a_collision(self):
        # A slug rewrite that keeps the number is one identity, not two.
        problems, _ = self.analyse(
            renames=[(adr(40, "old-title"), adr(40, "new-title"))],
            additions=[adr(40, "old-title")], live=[adr(40, "new-title")])
        self.assertEqual(problems, [])

    def test_a_rename_chain_resolves_to_its_last_hop(self):
        problems, highest = self.analyse(
            renames=[(adr(10, "x"), adr(20, "x")), (adr(20, "x"), adr(30, "x"))],
            additions=[adr(10, "x"), adr(11, "y")],
            live=[adr(30, "x"), adr(11, "y")])
        self.assertEqual(problems, [])
        self.assertEqual(highest, 30)

    def test_a_fully_deleted_renumbered_chain_frees_its_old_number(self):
        # THE central design claim, and nothing else in this file tests it: "last carried", not
        # "ever carried". Both files here are gone — one absorbed at 44, the other renumbered to 47
        # and absorbed there — which is this repo's own 0044 history in miniature. Under "ever
        # carried" 44 has two holders and this reds; under "last carried" the repair stands and it
        # is green. Delete the `group - sources` fallback in `analyse()` and this is the test that
        # notices.
        problems, _ = self.analyse(
            renames=[(adr(44, "beta"), adr(47, "beta"))],
            additions=[adr(44, "alpha"), adr(44, "beta")],
            live=[])
        self.assertEqual(problems, [])

    def test_a_number_freed_by_a_chain_is_reported_when_reused(self):
        problems, _ = self.analyse(
            renames=[(adr(10, "x"), adr(20, "x"))],
            additions=[adr(10, "x"), adr(10, "later")],
            live=[adr(20, "x"), adr(10, "later")])
        # 10's terminal holders are the reused newcomer and nothing else — the chain moved the
        # original off it — so this is the documented gap, not a finding.
        self.assertEqual(problems, [])


class GitHarness(unittest.TestCase):
    """A throwaway git repository with a `docs/adr` in it. Carries no tests of its own."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="adr-numbers-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        (self.tmp / check_adr_numbers.ADR_DIR).mkdir(parents=True)
        self.git("init", "-q", "-b", "main", ".")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "user.name", "test")
        self.git("config", "commit.gpgsign", "false")

    def env(self):
        """Ignore whoever is running the suite: a global `commit.gpgsign` would fail every
        commit below with an error that looks nothing like the thing under test."""
        clean = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        clean["GIT_CONFIG_GLOBAL"] = str(self.tmp / "gitconfig-none")
        clean["GIT_CONFIG_SYSTEM"] = str(self.tmp / "gitconfig-none")
        return clean

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.tmp), *args],
                              capture_output=True, text=True, check=True, env=self.env())

    def write(self, name, body="a decision\n"):
        # `git rm` of the last file takes the directory with it, so recreate rather than assume.
        directory = self.tmp / check_adr_numbers.ADR_DIR
        directory.mkdir(parents=True, exist_ok=True)
        (directory / name).write_text(body, encoding="utf-8")

    def commit(self, message):
        self.git("add", "-A", ".")
        self.git("commit", "-q", "-m", message)

    def run_guard(self):
        return check_adr_numbers.collect_problems(str(self.tmp))


class GitRepoTest(GitHarness):
    """The half that reads git: rename detection, merge commits, and a history that cannot be
    trusted."""

    def test_a_clean_repo_is_green(self):
        self.write(adr(1, "first"))
        self.write(adr(2, "second"))
        self.commit("two decisions")
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertEqual(highest, 2)

    def test_git_reports_a_renumber_as_a_rename_and_the_guard_follows_it(self):
        # The whole reason this test builds a real repo: it is git's rename detection under test,
        # not a fixture's. Move the file, and the destination number must count as issued.
        body = "a decision\n" + ("filler line\n" * 40)
        self.write(adr(68, "swap"), body)
        self.commit("a decision")
        self.git("mv", f"{check_adr_numbers.ADR_DIR}/{adr(68, 'swap')}",
                 f"{check_adr_numbers.ADR_DIR}/{adr(74, 'swap')}")
        self.commit("renumber it")
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertEqual(highest, 74, "a rename destination is an issued number")

    def test_reusing_a_renamed_destination_after_a_fold_reds(self):
        body = "a decision\n" + ("filler line\n" * 40)
        self.write(adr(68, "swap"), body)
        self.commit("a decision")
        self.git("mv", f"{check_adr_numbers.ADR_DIR}/{adr(68, 'swap')}",
                 f"{check_adr_numbers.ADR_DIR}/{adr(74, 'swap')}")
        self.commit("renumber it")
        self.git("rm", "-q", f"{check_adr_numbers.ADR_DIR}/{adr(74, 'swap')}")
        self.commit("absorb it")
        self.write(adr(74, "newcomer"))
        self.commit("a newcomer takes the freed number")
        problems, _ = self.run_guard()
        self.assertEqual(len(problems), 1)
        self.assertIn("0074", problems[0])

    def test_the_working_tree_is_what_counts_as_live(self):
        # The pre-commit fragment runs before anything is committed, so an uncommitted duplicate
        # has to red or the local check is worthless.
        self.write(adr(5, "first"))
        self.commit("a decision")
        self.write(adr(5, "second"))
        problems, _ = self.run_guard()
        self.assertTrue(any("2 live files" in p for p in problems), problems)

    def test_a_staged_renumber_clears_the_finding_before_it_is_committed(self):
        # The repair has to pass at the moment it is made. The pre-commit fragment runs with the
        # `git mv` staged and nothing committed, so a guard reading only committed history would
        # red on the exact fix it demanded — and a check that blocks its own remedy gets bypassed.
        body = "a decision\n" + ("filler line\n" * 40)
        self.write(adr(78, "one"), body)
        self.write(adr(78, "two"))
        self.commit("two decisions, one number")
        self.assertTrue(self.run_guard()[0], "the collision must be reported before the repair")
        self.git("mv", f"{check_adr_numbers.ADR_DIR}/{adr(78, 'one')}",
                 f"{check_adr_numbers.ADR_DIR}/{adr(83, 'one')}")
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertEqual(highest, 83)

    def test_an_unstaged_deletion_does_not_free_the_number(self):
        # `rm` without `git rm` takes the file out of the live corpus. The number stays issued —
        # the deleted ADR's provenance line, if it ever gets one, still names it.
        self.write(adr(1, "first"))
        self.commit("a decision")
        (self.tmp / check_adr_numbers.ADR_DIR / adr(1, "first")).unlink()
        self.write(adr(1, "second"))
        problems, _ = self.run_guard()
        self.assertEqual(len(problems), 1)
        self.assertIn("distinct ADRs", problems[0])

    def _branching_collision(self):
        """A base commit, a `topic` branch and `main` each taking 0005. Returns the filler body."""
        body = "a decision\n" + ("filler line\n" * 40)
        self.write(adr(1, "base"), body)
        self.commit("a base decision")
        self.git("checkout", "-q", "-b", "topic")
        self.write(adr(5, "topic"), body)
        self.commit("topic takes 0005")
        self.git("checkout", "-q", "main")
        self.write(adr(5, "integration"), body)
        self.commit("integration takes 0005 too")
        return body

    def _renumber_into_the_merge(self):
        """Resolve the duplicate by amending the renumber into the merge commit itself."""
        self.git("mv", f"{check_adr_numbers.ADR_DIR}/{adr(5, 'topic')}",
                 f"{check_adr_numbers.ADR_DIR}/{adr(6, 'topic')}")
        self.git("commit", "-q", "--amend", "--no-edit")

    def test_a_renumber_made_inside_a_merge_is_seen(self):
        # `git log` emits NO diff for a merge commit by default, so a rename made while resolving
        # one leaves no record — and resolving a merge is exactly when a duplicate number becomes
        # visible. Without --diff-merges the repair is invisible: the hook passes (the rename is in
        # `git diff HEAD`) and CI then reds forever with nothing to fix.
        self._branching_collision()
        self.git("checkout", "-q", "topic")
        self.git("merge", "-q", "--no-ff", "main", "-m", "merge the integration branch in")
        self.assertTrue(self.run_guard()[0], "the merge must expose the duplicate first")
        self._renumber_into_the_merge()
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertEqual(highest, 6)

    def test_a_renumber_inside_a_merge_from_the_second_parent_is_a_known_gap(self):
        # PINS A LIMITATION ON PURPOSE, so read the reason before you "fix" it. Here the merge runs
        # the other way — the topic branch is merged INTO the integration branch — so the renumbered
        # file arrives from the SECOND parent and the rename lives only in a diff `first-parent`
        # never reads. The guard reds on a correctly repaired tree.
        #
        # `--diff-merges=separate` would read it, and is disqualified: it scores renames across
        # cross-parent diffs and invents edges that DELETE real collisions from the report (see the
        # module docstring, and `test_a_cross_parent_diff_must_not_invent_a_rename` below). A loud
        # false positive with a workflow escape beats a silent false negative in the guard's core
        # job. The escape: do the renumber as its own commit AFTER the merge, never inside it.
        #
        # Switching modes turns this test red. That is the point — the switch cannot happen without
        # someone reading why it was rejected.
        self._branching_collision()
        self.git("merge", "-q", "--no-ff", "topic", "-m", "merge the topic branch in")
        self.assertTrue(self.run_guard()[0], "the merge must expose the duplicate first")
        self._renumber_into_the_merge()
        problems, _ = self.run_guard()
        self.assertEqual(len(problems), 1, "the documented gap: the repair is invisible here")
        self.assertIn("0005", problems[0])

    def test_a_renumber_in_its_own_commit_after_a_merge_is_always_seen(self):
        # The escape hatch the gap above names, in the direction that has one. Same merge, but the
        # renumber is its own commit — an ordinary two-parent-free diff, visible in every mode.
        self._branching_collision()
        self.git("merge", "-q", "--no-ff", "topic", "-m", "merge the topic branch in")
        self.git("mv", f"{check_adr_numbers.ADR_DIR}/{adr(5, 'topic')}",
                 f"{check_adr_numbers.ADR_DIR}/{adr(6, 'topic')}")
        self.commit("renumber the topic decision, on its own")
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertEqual(highest, 6)

    def test_a_cross_parent_diff_must_not_invent_a_rename(self):
        # The reason `--diff-merges=separate` is not used, as a test rather than as a paragraph.
        # ADRs share enough scaffolding that git scores an absorbed one and an unrelated new one as
        # a 95%-similar rename when the two are compared across a merge's parents. That fake edge
        # welds two identities together, moves the absorbed ADR's terminal onto the newcomer's
        # number, and makes a REAL reuse vanish from the report. Under `separate` this test goes
        # green-with-no-findings — which is the failure, not the pass.
        def boiler(number, what):
            head = f"# a decision {number:04d} - {what}\n\n## Status\n\nAccepted\n\n## Context\n\n"
            body = "".join(f"Context line {i}, shared scaffolding across every record.\n"
                           for i in range(12))
            tail = f"\n## Decision\n\nWe decided {what}.\n\n## Consequences\n\n" + "".join(
                f"Consequence line {i}, shared scaffolding across every record.\n"
                for i in range(12))
            return head + body + tail

        self.write(adr(2, "old"), boiler(2, "the old thing"))
        self.commit("0002 exists")
        self.git("checkout", "-q", "-b", "topic")
        self.write(adr(2, "newcomer"), boiler(2, "a brand new unrelated thing"))
        self.commit("the topic branch reuses 0002 — this is the collision")
        self.git("checkout", "-q", "main")
        self.git("rm", "-q", f"{check_adr_numbers.ADR_DIR}/{adr(2, 'old')}")
        self.commit("absorb the old 0002")
        self.write(adr(5, "devnew"), boiler(5, "something else entirely"))
        self.commit("the integration branch adds 0005")
        self.git("merge", "-q", "--no-ff", "topic", "-m", "merge the topic branch in")
        problems, _ = self.run_guard()
        self.assertEqual(len(problems), 1, "the reuse of 0002 must survive the merge")
        self.assertIn("0002", problems[0])

    def test_a_promotion_from_an_unnumbered_name_counts_as_an_issue(self):
        # A rename whose SOURCE is not an ADR name — a draft in the directory promoted to a numbered
        # decision — and then absorbed, so the ONLY record that 12 was ever issued is that rename's
        # destination. Dead-lettering the record because the source did not parse loses the number
        # entirely, and the next author is handed one a rationale may already point at.
        body = "a draft\n" + ("filler line\n" * 40)
        (self.tmp / check_adr_numbers.ADR_DIR / "draft-notes.md").write_text(body, encoding="utf-8")
        self.write(adr(1, "first"))
        self.commit("a decision and a draft")
        self.git("mv", f"{check_adr_numbers.ADR_DIR}/draft-notes.md",
                 f"{check_adr_numbers.ADR_DIR}/{adr(12, 'promoted')}")
        self.commit("the draft becomes a numbered decision")
        self.git("rm", "-q", f"{check_adr_numbers.ADR_DIR}/{adr(12, 'promoted')}")
        self.commit("absorb it")
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertEqual(highest, 12, "the promotion is the only record that 12 was issued")
        # And the number stays taken: a newcomer claiming it is the collision, not a fresh start.
        self.write(adr(12, "newcomer"))
        self.commit("a newcomer takes 0012")
        problems, _ = self.run_guard()
        self.assertEqual(len(problems), 1)
        self.assertIn("0012", problems[0])

    def test_a_failing_git_log_is_reported_not_ignored(self):
        # An enumeration that failed and a corpus with no history look identical from here, and one
        # of them must never be read as "no numbers were issued".
        self.write(adr(1, "first"))
        self.commit("a decision")
        real = check_adr_numbers._git

        def broken(root, *args):
            if args[:1] == ("log",):
                return subprocess.CompletedProcess(args, 128, "", "fatal: not a valid object name")
            return real(root, *args)

        check_adr_numbers._git = broken
        self.addCleanup(setattr, check_adr_numbers, "_git", real)
        problems, highest = self.run_guard()
        self.assertEqual(len(problems), 1)
        self.assertIn("cannot enumerate", problems[0])
        self.assertIsNone(highest)

    def test_a_number_that_is_not_four_digits_is_not_an_adr(self):
        # The width is the grammar. A three-digit prefix is not an ADR, and a five-digit one read as
        # four would invent a baseline out of a file nobody numbered.
        self.write(adr(1, "first"))
        for name in ("012-short.md", "00123-long.md", "notes.md"):
            (self.tmp / check_adr_numbers.ADR_DIR / name).write_text("x\n", encoding="utf-8")
        self.commit("a decision and three files that are not decisions")
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertEqual(highest, 1)

    def test_a_shallow_clone_fails_loudly(self):
        self.write(adr(1, "first"))
        self.commit("a decision")
        self.write(adr(2, "second"))
        self.commit("another decision")
        clone = Path(tempfile.mkdtemp(prefix="adr-numbers-shallow-"))
        self.addCleanup(shutil.rmtree, clone, ignore_errors=True)
        target = clone / "repo"
        subprocess.run(["git", "clone", "-q", "--depth", "1", "--no-local",
                        f"file://{self.tmp}", str(target)],
                       check=True, capture_output=True, text=True, env=self.env())
        problems, highest = check_adr_numbers.collect_problems(str(target))
        self.assertEqual(len(problems), 1)
        self.assertIn("SHALLOW", problems[0])
        self.assertIsNone(highest)

    def test_a_history_that_records_no_adr_fails_loudly(self):
        # Not shallow, but equally unusable: the files exist and nothing was ever committed, so
        # the enumeration is empty for a reason that is not "no numbers were issued".
        self.write(adr(1, "first"))
        problems, _ = check_adr_numbers.collect_problems(str(self.tmp))
        self.assertEqual(len(problems), 1)
        self.assertIn("cannot say which numbers", problems[0])

    def test_a_directory_that_is_not_a_repo_fails_loudly(self):
        plain = Path(tempfile.mkdtemp(prefix="adr-numbers-plain-"))
        self.addCleanup(shutil.rmtree, plain, ignore_errors=True)
        problems, _ = check_adr_numbers.collect_problems(str(plain))
        self.assertEqual(len(problems), 1)
        self.assertIn("not a git work tree", problems[0])

    def test_a_repo_without_the_directory_is_green(self):
        self.git("rm", "-rq", "--ignore-unmatch", check_adr_numbers.ADR_DIR)
        shutil.rmtree(self.tmp / check_adr_numbers.ADR_DIR, ignore_errors=True)
        (self.tmp / "README.md").write_text("no decisions here\n", encoding="utf-8")
        self.commit("a repo with no decision records")
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertIsNone(highest)

    def test_a_non_numbered_file_in_the_directory_is_ignored(self):
        self.write(adr(1, "first"))
        (self.tmp / check_adr_numbers.ADR_DIR / "README.md").write_text("index\n", encoding="utf-8")
        self.commit("a decision and an index")
        problems, highest = self.run_guard()
        self.assertEqual(problems, [])
        self.assertEqual(highest, 1)


class MainTest(GitHarness):
    """The entry point, which the rest of the suite never touches.

    Two mutations survive a suite that stops at `collect_problems`, and one of them is fatal: an
    exit code hard-wired to 0 turns every check in this file into a report nobody is gated on. The
    other is the baseline printing the highest issued number instead of the next one, which quietly
    hands the next author a number that is already taken.
    """

    def run_main(self):
        """`(exit_code, stderr)` from `main()` over this repo."""
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = check_adr_numbers.main(str(self.tmp))
        return code, err.getvalue()

    def test_main_exits_nonzero_and_names_the_collision(self):
        self.write(adr(5, "first"))
        self.write(adr(5, "second"))
        self.commit("two decisions, one number")
        code, err = self.run_main()
        self.assertEqual(code, 1)
        self.assertIn("2 live files", err)
        self.assertIn("2 problem(s)", err)

    def test_main_exits_zero_on_a_clean_corpus(self):
        self.write(adr(5, "only"))
        self.commit("a decision")
        code, err = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("0 problem(s)", err)

    def test_main_reports_the_next_number_not_the_highest(self):
        self.write(adr(7, "first"))
        self.commit("a decision")
        _, err = self.run_main()
        self.assertIn("highest number ever issued 0007", err)
        self.assertIn("the next ADR takes 0008", err)

    def test_main_says_so_when_there_are_no_decisions(self):
        shutil.rmtree(self.tmp / check_adr_numbers.ADR_DIR, ignore_errors=True)
        (self.tmp / "README.md").write_text("no decisions here\n", encoding="utf-8")
        self.commit("a repo with no decision records")
        code, err = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("no ADRs found", err)


if __name__ == "__main__":
    unittest.main()
