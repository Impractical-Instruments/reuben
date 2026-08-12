#!/usr/bin/env python3
"""Reference-linter for the rules-doc system. Runs against any repo carrying one.

The repo is the ROOT argument. Unlike its two siblings this guard also reads the repo's CODE,
so check 7 below makes it the one validator with a per-repo surface: the lane census is a set
of constants in this file, and a repo whose tree carries a file type the census has never met
is REPORTED until somebody classifies it. That is the check working, but the classification
has nowhere repo-local to live today — see the payload README, "The census is not yet
per-repo".

Seven checks:
  1. No `ADR-<n>` references survive in CODE. The only legitimate ADR mentions are
     `Distilled from:` lines in docs/rules/rationale/** and the live ADRs in docs/adr/**
     (both are Markdown, which this linter does not scan as code), plus the `absorb-adrs`
     skill itself — the tool that distils ADRs into rules and stamps each rationale's
     provenance line, so its scaffolder + tests name ADRs by design (see SKILL_ALLOWLIST).
  2. Every `see rules: <topic>` / `see engine rules: <topic>` code comment names a kebab-case
     slug; for the same-repo form, docs/rules/<topic>.md must exist. For the cross-repo form,
     the topic is resolved against the pinned upstream submodule's <path>/docs/rules/<topic>.md
     at the SHA this repo is built against — inert in a repo that pins none, which is most of them,
     and live in one that does from the moment it bumps the pin.
  3. A `//!` module doc longer than MAX_UNPOINTED_MODULE_DOC lines names a topic. Length is a
     proxy for carrying rationale: a doc that long is arguing something, and an argument in code
     has to point at the rule it belongs to.
  4. No COMMENT cites an issue (`#<nn>`, `reuben#<nn>`) or a rule anchor (`agent-mcp.md#some-rule`).
     An issue number is provenance, and provenance lives in a rationale file's `Decided in:` /
     `Distilled from:` line — the same reason check 1 bans `ADR-<n>`. In code it is an unresolvable
     pointer to a closed argument, and it reliably marks a comment that is retelling history rather
     than stating mechanics. A rule anchor is the deeper half of the same mistake: code points at
     topics only. (The examples above are written with a placeholder because this file is scanned by
     its own check, the same reason check 1 spells `ADR-<n>` that way.)

     The rule is about prose, not about a language, so this runs over every lane in CODE_EXTS. What
     it does NOT reach is decided by ONE test, applied per lane: **does the language generate this
     comment into something a model or a user reads?** Where it does, the text is not only a comment,
     a second rule governs it, and a linter reading source cannot tell the two apart — so this check
     stands off. Where nothing generates prose out of comments, every comment is only a comment and
     all of it is in reach. Nothing here turns on the comment's SHAPE.

     THE SHAPE OF THE LANE LIST, decided rather than left implicit: first-party source is in reach BY
     DEFAULT, and the only per-lane fact is the comment OPENER. CODE_EXTS is an opener table, not a
     permission list — a suffix missing from it is a lane the guard cannot yet READ, not a lane that
     is exempt. The alternative shape, an allow-list of languages the guard understands, was rejected
     on its FAILURE MODE: it fails silently. A lane nobody added is a lane nobody is told about, and
     the stylesheet corpus is the proof — the consuming repo had 21 issue citations sitting in CSS
     comments while this guard reported its tree clean, because `.css` had never been a row.

     That is a claim about behaviour, so CHECK 7 below is what makes it one: reach being default-on
     means the EXCLUSIONS are what get enumerated, and every lane key in the tree has to land in
     exactly one of three buckets — readable, deliberately out, or not a prose surface. A lane in
     none of them is REPORTED. Without that check the sentence above is decoration: one set would
     decide both "can we read this" and "do we scan this", the two questions would collapse, and a
     lane nobody has added would be indistinguishable from a lane deliberately let go — which is the
     allow-list, wearing different words. Check 7 found `.env` and `.html` on its first run.

     Two things the default does not license. It is not permission to GUESS at a lane's comment
     syntax: guessing wrong turns code into prose (the reason HASH_EXTS is keyed off the suffix
     rather than sniffed), so a lane still earns its row before it is scanned — the row is what
     reading costs, not what reach costs. And it is not a claim that nothing is ever out. Markdown is
     out because provenance's one sanctioned home IS a Markdown file — a rationale's `Distilled from:`
     / `Decided in:` line, and the live ADRs — so scanning it would red on the very text the rule
     points at. A lane is out when another rule already governs the same text, or when nothing can
     read it yet and that is written down; never merely because no one got to it.

     RUST — `///`, `/** … */` and `/*! … */` are out of reach. A doc comment on a `JsonSchema` type
     is generated into the advertised `description` a model reads over the wire; this linter cannot
     tell which ones are, and an advertised description is governed by its own rule — see rules:
     code-as-grounding. That half is guarded from the door's own OUTPUT instead, in reuben-mcp's
     stdio integration test, which is the only place the distinction is decidable. `//` and `//!`
     are in reach: nothing generates those.

     JS/TS — everything is in reach, `//`, `///`, `/* … */` and `/** … */` alike. The premise the
     Rust carve-out needs is simply absent here: this repo has no `tsconfig.json`, no
     `jsconfig.json`, no `checkJs`, and no typedoc or jsdoc dependency, and the tool schemas the
     model reads are imported from a GENERATED JSON artifact built on the Rust side — so not one
     character of a JS comment reaches a model or a user. There is no second regime to be unable to
     distinguish. The corpus says the same thing from the other direction: JS block comments here
     carry argument, not description ("**THE SEAM HAS BEEN TAKEN** … This used to read: …"), which
     is exactly what this check exists to keep out of code.

     The `#` lanes — shell, YAML, TOML, Python — are in reach whole, for the same reason: nothing in
     a workflow file, a manifest or a shell script is ever generated into consumed prose. Python
     DOCSTRINGS are the one ragged edge, and they are ragged by accident rather than by decision: a
     docstring line is reached only when it happens to carry no quote character, so the guard sees
     some of a docstring and not the rest. Bringing them in deliberately is a separate change, and
     the mirrored-guard pointer question has to be settled before it, not after.

     CSS — in reach whole, and "whole" is the entire lane: `/* … */` is the only comment form CSS
     has, there is no doc-comment analogue to carve out, and nothing generates a stylesheet comment
     into anything a model or a user reads. The one lane fact it does NOT share with the JS family is
     that `//` opens nothing here — it is the authority slashes of a URL inside `url(…)`, and
     anywhere else a syntax error — so reading CSS with the JS scanner would take the tail of a URL
     for prose and red on its fragment. That is why it is a lane of its own (BLOCK_LANES) rather than
     one more suffix on the curly one. A backtick is likewise not a delimiter here, so CSS tracks
     `"` and `'` only.

     One collision CSS brings that no other lane does, left STRICT on purpose: an all-digit hex
     colour of three or four digits is spelled exactly like an issue citation, so a bare one written
     inside a comment reds. It cannot be narrowed away — those two are the same token, and no context
     around them decides it — and the two shapes it lands on are ordinary, not exotic: the grey ramp,
     and a commented-out declaration. So the guard stays strict AND says so where the author is
     standing: in the CSS lane the message carries COLOUR_HINT, naming the collision and the way out.
     Neither this docstring nor the rules doc is a surface anybody reads at the moment they are stuck.

     The hint is not selective and should not be read as if it were. SHORT_HEX_RE admits any three-
     or four-digit run, and these repos' issue numbers live squarely inside that range, so in
     practice EVERY citation the CSS lane reports carries the caveat — a real citation gets a
     paragraph about colours it does not need. That is the trade taken deliberately: the alternative
     is a hint that fires only on shapes a heuristic guessed were colours, which is the same guess
     the strictness exists to refuse, one level up. The message is worded conditionally ("if that is
     a hex colour") because it genuinely cannot tell, not as hedging.

     The DOT-PREFIXED lanes — `.env`, `.gitignore`, `.gitattributes`, `.gitmodules`, `.ignore` — and
     an extensionless script's SHEBANG are all in reach, all `#`. Each was invisible for the same
     reason: the guard read a suffix, and these files put the format somewhere else. `.env.production`
     has suffix `.production`, which describes nothing; `.gitignore` has no suffix at all; a githook
     has neither, and says `#!/bin/sh` on line one. `#` had opened a comment in every one of them the
     whole time. See `lane_key`, which exists for exactly this and is now one rule rather than a
     special case per file.

     `.html` — in reach, and the first lane here whose comment syntax changes BY REGION inside one
     file: `<!-- … -->` in the markup, `/* … */` inside `<style>`, `//` and `/* … */` inside
     `<script>`. The opener table did NOT grow a notion of nesting. The lane is a REGION FINDER
     instead (`html_comments`): it reads the `<!-- … -->` form itself, and hands each raw-text
     element to the lane that already reads that language — `<style>` to css, `<script>` to curly.
     Everything a comment means inside those regions is then decided by a scanner that is already
     tested, including CSS's colour collision, which travels with the REGION rather than with the
     file: a bare `#nnn` inside `<style>` gets COLOUR_HINT and the same token in an HTML comment does
     not. What is genuinely new is only where a region starts and stops, which is the only part that
     can be wrong, so that is the part written down below.

     WHERE A REGION STOPS is decided string-aware, which is a deliberate departure from the HTML
     tokenizer. The spec ends a raw-text element at the first `</script`, full stop — which is why a
     page has to spell it `<\\/script>` inside a JS string. `raw_text_end` instead skips a close tag
     that sits inside a STRING or a BLOCK comment of the region's own language. What licenses that
     is the narrowness: a close tag in either of those can only occur in a page a browser already
     reads as broken, so the two readings differ on nothing that works — and of the two ways to be
     wrong on a broken page, ending the region EARLY is the worse one, because the tail of the
     script then gets read as markup, where a `//` comment is invisible and a stray `-->` closes a
     comment that never opened. Early-ending is also why the close TAG NAME has to end
     (`</scriptfoo>` closes nothing, per HTML5) and why the open tag is matched quote-aware: a `>`
     inside an attribute value is not the end of the tag, and taking it for one leaves a dangling
     quote that opens a string, eats the close tag, and swallows the rest of the file.

     A `//` LINE comment is NOT tracked, which is the same argument reaching the opposite answer and
     is the one place this lane sides with the tokenizer. `doIt(); // go</script>` is not a broken
     page but an ordinary one — the tag ends the comment and the element together, exactly as
     written — so hiding a close tag behind `//` would lose the region on markup that WORKS, and
     lose it in the swallowing direction. The rule the three cases share: depart from the tokenizer
     only where the tokenizer's answer implies a page nobody can render anyway.

     ONE DIVERGENCE RISK TO HAND ON, because the same regions are wanted by a second tool. The lint
     story for inline `<script>` is unbuilt here (the ESLint plugin that extracted those blocks does
     not work under the pinned major), and every HTML-aware ESLint processor follows the TOKENIZER —
     it truncates at the first `</script`, which is precisely the reading rejected above. So if a
     processor lands, the two tools will disagree about where a script region ends on exactly the
     inputs that are already broken, and nothing would notice: one would lint half a file while the
     other read all of it. Whoever builds it should either adopt the boundary written down here or
     record that it is taking the tokenizer's. The shared thing is this decision, not the code —
     that side needs source text plus an offset map, in another language, in another repo.

     WHAT THE REGION FINDER DOES NOT DO, since it is a scanner and not a parser. It does not know
     element nesting or `@type`, so a `<script type="text/x-template">` holding markup is read as JS
     rather than as markup. It does not know JS regex literals, so a `/` opening one is not a string
     and a `</script` inside one would end the region. An open tag whose quote never closes falls
     back to ending at the first `>`, which is a guess, though the browser has none better. What it
     DOES track, each because the alternative is a wrong answer rather than a missing one: a
     `<script>` or `<style>` written inside an HTML comment opens no region; `<textarea>` and
     `<title>` hold ESCAPABLE raw text, so a `<!--` in one is page content and is skipped rather than
     reported. None of the unhandled shapes exists in either tree, and each is a written gap rather
     than a silent one, which is the same standard check 7 holds a lane to.

     A note on scope, since a lane table invites the question: `.go`, `.c`, `.java` and friends sit
     in CODE_EXTS and are read as JS is. Neither repo contains one. If a Javadoc'd Java file ever
     lands, it fails the test above — javadoc IS a generator — and earns its own row.
  5. A `see rules: <topic>` pointer stops at the topic. Check 4's `RULE_ANCHOR_RE` only sees the
     `<file>.md#<rule>` spelling; prose reaches a rule three other ways — `<topic>#<rule>`,
     a parenthesised `(<rule>, <rule>)` trailing the topic, and a comma-continued list — and
     a Rust `///` is out of check 4's reach entirely. Check 5 reads the pointer's own tail instead, so
     every spelling resolves or is reported. It also rejects a capitalised `see`, which the
     grammar does not admit and which therefore slips past checks 2 and 3 unvalidated.

     Both this and check 2 run over the comment-folded view, so a pointer rustfmt wrapped across
     a line break is still read whole.
     see rules: code-as-grounding
  6. A parity marker records a reason. A test whose job is that two lists match is evidence that
     generation was not attempted, so it carries a marker naming why one list cannot be generated
     from the other; this check holds every marker that exists to a substantive reason
     (MIN_PARITY_REASON_WORDS) and rejects a mis-cased spelling rather than skipping it, the same
     way check 5 handles a capitalised pointer.

     What it deliberately does NOT do is find parity tests that carry no marker. That is the
     undecidable half: `assert_eq!(a.len(), b.len())` is a round-trip everywhere it appears in
     this workspace, and the parity test that motivated the rule matches no name or shape pattern
     at all — a detector with that ratio is one people learn to filter out. The reason is demanded
     at writing time instead, while the author still knows whether generation was tried.
  7. Every lane key present in the tree is classified. Reach is default-on (check 4), which is a
     claim about BEHAVIOUR and so has to be made one: a lane key is READABLE (an opener row in
     CODE_EXTS), deliberately OUT OF REACH (OUT_OF_REACH_EXTS, whose entry IS the written reason), or
     NOT A PROSE SURFACE at all (NOT_SOURCE_EXTS — data, fixtures, binaries, lockfiles). A lane key
     in none of the three is reported.

     What this buys is the failure mode. Under an allow-list, a lane nobody added and a lane
     deliberately let go look identical from outside — both are silence — and that is precisely how
     `.css` accumulated 21 citations unnoticed. Here the enumeration burden sits on the EXCLUSIONS,
     so the guard's answer to a lane it has never met is a sentence rather than a shrug. The cost is
     that a genuinely new file type reds until somebody classifies it: a one-line edit, and the whole
     point. Run against these two trees it immediately named two lanes nobody had noticed — `.env`,
     which turned out to be readable and carrying a citation, and `.html`, which was classified out
     until it earned the region finder it now has. Both ended up READ, which is the argument: the
     bucket a lane lands in first is a statement about the guard, and it is meant to be revisited.

     BOTH exclusion buckets carry reasons, and for the same reason: if only one demanded a sentence,
     the other would be the cheap door out of a check-7 red, and the allow-list would be back wearing
     a third set of words. NOT_SOURCE_EXTS shares three reasons across its entries rather than
     inventing sixteen — the argument is per KIND (no comment syntax, binary asset, generated
     output), and writing it out once per image format would be the ritual, not the argument.

     It reads a lane KEY, not a suffix (`lane_key`), because a file declares its format in more than
     one place and reading only the suffix is how this check came to assert something untrue. A
     dot-prefixed name IS the format; an extensionless executable declares it on line one. Four
     `.gitignore`s and a githook were carrying citations in this exact ban's blind spot, in both
     repos, while check 7 reported every lane classified — it had no key for them to be unclassified
     UNDER. A file that declares no format anywhere keys as "" and is classified like anything else,
     with its reason written down, rather than dropping out of the census.

     WHAT IT WALKS is the repository, not the filesystem: `repo_files` asks git for tracked plus
     untracked-not-ignored, and only falls back to `rglob` outside a working tree. `rglob` made the
     guard un-runnable locally the moment anybody ran the tests — a Playwright run leaves
     `test-results/**/video.webm`, and check 7 would report an unclassified `.webm` and advise giving
     a video file a comment opener. CI never saw it, because a clean checkout has no junk in it,
     which is the shape of bug that survives longest.

Exit non-zero on any violation. Stdlib only.

Usage: python3 check_rules_refs.py [root=.]
"""
from __future__ import annotations
import bisect, re, subprocess, sys
from pathlib import Path, PurePath

