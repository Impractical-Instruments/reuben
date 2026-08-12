#!/usr/bin/env python3
"""Check that every generated file in this repository still matches the digest it carries.

Some documents here are generated rather than hand-written. Each generated span records a
`sha256=` digest of itself, on the line that delimits it — a `<!-- GENERATED … -->` header
for a whole generated file, an `<!-- ii:begin <id> … -->` marker for a generated region
inside a hand-written one. This program walks the tree, recomputes each digest from the
file alone, and compares.

WHAT A MATCH PROVES. Nobody hand-edited the generated span.

WHAT A MATCH DOES NOT PROVE. That the span still matches the template and the manifest it
was produced from. This is tamper-detection, not staleness-detection: strictly weaker than
the generator's own `--check`, which has the templates and the manifest that this program
deliberately carries neither of. A file whose template changed upstream, and which nobody
has touched since, verifies perfectly here and is out of date.

WHAT IT NEVER LOOKS FOR. Files that are missing. It checks the artifacts it finds and asserts
nothing about which ones ought to exist — so deleting an artifact's header line, rather than
editing its body, removes it from the run rather than failing it. What stands against that is
the count: every run reports how many artifacts it verified, and `--require-artifacts` turns
"none at all" into a failure.

PROVENANCE. This file is authored in `Impractical-Instruments/agent-tools` and vendored into
the repos that run it. If you are reading it anywhere else, you are reading a copy: change it
upstream, in that repo, or the next refresh overwrites your edit. That name records where the
source is maintained and nothing more — the copy in front of you is covered by the licence of
the repository you are reading it in.

Usage: python3 ii_verify.py [-v] [--require-artifacts] [ROOT]   ROOT defaults to `.`
Exit:  0 clean · 1 problems found · 2 could not safely run (usage, an unreadable root or file)

Standard library only, Python 3.8+. No network, no configuration, no plugin. Deterministic:
the same tree gives the same output in the same order.
"""
from __future__ import annotations

import hashlib
import os
import re
import subprocess
import sys
from pathlib import Path

PROGRAM = "ii_verify"

USAGE = """usage: python3 ii_verify.py [-v|--verbose] [--require-artifacts] [ROOT]

Recompute the `sha256=` digest carried by every generated artifact under ROOT (default `.`)
and compare it to the recorded one.

  -v, --verbose          list every artifact checked, not just the count
      --require-artifacts  finding zero artifacts is a failure, not a clean run
  -h, --help             print this and exit 0

Exit: 0 clean · 1 problems found · 2 could not safely run"""


# --------------------------------------------------------------------------------------
# The format contract
#
# Authoritative statement: `generator/README.md` in Impractical-Instruments/agent-tools,
# section "The hash — what a vendored verifier checks". Everything below is an
# implementation of that section. It is not the place the contract is decided.
# --------------------------------------------------------------------------------------

HTML_STYLE = ("<!--", "-->")
HASH_STYLE = ("#", None)

# A closed map, identical to the producer's. An extension outside it is not read at all.
COMMENT_STYLES = {
    ".md": HTML_STYLE,
    ".html": HTML_STYLE,
    ".py": HASH_STYLE,
    ".sh": HASH_STYLE,
    ".toml": HASH_STYLE,
    ".yml": HASH_STYLE,
    ".yaml": HASH_STYLE,
}

# Whole-file artifacts carry a `# Title` heading, so that contract is markdown-only.
WHOLE_FILE_EXTS = {".md"}

HASH_TOKEN_RE = re.compile(r"sha256=([0-9a-f]{64})")

# Matches a header whose `sha256=` token has been deleted, which the producer's own predicate
# does not. See the README, "What it reports as a problem".
GENERATED_HEADER_RE = re.compile(r"^<!--\s*GENERATED from .* — edit the source, not this file\.")

# The regeneration command is read out of the artifact's own marker, never declared here.
# See the README, "What it reports as a problem".
HEADER_COMMAND_RE = re.compile(r"\bby\s+`([^`]+)`")
MARKER_COMMAND_RE = re.compile(r"Regenerate with\s+`([^`]+)`")

# CommonMark: three or more backticks or tildes, indented less than four spaces.
FENCE_RE = re.compile(r"^ {0,3}(?P<fence>`{3,}|~{3,})\s*(?P<info>.*)$")


def span_hash(span: str) -> str:
    r"""sha256 of a covered span: `\r\n` to `\n`, UTF-8 encoded, lowercase hex.

    Nothing else is normalised. Without the line-ending step, a clone made with
    `core.autocrlf=true` reds every artifact in the repo.
    """
    return hashlib.sha256(span.replace("\r\n", "\n").encode("utf-8")).hexdigest()


