#!/usr/bin/env python3
"""Claim ledger for the doc surface — the layer that decides what a machine can check.

The corpus states things that can be *wrong*: a file path, a code identifier, a count, a claim that
something is single-sourced. This walks the governed docs, extracts each such statement as a typed
**claim**, and splits them by whether a machine can decide the claim at all:

  DECIDABLE — resolved here, and a failure fails the build:
    path          a backticked path (contains `/`) resolves to a real file, exactly or by suffix
    identifier    a backticked snake_case/CamelCase name appears somewhere in source
    guard         a `Guarded by: <path>::<test_fn>` line names a test function that exists
    roster-count  no doc states how many tools/verbs/contracts there are — see ROSTER_COUNT_RE

  UNDECIDABLE — extracted, ranked, and routed to a reviewer; never gates:
    count            "two spellings" — a machine cannot know what to count
    single-sourcing  "generated from one source" — the claim is about a mechanism, not a token

The split is the point. A gate that guesses at the undecidable half trains people to ignore it; a
reviewer handed all 300-odd claims reads none of them. Extracting cheaply and routing honestly is
what makes the expensive layer affordable — it reads a worklist, not a corpus.

**Two scoping rules, both load-bearing.**

*Which docs.* Only the governed surface: `docs/rules/`, `docs/agents/`, the root Markdown, and the
skills. `docs/research/`, `docs/rituals/` and `docs/adr/` are records of a moment — a research note
naming a file that has since moved is not wrong, it is history, and git is where history is checked.

*Which claims gate, per document kind.* A **rule** is present-tense and normative; a **rationale**
is an argument, and an argument names what it rejected — `In`/`Out` "not `InPort`/`OutPort`",
`pitch2freq` over the rejected `degree_to_freq`, the `upload_sample` tool that was turned down. Those
identifiers must NOT exist, so requiring them to resolve inverts the corpus's own meaning. Identifier
claims therefore gate on now-state docs and route to review inside `rationale/`. Path claims gate
everywhere: a rejected *name* is common, a rejected *file path* is not — and in the **entry docs**
(AGENTS.md and friends) they must resolve in full, because that surface is read as navigation and an
agent opens what it names.

Stdlib only. Green on an empty tree. Usage:

    python3 scripts/extract_doc_claims.py --check .            # gate the decidable claims
    python3 scripts/extract_doc_claims.py --review .           # print the reviewer's worklist
    python3 scripts/extract_doc_claims.py --json out.json .    # the whole ledger
    python3 scripts/extract_doc_claims.py --check --since dev  # only claims in changed files
"""
from __future__ import annotations
import argparse, json, re, subprocess, sys
from dataclasses import asdict, dataclass, field
from pathlib import Path

SKIP_DIRS = {".git", "target", "node_modules", "dist", "build", "__pycache__", "worktrees"}
# A claim-checker must not be its own evidence. This file and its tests name identifiers as
# EXAMPLES — in a comment, in a fixture — and indexing them would let any absent name resolve
# against the very code complaining about it. Found the hard way: a comment here quoting a
# defective doc silenced that doc's defect.
SELF_EXCLUDED = {"extract_doc_claims.py", "test_extract_doc_claims.py"}
# Where a claim's tokens are looked up. Config formats carry real identifiers (a workflow key, a
# manifest field), so a doc naming one is making a checkable claim like any other.
SOURCE_EXTS = {".rs", ".py", ".json", ".toml", ".yml", ".yaml", ".sh", ".js", ".mjs", ".ts"}
# Extensions a path claim may name. Wider than SOURCE_EXTS: docs cite docs.
PATH_EXTS = "rs|py|json|toml|md|yml|yaml|sh|tosc|lock"

# The governed surface, as (directory, recursive) or a literal file, relative to root.
GOVERNED_DIRS = ("docs/rules", "docs/agents", ".claude/skills")
GOVERNED_ROOT_FILES = ("CLAUDE.md", "AGENTS.md", "README.md", "CONTRIBUTING.md")