# The opener table, NOT a permission list: first-party source is in reach by default and a suffix
# earns its row here so the guard knows how to READ it. See the module docstring's checks 4 and 7.
CODE_EXTS = {".rs", ".py", ".mjs", ".js", ".ts", ".jsx", ".tsx", ".go", ".c", ".h",
             ".cpp", ".hpp", ".java", ".rb", ".sh", ".toml", ".yml", ".yaml", ".css", ".html",
             ".env", ".gitignore", ".gitattributes", ".gitmodules", ".ignore", "#!"}

# The other two buckets check 7 sorts a lane key into. Under default-on it is the EXCLUSIONS that
# have to be enumerated, so these two are the ones that grow, and a key in none of the three is
# reported rather than skipped.
#
# BOTH carry reasons, and they carry them for the same reason. An exclusion is a decision, and a
# decision with no argument is the silence this check exists to abolish — so if only one bucket
# demanded a sentence, the other would be the cheap door an author takes to make a check-7 red go
# away, and the allow-list would be back wearing a third set of words. The reason IS the entry.
#
# Deliberately out of reach: first-party source the guard does not read, and why not.
OUT_OF_REACH_EXTS = {
    ".md": "provenance's one sanctioned home is a Markdown file — a rationale's `Distilled from:` / "
           "`Decided in:` line, and the live ADRs — so scanning it would red on the very text the "
           "rule points at",
}
# Not a prose surface at all. Three reasons cover every entry, so they are named once and shared:
# writing sixteen bespoke paragraphs about image formats would be the ritual, not the argument.
NO_COMMENT_SYNTAX = ("the format has no comment syntax at all, so there is no comment here for the "
                     "ban to reach — every byte of it is content")
