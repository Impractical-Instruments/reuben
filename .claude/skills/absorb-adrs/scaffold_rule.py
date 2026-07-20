#!/usr/bin/env python3
"""Scaffold a guard-passing rule<->rationale pair for the rules-doc system.

The `absorb-adrs` skill's steps 2+3 (write the rule, write its rationale) have a fiddly, easy-to-
break invariant that the S01 link guard enforces: a rule is a raw-HTML `<a id="slug"></a>` anchor
above an `### H3`, carrying EXACTLY ONE `[why](rationale/<topic>/<slug>.md)` link whose target file
must exist, strictly 1:1 rule<->rationale. Hand-typing that (anchor grammar, the relative path, the
em-dash conventions) is where a human/agent slips. This helper does the mechanical part so the
author is left with only the judgement: the normative sentence, the "now" story, and the reasoning.

It is deliberately small and leans on the S01 templates + guards rather than re-implementing them:
  - a NEW topic doc is scaffolded as a guard-passing skeleton mirroring `_templates/topic.md`
    (title + summary filled; `## Now` and `## Terms` left as TODO placeholders for the author);
  - the rule block is appended into the topic's `## Rules` section (before `## Terms`);
  - the rationale doc is instantiated from `_templates/rationale.md` with the rule ref + provenance
    line filled and the body left as a TODO.
It refuses to clobber an existing rationale and refuses a duplicate rule slug, so re-running is safe.
It does NOT collate README (run `check_rules_derive.py --write`) or write real prose — that is the
author's / skill's job. Portable: repo-relative `docs/rules/` + `_templates/`, present in both repos.

Usage:
  python3 scaffold_rule.py --topic <slug> --title "<Title>" --summary "<one-line summary>" \
      --rule <slug> --heading "<present-tense normative statement.>" \
      [--from "ADR-0007[, ADR-0008]"] [--root .]

Stdlib only. Exit 0 on success; non-zero with a `message` on any refusal/error.
"""
from __future__ import annotations
import argparse, re, sys
from pathlib import Path

SLUG_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")  # same kebab-case shape the refs guard demands
BODY_TODO = ("<!-- TODO(absorb-adrs): the condensed reasoning that still applies — the forces that "
             "make the rule the right call; drop superseded/dead-end history (git keeps it). -->")
NOW_TODO = ("<!-- TODO(absorb-adrs): the present-tense \"now\" story for this topic — prose, not a "
            "rule list. Mirror the shape of docs/rules/_templates/topic.md. -->")
TERMS_TODO = ("<!-- TODO(absorb-adrs): this topic's defining terms, one per line as "
              "`- **Term** — definition`; collated into the rules-index glossary. -->")


def skeleton(title: str, summary: str) -> str:
    """A new topic doc with an EMPTY `## Rules` section — guard-passing once a rule is inserted.
    Mirrors `_templates/topic.md`; the author replaces the two TODO placeholders with real prose."""
    return (f"# {title}\n\n"
            f"> {summary}\n\n"
            f"## Now\n\n{NOW_TODO}\n\n"
            f"## Rules\n\n"
            f"## Terms\n\n{TERMS_TODO}\n")


def insert_rule(text: str, topic: str, rule: str, heading: str) -> str:
    """Append a rule block to the end of the topic's `## Rules` section (before the next `## `)."""
    block = [f'<a id="{rule}"></a>', f"### {heading}", "", f"[why](rationale/{topic}/{rule}.md)"]
    lines = text.split("\n")
    try:
        rules_idx = next(i for i, l in enumerate(lines) if l.strip() == "## Rules")
    except StopIteration:
        raise ValueError("topic doc has no `## Rules` section")
    boundary = next((i for i in range(rules_idx + 1, len(lines)) if lines[i].startswith("## ")),
                    len(lines))
    insert_at = boundary
    while insert_at - 1 > rules_idx and lines[insert_at - 1].strip() == "":
        insert_at -= 1  # trim trailing blanks so we control the single separating blank line
    # Tail resumes at `boundary` (not `insert_at`), dropping the old trailing blanks we just skipped,
    # so exactly one blank line separates the block from the next `## ` heading.
    new = lines[:insert_at] + [""] + block + [""] + lines[boundary:]
    return "\n".join(new)