def fence_mask(lines: "list[str]") -> "list[bool]":
    """True for every line inside, or delimiting, a fenced code block.

    A marker inside a fence is documentation, not a marker — a file that *documents* this
    contract is the forcing case. The producer skips exactly these lines.
    """
    mask = [False] * len(lines)
    closing = None
    for i, line in enumerate(lines):
        m = FENCE_RE.match(line.rstrip("\r"))
        if closing is None:
            # A backtick fence's info string may not itself contain a backtick (CommonMark).
            if m and not (m.group("fence")[0] == "`" and "`" in m.group("info")):
                closing = m.group("fence")
                mask[i] = True
            continue
        mask[i] = True
        if (m and m.group("fence")[0] == closing[0]
                and len(m.group("fence")) >= len(closing)
                and not m.group("info").strip()):
            closing = None
    return mask


def marker_res(style) -> tuple:
    open_ = re.escape(style[0])
    return (
        re.compile(r"^[ \t]*" + open_ + r"\s*ii:begin\s+(\S+)"),
        re.compile(r"^[ \t]*" + open_ + r"\s*ii:end\s+(\S+)"),
    )


def whole_file_span(lines: "list[str]", header_index: int) -> str:
    """Contract B: the whole file minus the header line and its terminating newline."""
    return "\n".join(lines[:header_index] + lines[header_index + 1:])


def region_span(lines: "list[str]", begin: int, end: int) -> str:
    """Contract A: the lines strictly between the markers, each carrying its terminator."""
    return "".join(line + "\n" for line in lines[begin + 1:end])


# --------------------------------------------------------------------------------------
# What a file claims about itself
# --------------------------------------------------------------------------------------


class Claim:
    """One generated span, its recorded token (or None) and the digest recomputed from it.

    `begin` and `end` are line indices: the header line for a whole file (with `end` None),
    the two marker lines for a region.
    """

    __slots__ = ("rel", "region", "recorded", "computed", "marker", "begin", "end")

    def __init__(self, rel: str, region: "str | None", recorded: "str | None",
                 computed: str, marker: str, begin: int, end: "int | None"):
        self.rel = rel
        self.region = region
        self.recorded = recorded
        self.computed = computed
        self.marker = marker
        self.begin = begin
        self.end = end

    @property
    def name(self) -> str:
        return self.rel if self.region is None else "{}:{}".format(self.rel, self.region)

    @property
    def what(self) -> str:
        return "whole file" if self.region is None else "region '{}'".format(self.region)

    @property
    def command(self) -> "str | None":
        pattern = HEADER_COMMAND_RE if self.region is None else MARKER_COMMAND_RE
        m = pattern.search(self.marker)
        return m.group(1) if m else None


class Dangling:
    """A marker with no partner, in either direction.

    `kind` is `"begin"` (no `ii:end` follows) or `"end"` (no `ii:begin` precedes). Both mean
    a delimiter line is gone and the region's extent can no longer be read, so both are
    reported. Only the `begin` marker carries a regeneration command; an `ii:end` never does.
    """

    __slots__ = ("rel", "region", "marker", "line", "kind")

    def __init__(self, rel: str, region: str, marker: str, line: int, kind: str):
        self.rel = rel
        self.region = region
        self.marker = marker
        self.line = line
        self.kind = kind

    @property
    def command(self) -> "str | None":
        m = MARKER_COMMAND_RE.search(self.marker)
        return m.group(1) if m else None


def scan(rel: str, text: str) -> tuple:
    """Return (claims, dangling) for one file, in file order."""
    ext = Path(rel).suffix
    lines = text.split("\n")
    fenced = fence_mask(lines)
    claims = []
    dangling = []

    if ext in WHOLE_FILE_EXTS:
        for i, line in enumerate(lines):
            if fenced[i] or not GENERATED_HEADER_RE.match(line):
                continue
            token = HASH_TOKEN_RE.search(line)
            claims.append(Claim(
                rel, None,
                token.group(1) if token else None,
                span_hash(whole_file_span(lines, i)),
                line, i, None,
            ))

    style = COMMENT_STYLES.get(ext)
    if style is not None:
        begin_re, end_re = marker_res(style)
        open_at = None
        for i, line in enumerate(lines):
            if fenced[i]:
                continue
            m = begin_re.match(line)
            if m:
                # A second `ii:begin` before the first closed leaves the first unclosed.
                if open_at is not None:
                    dangling.append(Dangling(rel, open_at[0], open_at[2], open_at[1], "begin"))
                open_at = (m.group(1), i, line)
                continue
            m = end_re.match(line)
            if m:
                if open_at is not None and m.group(1) == open_at[0]:
                    region, begin, marker = open_at
                    token = HASH_TOKEN_RE.search(marker)
                    claims.append(Claim(
                        rel, region,
                        token.group(1) if token else None,
                        span_hash(region_span(lines, begin, i)),
                        marker, begin, i,
                    ))
                    open_at = None
                else:
                    dangling.append(Dangling(rel, m.group(1), line, i, "end"))
        if open_at is not None:
            dangling.append(Dangling(rel, open_at[0], open_at[2], open_at[1], "begin"))

    return claims, dangling