BINARY_ASSET = ("a binary asset: bytes rather than text, and nothing anybody writes prose into or "
                "reads prose out of")
GENERATED_OUTPUT = ("generated by the tool that owns it and edited by regenerating, never by hand, "
                    "so a comment in it does not survive to be read")
# A file whose name declares no format and whose first line declares no interpreter. It states no
# comment syntax, so it has no comments — the ban has nothing to reach. LICENSE and NOTICE are the
# whole population in these two trees, and both are verbatim third-party text besides.
NO_LANE_KEY = ("the name declares no format and the first line declares no interpreter, so the file "
               "states no comment syntax and therefore has no comments to police")
NOT_SOURCE_EXTS = {
    "": NO_LANE_KEY,
    ".json": NO_COMMENT_SYNTAX, ".txt": NO_COMMENT_SYNTAX, ".tiktoken": NO_COMMENT_SYNTAX,
    # `.mcp.json` — the one file where the dot-prefixed leading segment is a NAME and the format
    # is the suffix behind it, so `lane_key` keys it as `.mcp`. Classified rather than special-cased
    # in `lane_key`: teaching that function to prefer a trailing suffix would have to except
    # `.env.production`, whose `.production` names an environment and no syntax at all. The verdict
    # is the same either way — JSON.
    ".mcp": NO_COMMENT_SYNTAX,
    ".lock": GENERATED_OUTPUT, ".map": GENERATED_OUTPUT,
    ".tosc": BINARY_ASSET, ".wav": BINARY_ASSET, ".woff2": BINARY_ASSET, ".png": BINARY_ASSET,
    ".jpg": BINARY_ASSET, ".jpeg": BINARY_ASSET, ".gif": BINARY_ASSET, ".ico": BINARY_ASSET,
    ".webp": BINARY_ASSET, ".wasm": BINARY_ASSET, ".pyc": BINARY_ASSET,
}
SKIP_DIRS = {".git", "target", "node_modules", "dist", "build"}
# Two skips that are only ever a skip AT THE ROOT, matched as a root-anchored tuple rather than as
# a bare directory name anywhere in the path.
#
# A nested checkout is a second copy of the repo, not repo content: the agent worktree tool checks
# a full tree out under `.claude/worktrees/<id>/`, so a plain walk reads every file twice and
# reports the stale copy's violations at paths that look real. Anchored rather than skipping all of
# `.claude`, because the rest of that directory is tracked source — SKILL_ALLOWLIST below exists
# precisely because a skill under it is scanned today.
#
# `engine/` is the submodule the consuming repo scans FROM, so a run at that root must not read the
# submodule's tree as its own. It used to sit in SKIP_DIRS, which matches any component: that also
# skipped `crates/reuben-api/src/engine/`, eight first-party modules that went unread under this
# repo's own CI. A directory is not exempt for being NAMED `engine` — only for BEING the submodule,
# which only the root position says.
SKIP_PREFIXES = {(".claude", "worktrees"), ("engine",)}
# The absorb-adrs skill is the one sanctioned home of ADR-NNNN tokens in code: it distils ADRs
# into rules and writes each rationale's `Distilled from:` line, so its scaffolder + tests name
# ADRs by design. Keyed on a PATH COMPONENT, so it follows the skill wherever it is installed and
# is simply inert in a tree that has none — which is every tree that reaches the skill from a
# plugin rather than committing it.
SKILL_ALLOWLIST = {"absorb-adrs"}