PATH_RE = re.compile(rf"`([A-Za-z0-9_./-]+\.(?:{PATH_EXTS})(?::\d+(?:-\d+)?)?)`")
# Identifiers are read out of code spans rather than matched whole, so a qualified path yields its
# segments: `Coordinator::swap` is a claim about `Coordinator`, and matching the span as one token
# would let every `Type::method` mention through unchecked.
CODE_SPAN_RE = re.compile(r"`([^`]+)`")
QUALIFIED_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*$")
# Deliberately narrow: snake_case needs a real underscore and CamelCase needs a second hump, so
# `f32`, `ok` and `Descriptor` are not read as claims. Under-claiming beats a noisy gate.
IDENT_RE = re.compile(r"^(?:[a-z][a-z0-9]*(?:_[a-z0-9]+)+|[A-Z][A-Za-z0-9]*(?:[A-Z][a-z0-9]+)+)$")
GUARD_RE = re.compile(r"^\s*Guarded by:\s*(\S+?)::(\w+)\s*$")
COUNT_RE = re.compile(
    r"\b(one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|"
    r"fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|\d{1,4})\s+"
    r"([a-z][a-z-]{3,}s)\b")
# The one count that is banned outright rather than routed. A roster count — "the same eight
# contracts", "across 27 tools", "the nineteen verbs" — goes stale the next time a verb ships, and
# the sentence around it has never needed the total to make its point: "the same contracts" and
# "across the advertised roster" say the same thing and cannot rot. Checking the number instead
# would mean teaching a stdlib script to count `CONTRACTS` through four doors; removing the drift
# surface is cheaper and stays correct.
#
# Three and up: a pair is architecture, not a roster — "the two verbs over typed handles", "the one
# contract validator" — and banning those would be a house style, not a defect check. Operator
# counts are deliberately out of scope: the ones in the corpus are past-tense evidence of a linking
# bug ("36 of 53 registered"), where the concrete number IS the argument being made.
ROSTER_COUNT_RE = re.compile(
    r"\b(three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|"
    r"sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|\d{1,4})\s+"
    r"(?:[a-z]+\s+)?(?:tools?|verbs?|contracts?)\b", re.I)
SINGLE_SOURCE_RE = re.compile(
    r"\b(single-sourced?|single source|one source|generated from|derived from|source of truth|"
    r"one declaration|exactly one place|cannot drift|never drift)\b", re.I)
FENCE_RE = re.compile(r"^\s*(?:```|~~~)")
ANCHOR_RE = re.compile(r'<a\s+id="([^"]+)"\s*>\s*</a>')

# Identifiers that resolve outside this repo's source, with the reason each is legitimately absent.
# An entry here is a claim that the name is NOT ours; it is not a place to park a stale reference.
KNOWN_EXTERNAL = {
    "AudioWorkletProcessor": "browser API",
    "AudioWorkletGlobalScope": "browser API",
    "Float32Array": "browser API",
    "TextDecoder": "browser API",
    "HashMap": "Rust std",
    "assert_no_alloc": "third-party crate",
    "data_len": "product-repo staging seam, not this repo",
    "upload_sample": "the rejected tool name, argued against by name",
    "enum_index": "named as the API that must not appear on the hot path",
}


@dataclass
class Claim:
    kind: str          # path | identifier | guard | count | single-sourcing
    file: str
    line: int
    text: str          # the claim exactly as written
    decidable: bool
    status: str        # ok | unresolved | needs-review
    note: str = ""

    def render(self) -> str:
        tail = f" — {self.note}" if self.note else ""
        return f"{self.file}:{self.line}: [{self.kind}] {self.text}{tail}"


@dataclass
class Index:
    """What the repo actually contains, built once."""
    root: Path = Path(".")
    paths: list[str] = field(default_factory=list)     # posix, relative to root
    tokens: set[str] = field(default_factory=set)      # every identifier in every source file

    def resolves_path(self, token: str, exact: bool = False) -> bool:
        """Exact match, or — unless `exact` — a unique-enough suffix. Docs legitimately write
        `format/normalize.rs` for a file that lives at
        `crates/reuben-core/src/format/normalize.rs`, and demanding the full path everywhere would
        be a house style this repo does not have.

        The entry docs are the exception (`exact=True`): AGENTS.md and README.md are a *navigation*
        surface, so an agent opens the path they name rather than reading it as a reference. A path
        that resolves only by suffix is one an agent cannot open."""
        bare = token.split(":")[0].removeprefix("./")
        return any(p == bare or (not exact and p.endswith("/" + bare)) for p in self.paths)

    def has_test_fn(self, path: str, fn: str) -> bool:
        """`def` as well as `fn`: the guards over the doc corpus itself are Python scripts with
        Python tests, so a rule about one can only name a `def`."""
        candidates = [p for p in self.paths if p == path or p.endswith("/" + path)]
        pattern = re.compile(rf"\b(?:fn|def)\s+{re.escape(fn)}\b")
        return any(pattern.search(Path(self.root, p).read_text(encoding="utf-8", errors="ignore"))
                   for p in candidates)