# --------------------------------------------------------------------------------------
# Discovery
# --------------------------------------------------------------------------------------

# Build output and caches, skipped by the fallback walk only. On the git path a repo's own
# ignore rules do this job, and better. See the README, "How it reads a tree".
SKIP_DIRS = frozenset({
    ".git", ".hg", ".svn",
    "__pycache__", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox",
    "node_modules", ".venv", "venv",
    "target", "build", "dist", ".next", ".gradle",
})


def git_files(root: Path) -> "list[str] | None":
    """Paths under `root` that git accounts for, or None when git cannot answer.

    `--cached --others --exclude-standard` is tracked **plus** untracked-and-not-ignored. A
    bare `ls-files` omits untracked files, and a freshly vendored artifact is untracked on
    the very pull request that adds it — the run that most needs to check it.
    """
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "ls-files", "-z",
             "--cached", "--others", "--exclude-standard"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60,
        )
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    out = result.stdout.decode("utf-8", "replace")
    return sorted(part for part in out.split("\0") if part)


def walk_files(root: Path) -> "list[str]":
    """Every path under `root`, skipping build output and nested repositories."""
    out = []
    for dirpath, dirnames, filenames in os.walk(str(root)):
        keep = []
        for name in sorted(dirnames):
            # A directory carrying a `.git` entry is a separate repository with its own CI
            # and its own artifacts. `git ls-files` does not descend into one, so neither
            # does this, or the two discovery paths would disagree about the same tree.
            if name in SKIP_DIRS or os.path.exists(os.path.join(dirpath, name, ".git")):
                continue
            keep.append(name)
        dirnames[:] = keep
        for name in sorted(filenames):
            rel = os.path.relpath(os.path.join(dirpath, name), str(root))
            out.append(rel.replace(os.sep, "/"))
    return sorted(out)


def readable(root: Path, rels: "list[str]") -> "list[str]":
    """The subset of `rels` this program opens, in a deterministic order."""
    keep = []
    for rel in rels:
        if Path(rel).suffix not in COMMENT_STYLES:
            continue
        path = root / rel
        # A candidate path may be a gitlink, a deleted-but-tracked file, or a symlink into
        # the same tree — which would report one artifact twice, under two names.
        if path.is_file() and not path.is_symlink():
            keep.append(rel)
    return keep


def readable_files(root: Path) -> "list[str]":
    """Every path under `root` this program will read, in a deterministic order."""
    rels = git_files(root)
    if rels is None:
        rels = walk_files(root)
    return readable(root, rels)


# --------------------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------------------

NO_COMMAND = (
    "  This artifact's marker names no regeneration command, so this program cannot name "
    "one either. Regenerate it with whatever produced it, and report the marker as broken."
)


def fix_line(command: "str | None") -> str:
    if command is None:
        return NO_COMMAND
    return "  Regenerate it with: {}".format(command)


def mismatch_message(claim: Claim) -> str:
    return (
        "{}: {} does not match the sha256 it records — the generated span was edited by "
        "hand.\n"
        "  recorded: {}\n"
        "  computed: {}\n"
        "{}"
    ).format(claim.rel, claim.what, claim.recorded, claim.computed, fix_line(claim.command))


def missing_token_message(claim: Claim) -> str:
    return (
        "{}: {} carries no `sha256=` token, so nothing here can be checked. A generated "
        "artifact with no digest is not a clean one — reporting it as clean would throw the "
        "check away.\n"
        "{}"
    ).format(claim.rel, claim.what, fix_line(claim.command))