ADR_RE  = re.compile(r"\bADR-\d+\b")
SEE_RE  = re.compile(r"\bsee (engine )?rules: ([A-Za-z0-9-]+)")
SLUG_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
# The same pointer with the `see` unanchored to case. Only the lowercase spelling is grammar
# (docs/rules/README.md, Conventions); this exists so the other spelling is *reported* rather than
# skipped by SEE_RE, since an unmatched pointer is an unvalidated topic, not a clean file.
SEE_ANYCASE_RE = re.compile(r"\bsee (engine )?rules:[ \t]*", re.IGNORECASE)

# The unpointed `//!` budget. Ten lines is room for what a module doc legitimately owes a reader —
# what this file is, and the local facts the code cannot state — before the length itself says
# rationale accumulated.
MAX_UNPOINTED_MODULE_DOC = 10

# An issue citation in a comment: a bare hash-and-number, or the cross-repo `reuben#<nn>`. Two
# digits minimum, so `#3` in prose and a commented-out `#[derive(…)]` cannot trip it. The examples
# are placeholders because this file is scanned by its own check.
ISSUE_RE = re.compile(r"(?<![\w#])(?:[a-z][\w.-]*)?#\d{2,4}\b")
# A rule-level pointer: `<topic>.md#<rule>`, spelled with placeholders because this file is scanned
# by its own check. Code points at TOPICS only
# (docs/rules/README.md, Conventions) — a rule slug is the deepest rung and reworded freely, so a
# comment naming one is a link that breaks silently.
RULE_ANCHOR_RE = re.compile(r"\b[a-z0-9-]+\.md#[a-z0-9-]+")

# The CSS lane's one ambiguity, and the only thing the guard can do about it. An all-digit three- or
# four-digit hex colour IS an issue citation, character for character, and the collision lands
# squarely on the grey ramp — the most hand-written shorthand there is — and on a commented-out
# declaration, the most ordinary comment a stylesheet carries. The guard stays strict, because every
# narrowing that would let a grey through would also let a real citation through; what it owes the
# author instead is a message that names the collision, since the docstring and the rules doc are
# not the surface anyone hits at 2am. Only the digits-only shapes can be a colour: the cross-repo
# `<repo>#<nnn>` spelling cannot, and gets the plain message. (Spelled with placeholders throughout,
# because this file is scanned by its own check.)
SHORT_HEX_RE = re.compile(r"^#\d{3,4}$")
COLOUR_HINT = ("  (If that is a hex COLOUR, not a citation: it is the same token as an issue number "
               "and nothing can tell them apart, so this lane reads it strictly. Write it six-digit "
               "or as `hsl(…)`, or say what the colour is FOR — a colour restated in a comment is "
               "restating the declaration under it. A colour in a DECLARATION is never reached.)")

# Check 5's three ways a pointer reaches past its topic, matched against the text right after it:
# `<topic>#<rule>`, a parenthesised list, and a comma-continued list. Each captured token is
# resolved as a topic; anything that is not one is a rule slug, the deepest rung, which code does
# not name. Nothing further out counts — ordinary prose follows a pointer all the time.
POINTER_ANCHOR_RE = re.compile(r"^#([a-z0-9-]+)")
POINTER_PARENS_RE = re.compile(r"^[\s.:—-]*\(\s*([a-z0-9-]+(?:\s*,\s*[a-z0-9-]+)*)\s*[,)]")
POINTER_COMMAS_RE = re.compile(r"^((?:\s*,\s*[a-z0-9-]+)+)")
# Check 6's marker and the bar it has to clear. The reason has to be a sentence about *this* pair
# of lists — "the door builds it at runtime, so the test is the only place both exist" — and a word
# count is the cheapest thing separating one from a rubber stamp (`yes`, `n/a`, `see above`). Five
# words is the shortest real answer anyone has needed to give. The marker is spelled in the regex
# rather than written out here: this file is scanned by its own check.
PARITY_RE = re.compile(r"\bParity:[ \t]*([^\n]*)")
PARITY_ANYCASE_RE = re.compile(r"\bparity:[ \t]*", re.IGNORECASE)
MIN_PARITY_REASON_WORDS = 5

# Which token opens a comment, by extension. Keyed off the suffix rather than sniffed generically:
# `#` opens a comment in the shell/Python half and is an attribute (`#[…]`) or a raw-string delimiter
# (`r#"…"#`) in Rust, and guessing wrong turns code into prose.
HASH_EXTS = {".py", ".sh", ".rb", ".toml", ".yml", ".yaml", ".env", ".gitignore", ".gitattributes",
             ".gitmodules", ".ignore", "#!"}
# Which interpreters make a shebang a `#` lane. Not a guess: an interpreter absent from this set is
# reported by check 7 under its own name rather than read with an opener nobody checked — `#` opens
# a comment in a shell script and is a private field in a JS one.
HASH_INTERPRETERS = {"sh", "bash", "zsh", "dash", "ksh", "ash", "python", "python3", "ruby", "perl"}
# The five lanes, by suffix. `rust` and `curly` share the `//` opener and differ on what a comment
# may also be; `css` shares `curly`'s block form and differs on whether `//` opens anything at all;
# `html` is not a comment grammar at all but a region finder over the other two: see the module
# docstring's check 4.
RUST_EXTS = {".rs"}
CSS_EXTS = {".css"}
HTML_EXTS = {".html"}
# What delimits a string, per lane. Not one set: in Rust a lone `'` is a LIFETIME (`&'a str`), and
# treating it as an open string swallows the rest of the line — which is why Rust tracks `"` only.
# Everywhere else the apostrophe really is a delimiter, and in the JS family so is the backtick;
# missing them makes the guard red on a URL held in an ordinary single-quoted string.
RUST_QUOTES = '"'
CURLY_QUOTES = "\"'`"
HASH_QUOTES = "\"'"
# CSS has no template literal, so a backtick is not a delimiter and must not be read as one: an
# unpaired one would otherwise open a string that swallows every comment below it.
CSS_QUOTES = "\"'"
# The block-comment lanes and how each is read: `(string delimiters, does `//` open a comment)`.
# CSS says no — there `//` is the authority slashes of a `url(…)`, and reading it as an opener turns
# the rest of the URL into prose.
BLOCK_LANES = {"curly": (CURLY_QUOTES, True), "css": (CSS_QUOTES, False)}
# The `.html` lane, which is one fact about the format and nothing else. A `<script>` or `<style>`
# element holds RAW TEXT: the markup grammar stops at the open tag and resumes after the close tag,
# and what lies between belongs to another language whole. So the lane hands each region to the lane
# that already reads that language, and reads only the markup's own `<!-- … -->` form itself.
#
# The open tag is matched QUOTE-AWARE — `"…"` and `'…'` may hold the `>` that would otherwise end it
# — because getting that wrong does not cost a few characters, it costs the file: the stray quote
# opens a string in the delegated scanner, the string eats the close tag, and every comment below,
# markup ones included, goes unread. The bare `[^>]*>` fallback is second in the alternation and
# only reachable when a quote never closes, which is a file no browser agrees about either. The
# alternatives are disjoint on their first character, so the group cannot backtrack pathologically.
HTML_REGION_LANES = {"script": "curly", "style": "css"}
# The other half of the raw-text family, which has no lane because it is not source: `<textarea>` and
# `<title>` hold ESCAPABLE raw text, so a `<!--` inside one is page content a user reads literally,
# not a comment. They are skipped whole rather than scanned — the alternative is reporting a
# citation against text that is rendered on the page, which is the reverse of what this check is for.
# `HTML_REGION_LANES.get` returning None is what tells the walk which of the two it is holding.
HTML_TEXT_ELEMENTS = {"textarea", "title"}
HTML_REGION_RE = re.compile(
    rf"""<({'|'.join(sorted(HTML_REGION_LANES.keys() | HTML_TEXT_ELEMENTS))})\b"""
    r"""(?:(?:[^>"']|"[^"]*"|'[^']*')*>|[^>]*>)""", re.IGNORECASE)