def build_index(root: Path) -> Index:
    idx = Index(root=root)
    for p in root.rglob("*"):
        if SKIP_DIRS & set(p.parts) or not p.is_file():
            continue
        idx.paths.append(p.relative_to(root).as_posix())
        if p.suffix in SOURCE_EXTS and p.name not in SELF_EXCLUDED:
            idx.tokens |= set(re.findall(r"[A-Za-z_][A-Za-z0-9_]*",
                                         p.read_text(encoding="utf-8", errors="ignore")))
    return idx


def governed_docs(root: Path) -> list[Path]:
    """Every Markdown file whose statements are current claims about this repo.

    Symlinks are dropped rather than followed: `CLAUDE.md` points at `AGENTS.md`, and counting one
    file twice would double every claim in it and report each defect two ways."""
    out: list[Path] = []
    for name in GOVERNED_ROOT_FILES:
        if (root / name).is_file() and not (root / name).is_symlink():
            out.append(root / name)
    for d in GOVERNED_DIRS:
        base = root / d
        if not base.is_dir():
            continue
        out.extend(p for p in base.rglob("*.md")
                   if not (SKIP_DIRS & set(p.parts)) and "_templates" not in p.parts
                   and not p.is_symlink())
    return sorted(set(out))


def rule_spans(text: str) -> list[tuple[int, str, str]]:
    """`(1-based anchor line, rule slug, the rule's text)` for each rule in a topic doc.

    A rule runs from its `<a id>` anchor to the next anchor or the next H2, whichever comes first —
    the same span the link guard walks. Claims about rules are keyed by the RULE, not by the line:
    a slug that happens to contain "single-source" appears on its anchor line and again in its
    `[why]` link, and reporting those as three findings buries the one that is real."""
    lines = text.split("\n")
    anchors = [(i, m.group(1)) for i, ln in enumerate(lines)
               for m in (ANCHOR_RE.search(ln),) if m]
    h2 = [i for i, ln in enumerate(lines) if ln.lstrip().startswith("## ")]
    out = []
    for idx, (a_line, slug) in enumerate(anchors):
        next_anchor = anchors[idx + 1][0] if idx + 1 < len(anchors) else len(lines)
        next_h2 = next((h for h in h2 if h > a_line), len(lines))
        out.append((a_line + 1, slug, "\n".join(lines[a_line:min(next_anchor, next_h2)])))
    return out


def prose_lines(text: str) -> list[tuple[int, str]]:
    """`(1-based lineno, line)` for lines outside fenced blocks. A shell transcript or a layout
    diagram is an illustration, not a claim about what exists right now."""
    out, in_fence = [], False
    for i, line in enumerate(text.split("\n"), 1):
        if FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if not in_fence:
            out.append((i, line))
    return out


def identifiers_in(line: str) -> list[str]:
    """Every identifier claimed by a code span on this line, in order, deduplicated."""
    out: list[str] = []
    for span in CODE_SPAN_RE.findall(line):
        if not QUALIFIED_RE.match(span):
            continue
        for segment in span.split("::"):
            if IDENT_RE.match(segment) and segment not in out:
                out.append(segment)
    return out