def dangling_message(item: Dangling) -> str:
    """One shape for both directions: a deleted `ii:begin` and a deleted `ii:end` are the
    same event seen from opposite ends, and neither leaves a region that can be read."""
    if item.kind == "begin":
        return (
            "{}: region '{}' has an opening `ii:begin` marker with no matching `ii:end {}`. "
            "Nothing can say where the region ended, so its digest cannot be recomputed.\n"
            "{}"
        ).format(item.rel, item.region, item.region, fix_line(item.command))
    return (
        "{}: `ii:end {}` on line {} has no matching `ii:begin {}` before it. Nothing can say "
        "where the region began, so its digest cannot be recomputed.\n"
        "  Restore the opening marker, or delete this line and regenerate the file."
    ).format(item.rel, item.region, item.line + 1, item.region)


# --------------------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------------------


def parse_argv(argv: "list[str]") -> dict:
    opts = {"root": ".", "verbose": False, "help": False, "require": False}
    positional = []
    for arg in argv:
        if arg in ("-h", "--help"):
            opts["help"] = True
        elif arg in ("-v", "--verbose"):
            opts["verbose"] = True
        elif arg == "--require-artifacts":
            opts["require"] = True
        elif arg.startswith("-") and arg != "-":
            raise ValueError("unknown option `{}`".format(arg))
        else:
            positional.append(arg)
    if len(positional) > 1:
        raise ValueError("expected at most one ROOT, got {}".format(len(positional)))
    if positional:
        opts["root"] = positional[0]
    return opts


def main(argv: "list[str]", stream=None) -> int:
    out = sys.stderr if stream is None else stream

    try:
        opts = parse_argv(argv)
    except ValueError as e:
        print("{}: {}".format(PROGRAM, e), file=out)
        print(USAGE, file=out)
        return 2

    if opts["help"]:
        print(USAGE, file=out)
        return 0

    root = Path(opts["root"])
    if not root.is_dir():
        print("{}: {} is not a directory".format(PROGRAM, root), file=out)
        return 2
    root = root.resolve()

    problems = []
    unreadable = []
    checked = 0
    verified = 0
    for rel in readable_files(root):
        try:
            # Bytes, then decode. `read_text` opens in universal-newline mode, which folds a
            # lone `\r` into `\n` — more normalisation than the contract permits.
            text = (root / rel).read_bytes().decode("utf-8")
        except OSError as e:
            # Collected, not raised. Aborting here would throw away every problem the sweep
            # had already found, and those findings are the reason anyone ran this.
            unreadable.append("{}: could not be read — {}".format(rel, e))
            continue
        except UnicodeDecodeError:
            continue
        claims, dangling = scan(rel, text)
        problems.extend(dangling_message(d) for d in dangling)
        for claim in claims:
            checked += 1
            if claim.recorded is None:
                problems.append(missing_token_message(claim))
            elif claim.recorded != claim.computed:
                problems.append(mismatch_message(claim))
            else:
                verified += 1
                if opts["verbose"]:
                    print("{}: ok  {}".format(PROGRAM, claim.name), file=out)

    for message in problems + unreadable:
        print(message, file=out)

    if unreadable:
        print(
            "{}: {} problem(s) and {} unreadable file(s); {} of {} artifact(s) verified. "
            "The sweep was incomplete, so a clean result would not have meant anything."
            .format(PROGRAM, len(problems), len(unreadable), verified, checked),
            file=out,
        )
        return 2

    if problems:
        print(
            "{}: {} problem(s); {} of {} artifact(s) verified".format(
                PROGRAM, len(problems), verified, checked
            ),
            file=out,
        )
        return 1

    if not checked:
        print(
            "{}: no generated artifact found under {}.\n"
            "  That is NOT proof of health. This program checks the artifacts it finds and "
            "asserts nothing about which ones ought to exist, so an empty result and a "
            "healthy repo look identical from here. Pass --require-artifacts to make this "
            "a failure.".format(PROGRAM, root),
            file=out,
        )
        return 1 if opts["require"] else 0

    print("{}: {} artifact(s) verified".format(PROGRAM, verified), file=out)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

# ii:begin provenance — derived from .ii/repo.toml; do not hand-edit out of sync. Regenerate with `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`. sha256=3ecbfd78b8fa9d384447d58dd57e76719c97d105f16165ee5be295970d1bfb4a
# Source:   Impractical-Instruments/agent-tools@4c5f337afe09ba5eab5567c67918ea66f97f3ad7:plugins/impractical-doctrine/verifier/ii_verify.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/verifier/ii_verify.py?ref=main' --jq '.content' | base64 -d > scripts/ii_verify.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
# ii:end provenance