HTML_COMMENT_OPEN, HTML_COMMENT_CLOSE = "<!--", "-->"
# What may follow a close tag's name. HTML5 ends a raw-text element only when the next character is
# whitespace, `/` or `>`, so `</scriptfoo>` closes nothing — and a bare prefix match there would end
# the region EARLY, which is the one failure mode this lane's whole argument is against.
HTML_TAG_END = " \t\n\r\f/>"
# Where a `#` may open a comment, and where a quote may open a string, in the `#` lanes. YAML's rule
# is that `#` comments only at line start or after whitespace — a bare URL's fragment is data, not
# prose — and shell agrees (a `#` mid-word is literal). TOML and Python are laxer, so this is a
# deliberate NARROWING there: an unspaced `x=1#<n>` is missed, which is rarer than the URL it
# stops flagging.
# The quote rule is the same idea one level down: a quote opens a scalar only at a token start, so
# the apostrophe in `name: Charlie's step` stays literal instead of swallowing the comment after it.
HASH_OPENER_BEFORE = " \t"
HASH_QUOTE_BEFORE = " \t[{(,:=-"
# A continuation line's opener, stripped so a comment reads as one string across the line break
# rustfmt (or a human) put in it. `*` and `--` cover a `/* … */` block and SQL-ish comments.
CONT_MARKER_RE = re.compile(r"^(///|//!|//|#|\*|--)[ \t]?")


def lane_key(name: str, first_line: str = "") -> str:
    """The token a file's lane is decided by. Its suffix, except where the suffix is not the format.

    Three shapes, one argument: a file declares its format somewhere, and the suffix is only the
    most common place. Where the guard read the suffix and nothing else, it saw nothing at all.

    A DOT-PREFIXED name IS the format, and anything after a second dot is a qualifier: `.env` and
    `.env.production` are both dotenv (the suffix `.production` names an environment and describes
    no syntax), and `.gitignore` has no suffix whatsoever. Keying the leading segment covers both —
    it is one rule where `.env` alone used to be a special case.

    An EXTENSIONLESS EXECUTABLE declares its format on line one. `#!/bin/sh` is as good a lane
    declaration as `.sh`, so a shebang naming an interpreter whose comments open with `#` keys as
    that lane. An interpreter NOT on that list keys as ITSELF and check 7 reports it, because `#`
    does not open a comment in every language a shebang can name and guessing is what the opener
    table exists to prevent.

    Everything else keys as its lowercased suffix, or as "" — which is a key like any other, and
    carries its reason in NOT_SOURCE_EXTS rather than dropping out of the census.
    """
    name = name.lower()
    if name.startswith("."):
        return "." + name[1:].split(".")[0]
    suffix = PurePath(name).suffix
    if suffix:
        return suffix
    if first_line.startswith("#!"):
        tokens = first_line[2:].split()
        interp = PurePath(tokens[0]).name if tokens else ""
        if interp == "env" and len(tokens) > 1:          # `#!/usr/bin/env bash`
            interp = PurePath(tokens[1]).name
        return "#!" if interp in HASH_INTERPRETERS else (f"#!{interp}" if interp else "")
    return ""


def first_line(path: Path) -> str:
    """The file's first line, for a shebang. A bounded read: a name that declares no format is no
    reason to slurp a font."""
    try:
        with path.open("rb") as fh:
            return fh.readline(256).decode("utf-8", "replace").strip()
    except OSError:
        return ""