def extract(path: Path, root: Path, idx: Index) -> list[Claim]:
    rel = path.relative_to(root).as_posix()
    # A rationale argues; everything else states the now. See the module docstring.
    is_argument = "rationale" in path.parts
    is_topic = path.parent == root / "docs" / "rules" and path.name != "README.md"
    is_entry_doc = path.parent == root and path.name in GOVERNED_ROOT_FILES
    claims: list[Claim] = []

    text = path.read_text(encoding="utf-8", errors="ignore")
    for lineno, line in prose_lines(text):
        for token in PATH_RE.findall(line):
            if "/" not in token.split(":")[0].removeprefix("./"):
                # A bare filename that matches a real file is a fair reference. One that matches
                # nothing is as often an illustration (`a.json`, `pad.json`) as a stale claim, and
                # telling those apart is judgment — so it routes rather than fails.
                claims.append(Claim("path", rel, lineno, token, False,
                                    "ok" if idx.resolves_path(token) else "needs-review",
                                    "" if idx.resolves_path(token)
                                    else "bare filename matching no file — claim or example?"))
            elif idx.resolves_path(token, exact=is_entry_doc):
                claims.append(Claim("path", rel, lineno, token, True, "ok"))
            else:
                claims.append(Claim("path", rel, lineno, token, True, "unresolved",
                                    "not an openable path — an entry doc names paths in full"
                                    if is_entry_doc else "no such file, by full path or suffix"))

        for token in identifiers_in(line):
            if token in idx.tokens:
                claims.append(Claim("identifier", rel, lineno, token, True, "ok"))
            elif token in KNOWN_EXTERNAL:
                claims.append(Claim("identifier", rel, lineno, token, True, "ok",
                                    KNOWN_EXTERNAL[token]))
            elif is_argument:
                claims.append(Claim("identifier", rel, lineno, token, False, "needs-review",
                                    "absent from source, in a doc that argues about names"))
            else:
                claims.append(Claim("identifier", rel, lineno, token, True, "unresolved",
                                    "names code that does not exist"))

        m = GUARD_RE.match(line)
        if m:
            target, fn = m.group(1), m.group(2)
            ok = idx.has_test_fn(target, fn)
            claims.append(Claim("guard", rel, lineno, f"{target}::{fn}", True,
                                "ok" if ok else "unresolved",
                                "" if ok else "names no such test function"))

        bare = line.replace("`", "")
        for m in ROSTER_COUNT_RE.finditer(bare):
            n = m.group(1)
            if n.isdigit() and int(n) < 3:
                continue
            claims.append(Claim("roster-count", rel, lineno, m.group(0), True, "unresolved",
                                "a roster count drifts the next verb that ships — "
                                "say what the sentence needs, not how many"))

        # A count is only worth a reviewer's time when the line also names something nameable —
        # "the same eight `resources`" is checkable, "two devices" is architecture and never will be.
        if re.search(r"`[^`]+`", line):
            # Backticks come off first: docs write "the same eight `resources`", and leaving the
            # markup in place breaks the number-then-noun match on exactly the countable cases.
            for m in COUNT_RE.finditer(bare):
                claims.append(Claim("count", rel, lineno, m.group(0), False, "needs-review",
                                    "counts something the line names"))

    # Only a RULE makes a normative single-sourcing claim, and that set is what #634 asks to audit
    # for a nameable guard. A rationale re-argues the claim and a skill repeats it; routing those
    # buries the handful that matter under eighty that do not.
    if is_topic:
        for lineno, slug, span in rule_spans(text):
            if SINGLE_SOURCE_RE.search(span):
                claims.append(Claim("single-sourcing", rel, lineno, f"#{slug}", False,
                                    "needs-review", "claims one source — can it name a guard?"))

    return claims


def changed_files(root: Path, since: str) -> set[str] | None:
    try:
        out = subprocess.run(["git", "-C", str(root), "diff", "--name-only", f"{since}...HEAD"],
                             capture_output=True, text=True, check=True).stdout
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    return {ln.strip() for ln in out.splitlines() if ln.strip()}


def collect(root_arg: str = ".", since: str | None = None) -> list[Claim]:
    root = Path(root_arg).resolve()
    docs = governed_docs(root)
    if since:
        touched = changed_files(root, since)
        if touched is not None:
            docs = [p for p in docs if p.relative_to(root).as_posix() in touched]
    if not docs:
        return []
    idx = build_index(root)
    return [c for p in docs for c in extract(p, root, idx)]


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("root", nargs="?", default=".")
    ap.add_argument("--check", action="store_true", help="exit non-zero on an unresolved claim")
    ap.add_argument("--review", action="store_true", help="print the needs-review worklist")
    ap.add_argument("--json", metavar="PATH", help="write the whole ledger as JSON")
    ap.add_argument("--since", metavar="REF", help="only docs changed since REF")
    args = ap.parse_args(argv)

    claims = collect(args.root, args.since)
    unresolved = [c for c in claims if c.status == "unresolved"]
    review = [c for c in claims if c.status == "needs-review"]

    if args.json:
        Path(args.json).write_text(json.dumps([asdict(c) for c in claims], indent=2) + "\n",
                                   encoding="utf-8")

    if args.review:
        for c in review:
            print(c.render())

    for c in unresolved:
        print(c.render(), file=sys.stderr)

    kinds = sorted({c.kind for c in claims})
    tally = ", ".join(f"{k} {sum(1 for c in claims if c.kind == k)}" for k in kinds)
    print(f"extract_doc_claims: {len(claims)} claim(s) [{tally}]; "
          f"{len(unresolved)} unresolved, {len(review)} to review", file=sys.stderr)
    return 1 if (args.check and unresolved) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