def render_rationale(template: str, topic: str, rule: str, heading: str, provenance: str) -> str:
    """Instantiate `_templates/rationale.md`: fill the `# Why:` line, the `[Rule]` back-link, and
    the `Distilled from:` provenance, and collapse the reasoning body to a single TODO.

    The body is scoped STRUCTURALLY to the region between the `[Rule]` line and the `Distilled from:`
    line, so it becomes exactly one `BODY_TODO` no matter how many `<...>` placeholders the template
    body carries. (A whole-text `<...>` regex would silently multiply them into several TODOs if the
    template ever grew a second placeholder — mangling the rationale with no red test.) Raises a clear
    error if the two structural anchors are missing or out of order, so template drift is caught."""
    lines = template.split("\n")
    rule_idx = next((i for i, l in enumerate(lines) if l.startswith("[Rule]")), None)
    prov_idx = next((i for i, l in enumerate(lines) if l.startswith("Distilled from:")), None)
    if rule_idx is None or prov_idx is None or prov_idx <= rule_idx:
        raise ValueError("rationale template missing an ordered `[Rule]` ... `Distilled from:` "
                         "pair — cannot scope the reasoning body")
    out: list[str] = []
    for i, ln in enumerate(lines):
        if i == rule_idx:
            out += [f"[Rule](../../{topic}.md#{rule})", "", BODY_TODO, ""]  # body -> one TODO
        elif rule_idx < i < prov_idx:
            continue  # drop the template's whole body region (any number of placeholders)
        elif i == prov_idx:
            out.append(f"Distilled from: {provenance}")
        elif ln.startswith("# Why:"):
            out.append(f"# Why: {heading}")
        else:
            out.append(ln)
    return "\n".join(out)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="Scaffold a rule<->rationale pair (absorb-adrs).")
    ap.add_argument("--topic", required=True, help="topic slug (kebab-case), e.g. signal-time-dsp")
    ap.add_argument("--title", required=True, help="topic title, e.g. 'Signal / OSC / time / DSP'")
    ap.add_argument("--summary", required=True, help="one-line topic summary (the index line)")
    ap.add_argument("--rule", required=True, help="rule slug (kebab-case), unique within the topic")
    ap.add_argument("--heading", required=True, help="present-tense normative statement, one sentence")
    ap.add_argument("--from", dest="provenance", default="ADR-XXXX (fill in)",
                    help="provenance for the `Distilled from:` line, e.g. 'ADR-0007'")
    ap.add_argument("--root", default=".", help="repo root (default '.')")
    args = ap.parse_args(argv)

    for label, slug in (("topic", args.topic), ("rule", args.rule)):
        if not SLUG_RE.match(slug):
            print(f"error: {label} slug '{slug}' is not kebab-case", file=sys.stderr)
            return 1

    rules = Path(args.root).resolve() / "docs" / "rules"
    templates = rules / "_templates"
    topic_tpl, rat_tpl = templates / "topic.md", templates / "rationale.md"
    for tpl in (topic_tpl, rat_tpl):
        if not tpl.exists():
            print(f"error: missing template {tpl}", file=sys.stderr)
            return 1

    topic_path = rules / f"{args.topic}.md"
    rationale_path = rules / "rationale" / args.topic / f"{args.rule}.md"

    if rationale_path.exists():
        print(f"error: refusing to clobber existing rationale {rationale_path}", file=sys.stderr)
        return 1

    if topic_path.exists():
        text = topic_path.read_text(encoding="utf-8")
        if re.search(rf'<a\s+id="{re.escape(args.rule)}"\s*>', text):
            print(f"error: rule '{args.rule}' already exists in {topic_path.name}", file=sys.stderr)
            return 1
        created_topic = False
    else:
        text = skeleton(args.title, args.summary)
        created_topic = True

    try:
        text = insert_rule(text, args.topic, args.rule, args.heading)
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    rationale = render_rationale(rat_tpl.read_text(encoding="utf-8"),
                                 args.topic, args.rule, args.heading, args.provenance)

    topic_path.parent.mkdir(parents=True, exist_ok=True)
    topic_path.write_text(text, encoding="utf-8")
    rationale_path.parent.mkdir(parents=True, exist_ok=True)
    rationale_path.write_text(rationale, encoding="utf-8")

    print(f"{'created' if created_topic else 'updated'} topic  {topic_path}")
    print(f"created rationale {rationale_path}")
    print("next: fill the TODOs (now-story, terms, reasoning), then run "
          "check_rules_derive.py --write and the guards.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