def repo_files(root: Path) -> list[Path] | None:
    """Repo content as git defines it, or None if `root` is not inside a working tree.

    The guard's subject is the repository, and `rglob` walks the FILESYSTEM — which is a different
    set the moment anybody runs the tests. A Playwright run leaves `test-results/**/video.webm`
    lying around, and check 7 would dutifully report an unclassified `.webm` and advise giving a
    video file a comment opener. CI never saw it (a clean checkout has no junk) so the guard would
    have been green in the one place it runs and unusable in the place it is read.

    `--cached --others --exclude-standard` is tracked PLUS untracked-but-not-ignored: a file staged
    or merely written is repo content and gets scanned, while everything `.gitignore` disclaims is
    not. Fixing this by naming `test-results` in SKIP_DIRS would be whack-a-mole against a category
    whose next member is not yet named.
    """
    try:
        out = subprocess.run(["git", "-C", str(root), "ls-files", "--cached", "--others",
                              "--exclude-standard", "-z"], capture_output=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    return [root / rel for rel in out.stdout.decode("utf-8", "replace").split("\0") if rel]


def suffix_class_problems(seen: dict[str, str]) -> list[str]:
    """Check 7 for the whole tree: every lane key present is in exactly one of the three buckets.

    `seen` maps a lane key to the first path carrying it, so the report names a file to go look at.
    This is the check that makes default-on true of the CODE. Without it, one set decides both
    "can we read this" and "do we scan this", the two questions collapse, and a lane nobody has
    added is indistinguishable from a lane deliberately let go — which is the allow-list failure
    mode by another name. See the module docstring's check 7.
    """
    problems = []
    for key, rel in sorted(seen.items()):
        if key in CODE_EXTS or key in OUT_OF_REACH_EXTS or key in NOT_SOURCE_EXTS:
            continue
        problems.append(
            f"{rel}:1: unclassified lane `{key}` — reach is default-on, so every lane in the tree is "
            f"one of three things and this is none of them: READABLE (give it an opener row in "
            f"CODE_EXTS), deliberately OUT OF REACH (OUT_OF_REACH_EXTS, and the entry is the reason "
            f"why), or NOT A PROSE SURFACE at all (NOT_SOURCE_EXTS). Pick one — leaving it unsaid is "
            f"the failure this check exists to stop")
    return problems


def lane_of(suffix: str) -> str:
    """Which comment grammar a file is read with. Four, not one per language: what matters is the
    comment syntax and whether a comment can also be something else."""
    if suffix in RUST_EXTS:
        return "rust"
    if suffix in CSS_EXTS:
        return "css"
    if suffix in HTML_EXTS:
        return "html"
    return "hash" if suffix in HASH_EXTS else "curly"


def comment_body(line: str, opener: str = "//", reach_docs: bool = True,
                 quotes: str | None = None) -> tuple[int, str] | None:
    """(index just past the opener, body) of the line's comment, or None if it has none.

    A hand-rolled scan rather than a regex because the opener has to be outside string literals:
    the `//` in `"https://…"` is data, and a regex cannot see the quote that precedes it. With
    `reach_docs` false, a `///` doc comment returns None — check 4 does not reach one IN RUST.

    `quotes` is the lane's string delimiters (see RUST_QUOTES / CURLY_QUOTES / HASH_QUOTES); the
    `#` lane additionally requires an opener to start a word and a quote to start a token.
    """
    if quotes is None:
        quotes = HASH_QUOTES if opener == "#" else RUST_QUOTES
    hashes = opener == "#"
    i, n, in_str = 0, len(line), ""
    while i < n:
        c = line[i]
        if in_str:
            if c == "\\":
                i += 2
                continue
            if c == in_str:
                in_str = ""
        elif c in quotes and not (hashes and i and line[i - 1] not in HASH_QUOTE_BEFORE):
            in_str = c
        elif line.startswith(opener, i) and not (hashes and i and line[i - 1] not in HASH_OPENER_BEFORE):
            j = i + len(opener)
            if opener == "//":
                if line.startswith("///", i) and not reach_docs:
                    return None
                while j < n and line[j] in "/!":      # the `/` of `///`, the `!` of `//!`
                    j += 1
            return j, line[j:]
        i += 1
    return None


def curly_comments(text: str, quotes: str = CURLY_QUOTES,
                   line_comments: bool = True) -> list[tuple[int, str]]:
    """Every comment in a block-comment file as `(offset of the prose, prose)`, one entry per line.

    A whole-file scan rather than a per-line one, because `/* … */` is the only comment form here
    that a line cannot decide on its own: whether a line is prose depends on a `/*` above it. That
    is also what makes the block form reachable at all — reading it line by line is how a `//`
    inside a URL inside a block comment came to be the only thing that made a block comment
    visible. String state is per line except for a backtick, which is the one delimiter JS lets
    span lines.

    `quotes` and `line_comments` are the whole difference between the two lanes that use this
    scanner (BLOCK_LANES): CSS has neither a `//` opener nor a backtick.
    """
    out: list[tuple[int, str]] = []
    in_block = False
    in_str = ""
    pos = 0
    for raw in text.splitlines(True):
        line = raw.rstrip("\n")
        n = len(line)
        i = 0
        start = 0 if in_block else None
        while i < n:
            c = line[i]
            if in_block:
                if line.startswith("*/", i):
                    out.append((pos + start, line[start:i]))
                    in_block, start, i = False, None, i + 2
                    continue
                i += 1
                continue
            if in_str:
                if c == "\\":
                    i += 2
                    continue
                if c == in_str:
                    in_str = ""
                i += 1
                continue
            if c in quotes:
                in_str = c
                i += 1
                continue
            if line_comments and line.startswith("//", i):
                out.append((pos + i + 2, line[i + 2:]))
                i = n
                break
            if line.startswith("/*", i):
                in_block, start, i = True, i + 2, i + 2
                continue
            i += 1
        if in_block and start is not None:
            out.append((pos + start, line[start:]))
        if in_str != "`":
            in_str = ""                    # only a template literal survives the line break
        pos += len(raw)
    return out


def raw_text_end(text: str, start: int, closer: str, lane: str | None) -> int:
    """Where a raw-text element's content ends: the offset of its first REAL close tag.

    Real means two things. The tag name has to actually end — HTML5 admits only whitespace, `/` or
    `>` after it, so `</scriptfoo>` closes nothing, and a bare prefix match there would end the
    region early. And, where the region has a `lane`, the tag has to sit outside that language's own
    STRINGS and BLOCK comments.

    That second half is a deliberate departure from the HTML tokenizer, which ends a raw-text
    element at the first `</script` and nothing else — which is exactly why a page has to spell the
    tag `<\\/script>` inside a JS string. It is a narrow departure, and the boundary is the whole
    argument: a close tag inside a string or a `/* … */` can only occur in a page a browser ALREADY
    reads as broken, so the two readings differ on nothing that works, and of the two ways to be
    wrong on a broken page, ending the region EARLY is the worse one — the tail of the script gets
    read as markup, where a `//` comment is invisible and a stray `-->` closes a comment that never
    opened.

    A `//` LINE comment is deliberately NOT tracked, and that is the same argument reaching the
    opposite answer. `doIt(); // go</script>` is not a broken page, it is an ordinary one: the tag
    ends the comment and the element together, exactly as the author meant. Hiding a close tag
    behind `//` would therefore lose the region on markup that WORKS — every inline script whose
    last line carries a trailing comment — and it would lose it in the swallowing direction, taking
    the whole rest of the file into the script with it. A regex literal is not tracked either — see
    the module docstring.

    `lane` is None for the ESCAPABLE raw-text elements, whose content is text rather than source:
    nothing in a `<title>` opens a string or a comment, so nothing there may hide a close tag.
    """
    quotes = BLOCK_LANES[lane][0] if lane else ""
    i, n = start, len(text)
    in_str, in_block = "", False
    while i < n:
        c = text[i]
        if in_block:
            if text.startswith("*/", i):
                in_block, i = False, i + 1
        elif in_str:
            if c == "\\":
                i += 1
            elif c == in_str or (c == "\n" and in_str != "`"):
                in_str = ""
        elif c in quotes:
            in_str = c
        elif lane and text.startswith("/*", i):
            in_block, i = True, i + 1
        elif c == "<" and text[i:i + len(closer)].lower() == closer:
            after = text[i + len(closer):i + len(closer) + 1]
            if not after or after in HTML_TAG_END:      # `</scriptfoo>` is not a close tag
                return i
        i += 1
    return n


def markup_comments(text: str, lo: int, hi: int) -> list[tuple[int, str, str]]:
    """The `<!-- … -->` comments in one stretch of markup, one entry per line.

    Per line for the same reason `curly_comments` is: a piece is what a reported line number is
    computed from, so a citation on the third line of a comment has to arrive as its own piece.
    An unterminated `<!--` runs to the end of the stretch, matching how an unclosed `/*` is read.
    """
    out: list[tuple[int, str, str]] = []
    i = lo
    while (open_at := text.find(HTML_COMMENT_OPEN, i, hi)) >= 0:
        body = open_at + len(HTML_COMMENT_OPEN)
        close_at = text.find(HTML_COMMENT_CLOSE, body, hi)
        stop = hi if close_at < 0 else close_at
        while body < stop:
            nl = text.find("\n", body, stop)
            eol = stop if nl < 0 else nl
            out.append((body, text[body:eol], "html"))
            body = eol + 1
        i = stop + len(HTML_COMMENT_CLOSE)
    return out


def html_comments(text: str) -> list[tuple[int, str, str]]:
    """Every comment in an HTML file as `(offset of the prose, prose, the lane that read it)`.

    The region finder the `.html` lane is: markup outside a raw-text element goes to
    `markup_comments`, and a `<script>` / `<style>` body goes to the scanner that already reads that
    language, offset back into the whole file so a report still names a real line.

    The lane travels with each piece because it decides more than the opener does. CSS's hex-colour
    collision is a real ambiguity inside `<style>` and is not one in an HTML comment, so the hint
    has to follow the region rather than the file's suffix.

    Pieces come out in ASCENDING offset order, which is not incidental: `fold_pieces` decides what
    joins to what by comparing each piece's line to the one before it, so checks 2, 5 and 6 read a
    shuffled file as a pile of one-line comments.
    """
    out: list[tuple[int, str, str]] = []
    i = mark = 0
    n = len(text)
    while i < n:
        if text.startswith(HTML_COMMENT_OPEN, i):
            close_at = text.find(HTML_COMMENT_CLOSE, i + len(HTML_COMMENT_OPEN))
            i = n if close_at < 0 else close_at + len(HTML_COMMENT_CLOSE)
            continue                   # a `<script>` written inside a comment opens no region
        tag = HTML_REGION_RE.match(text, i) if text[i] == "<" else None
        if tag is None:
            i += 1
            continue
        kind = tag.group(1).lower()
        lane = HTML_REGION_LANES.get(kind)          # None for `<textarea>` / `<title>`: skipped
        body = tag.end()
        end = raw_text_end(text, body, f"</{kind}", lane)
        out.extend(markup_comments(text, mark, tag.start()))
        if lane is not None:
            quotes, line_comments = BLOCK_LANES[lane]
            out.extend((body + off, piece, lane)
                       for off, piece in curly_comments(text[body:end], quotes, line_comments))
        close_tag_end = text.find(">", end)
        i = mark = n if close_tag_end < 0 else close_tag_end + 1
    out.extend(markup_comments(text, mark, n))
    return out


def fold_pieces(text: str, pieces: list[tuple[int, str]]) -> tuple[str, list[int]]:
    """`fold_comments` for a lane scanned whole-file rather than line-by-line.

    Same folding rule as the line-by-line version — consecutive lines are one comment and join with
    a space, a gap starts a new one — but the pieces come from a whole-file scan, so a pointer
    wrapped inside a block comment is read whole instead of not at all.
    """
    line_starts = [0] + [i + 1 for i, c in enumerate(text) if c == "\n"]
    out: list[str] = []
    idx: list[int] = []
    prev_line = None
    for off, piece in pieces:
        line = bisect.bisect_right(line_starts, off)
        stripped = piece.lstrip()
        lead = len(piece) - len(stripped)
        cont = CONT_MARKER_RE.match(stripped) if prev_line == line - 1 else None
        start = off + lead + (cont.end() if cont else 0)
        body = piece[lead + (cont.end() if cont else 0):]
        if out:
            out.append(" " if prev_line == line - 1 else "\n")
            idx.append(start)
        for k, ch in enumerate(body):
            out.append(ch)
            idx.append(start + k)
        prev_line = line
    return "".join(out), idx


def fold_curly_comments(text: str, quotes: str = CURLY_QUOTES,
                        line_comments: bool = True) -> tuple[str, list[int]]:
    """`fold_pieces` over the block-comment scan, so a `/* … */` is prose too."""
    return fold_pieces(text, curly_comments(text, quotes, line_comments))


def fold_comments(text: str, opener: str = "//", quotes: str | None = None) -> tuple[str, list[int]]:
    """The file's comment prose alone, line breaks inside one comment folded to a space.

    Two things a line-by-line scan gets wrong, fixed in one pass. A pointer is a sentence and gets
    broken wherever the column runs out — `see rules:` on one line, its topic on the next — so
    reading a line sees a fragment and matches nothing, which reads as a clean file. And a pointer
    in a string literal (this guard's own test fixtures) is data, not prose, so code is dropped
    rather than scanned. The returned list maps each folded offset back to its source offset, so a
    problem still names the line the reader has to go edit.
    """
    out: list[str] = []
    idx: list[int] = []
    pos = 0
    open_comment = False
    for raw in text.splitlines(True):
        body = raw.rstrip("\n")
        stripped = body.lstrip()
        lead = len(body) - len(stripped)
        cont = CONT_MARKER_RE.match(stripped) if open_comment else None
        if cont:
            start, prose = lead + cont.end(), " "
        else:
            found = comment_body(body, opener, quotes=quotes)
            if not found:
                open_comment = False
                continue
            start, prose = found[0], "\n"
        if out:
            out.append(prose)
            idx.append(pos + start)
        for k in range(start, len(body)):
            out.append(body[k])
            idx.append(pos + k)
        open_comment = True
        pos += len(raw)
    return "".join(out), idx


def module_doc_problems(rel: str, text: str) -> list[str]:
    """Check 3 for one Rust file: the leading `//!` block is short, or it names a topic.

    The block is the first run of `//!` lines and ends at the first line that is not one. Reaching
    it means stepping over everything that legitimately precedes a module doc — blank lines, inner
    attributes (`#![…]`, which wrap across lines), a licence header in `//` or `/* … */`. Each of
    those is a way to put the doc out of the scan's reach, so each is skipped rather than treated
    as the first line of code.
    """
    doc: list[str] = []
    in_block = False                   # inside a /* … */
    attr_depth = 0                     # inside an inner attribute wrapped across lines
    for line in text.splitlines():
        s = line.strip()
        if in_block:
            in_block = "*/" not in s
            continue
        if attr_depth:
            attr_depth += s.count("[") - s.count("]")
            continue
        if s.startswith("//!"):
            doc.append(s)
            continue
        if doc:
            break                      # the block ended: first line of real code
        if not s or s.startswith("//"):
            continue                   # blank, or a plain comment sitting above the module doc
        if s.startswith("/*"):
            in_block = "*/" not in s[2:]
            continue
        if s.startswith("#!"):
            attr_depth = s.count("[") - s.count("]")
            continue
        break                          # code before any module doc: there is none
    if len(doc) <= MAX_UNPOINTED_MODULE_DOC or SEE_RE.search("\n".join(doc)):
        return []
    return [f"{rel}:1: {len(doc)}-line `//!` module doc names no topic — point the rationale at "
            f"one (`//! see rules: <topic>`) or cut it to mechanics "
            f"(<={MAX_UNPOINTED_MODULE_DOC} lines)"]


def comment_ref_problems(rel: str, text: str, lane: str = "rust") -> list[str]:
    """Check 4 for one file: no comment cites an issue or a rule anchor.

    Reads comment prose only, so an issue number inside a string literal (a test fixture, a URL)
    is not a violation — the target is prose that retells history, not data. `lane` selects the
    grammar and, with it, what is out of reach; the module docstring's check 4 argues each.

    Each piece carries the lane that READ it, which is the file's lane everywhere except `.html` —
    there one file holds three regions and a `<style>` comment is CSS however the file is named.
    """
    line_starts = [0] + [i + 1 for i, c in enumerate(text) if c == "\n"]

    def line_at(off: int) -> int:
        lo, hi = 0, len(line_starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            lo, hi = (mid, hi) if line_starts[mid] <= off else (lo, mid - 1)
        return lo + 1

    pieces: list[tuple[int, str, str]] = []
    if lane == "html":
        pieces = html_comments(text)
    elif lane in BLOCK_LANES:
        pieces = [(off, body, lane) for off, body in curly_comments(text, *BLOCK_LANES[lane])]
    else:
        opener = "#" if lane == "hash" else "//"
        quotes = HASH_QUOTES if lane == "hash" else RUST_QUOTES
        pos = 0
        for raw in text.splitlines(True):
            found = comment_body(raw.rstrip("\n"), opener, reach_docs=False, quotes=quotes)
            if found:
                # The opener goes back on the front before the scan. In a `#` lane the FIRST
                # citation on a line is the thing that opens the comment, as far as the scanner is
                # concerned — its own hash IS the opener — so the body starts one character too
                # late and the citation arrives as a bare number. Re-attaching costs Rust nothing:
                # neither pattern can match into a `//`.
                pieces.append((pos + found[0], opener + found[1], lane))
            pos += len(raw)

    problems = []
    for off, body, read_as in pieces:
        i = line_at(off)
        for ref in ISSUE_RE.findall(body):
            problems.append(f"{rel}:{i}: issue citation `{ref}` in a comment — provenance belongs "
                            f"in a rationale file, not in code; point at a topic instead"
                            + (COLOUR_HINT if read_as == "css" and SHORT_HEX_RE.match(ref) else ""))
        for ref in RULE_ANCHOR_RE.findall(body):
            problems.append(f"{rel}:{i}: rule-level pointer `{ref}` in a comment — point at the "
                            f"topic instead (`see rules: <topic>`)")
    return problems


def folded_with_lines(text: str, lane: str = "rust"):
    """`(folded prose, line_at)` — the comment-folded view plus a resolver from a folded offset
    back to the 1-based source line, so a problem names the line a reader has to go edit."""
    if lane == "html":
        folded, idx = fold_pieces(text, [(off, body) for off, body, _ in html_comments(text)])
    elif lane in BLOCK_LANES:
        folded, idx = fold_curly_comments(text, *BLOCK_LANES[lane])
    else:
        opener = "#" if lane == "hash" else "//"
        folded, idx = fold_comments(text, opener,
                                    HASH_QUOTES if lane == "hash" else RUST_QUOTES)
    line_starts = [0] + [i + 1 for i, c in enumerate(text) if c == "\n"]

    def line_at(pos: int) -> int:
        src = idx[pos] if pos < len(idx) else len(text)
        lo, hi = 0, len(line_starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            lo, hi = (mid, hi) if line_starts[mid] <= src else (lo, mid - 1)
        return lo + 1

    return folded, line_at


def parity_problems(rel: str, text: str, lane: str = "rust") -> list[str]:
    """Check 6 for one file: every parity marker that exists records a substantive reason.

    Runs over the comment-folded view for the same two reasons checks 2 and 5 do — a reason
    wrapped across a line break is counted whole, and a marker inside a string literal (this
    guard's own fixtures) is data rather than prose.
    """
    problems = []
    folded, line_at = folded_with_lines(text, lane)
    for m in PARITY_ANYCASE_RE.finditer(folded):
        i = line_at(m.start())
        marker = folded[m.start():m.end()].strip()
        if not marker.startswith("Parity:"):
            problems.append(f"{rel}:{i}: mis-cased `{marker}` — the marker is capitalised, and only "
                            f"that spelling is validated")
            continue
        reason = PARITY_RE.match(folded, m.start()).group(1).strip()
        if len(reason.split()) < MIN_PARITY_REASON_WORDS:
            problems.append(f"{rel}:{i}: parity marker records no reason ({reason!r}) — name why "
                            f"one list cannot be generated from the other, in at least "
                            f"{MIN_PARITY_REASON_WORDS} words; a test asserting two lists match is "
                            f"evidence about the design, and this is where it says so")
    return problems


def pointer_problems(rel: str, text: str, is_topic, lane: str = "rust") -> list[str]:
    """Checks 2 and 5 for one file: every `see rules:` pointer resolves, and stops at its topic.

    Both run over the comment-folded view (`fold_comments`), so a pointer wrapped across a line
    break is read whole rather than missed. `is_topic(cross, slug)` answers whether a slug
    resolves to a topic doc; the two checks share it so they agree on what a topic is.
    """
    problems = []
    folded, line_at = folded_with_lines(text, lane)

    for m in SEE_ANYCASE_RE.finditer(folded):
        i = line_at(m.start())
        if folded[m.start():m.start() + 3] != "see":
            problems.append(f"{rel}:{i}: capitalised `{folded[m.start():m.end()].strip()}` — the "
                            f"grammar is lowercase `see rules: <topic>`, and only that spelling is "
                            f"validated")
            continue
        pointer = SEE_RE.match(folded, m.start())
        if not pointer:
            continue                   # prose *about* the grammar, e.g. this file's own docstring
        cross, topic = pointer.group(1), pointer.group(2)
        if not SLUG_RE.match(topic):
            problems.append(f"{rel}:{i}: malformed rules slug '{topic}' (kebab-case expected)")
            continue
        if not is_topic(cross, topic):
            where = "engine/docs/rules" if cross else "docs/rules"
            problems.append(f"{rel}:{i}: `see {'engine ' if cross else ''}rules: {topic}` has no "
                            f"{where}/{topic}.md")
        tail = folded[pointer.end():pointer.end() + 160]
        named: list[str] = []
        if anchor := POINTER_ANCHOR_RE.match(tail):
            named = [anchor.group(1)]
        elif parens := POINTER_PARENS_RE.match(tail):
            named = [s.strip() for s in parens.group(1).split(",")]
        elif commas := POINTER_COMMAS_RE.match(tail):
            named = [s.strip() for s in commas.group(1).split(",") if s.strip()]
        for slug in named:
            if not is_topic(cross, slug):
                problems.append(f"{rel}:{i}: `see rules: {topic}` reaches past its topic to "
                                f"`{slug}` — code points at topics only, so a rule can be reworded "
                                f"without silently breaking the pointer")
    return problems


def main(root_arg: str = ".") -> int:
    root = Path(root_arg).resolve()
    errors: list[str] = []

    def topic_path(cross, slug: str) -> Path:
        """Where a topic doc lives: in-repo, or under the pinned engine submodule for the
        cross-repo form (the SHA web is built against)."""
        base = root / "engine" / "docs" / "rules" if cross else root / "docs" / "rules"
        return base / f"{slug}.md"

    def is_topic(cross, slug: str) -> bool:
        return bool(SLUG_RE.match(slug)) and topic_path(cross, slug).exists()

    seen: dict[str, str] = {}                  # lane key -> first path carrying it, for check 7
    listing = repo_files(root)
    for path in (listing if listing is not None else root.rglob("*")):
        if not path.is_file():
            continue
        parts = path.relative_to(root).parts
        if set(parts) & SKIP_DIRS:
            continue
        if any(parts[:len(pre)] == pre for pre in SKIP_PREFIXES):
            continue
        rel = path.relative_to(root).as_posix()
        # Classify BEFORE the readability gate — a lane the guard cannot read still has to be
        # accounted for — and before SKILL_ALLOWLIST, which exempts a skill from the ADR-token rule
        # and from nothing else. A `.scss` under an exempt skill is still an unclassified lane.
        # The shebang peek costs one short read, and only for a file whose NAME said nothing; the
        # alternative is slurping every font and wasm blob to look at a line they do not have.
        key = lane_key(path.name) or lane_key(path.name, first_line(path))
        seen.setdefault(key, rel)
        if set(parts) & SKILL_ALLOWLIST or key not in CODE_EXTS:
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        if key == ".rs":
            errors.extend(module_doc_problems(rel, text))
        lane = lane_of(key)
        errors.extend(comment_ref_problems(rel, text, lane))
        errors.extend(pointer_problems(rel, text, is_topic, lane))
        errors.extend(parity_problems(rel, text, lane))
        for i, line in enumerate(text.splitlines(), 1):
            if ADR_RE.search(line):
                errors.append(f"{rel}:{i}: ADR reference in code — point at a topic: `see rules: <topic>`")
    errors.extend(suffix_class_problems(seen))
    for e in errors:
        print(e, file=sys.stderr)
    print(f"check_rules_refs: {len(errors)} problem(s)", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:2]))

# ii:begin provenance — derived from .ii/repo.toml; do not hand-edit out of sync. Regenerate with `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`. sha256=d1a7da31a30a2d37ef0ed45957c3632cce4f2b2134ea353e19445a380029f573
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/check_rules_refs.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/check_rules_refs.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_refs.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
# ii:end provenance
