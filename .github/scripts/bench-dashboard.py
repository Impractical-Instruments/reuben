#!/usr/bin/env python3
"""Render the bench-history dashboard (layer 2).

Reads the `bench-history.jsonl` series (one {sha,commit_sha,date,run_id,layer,case,ir} object
per benched case per push to the source branch) and writes a README.md plus light/dark SVG line
charts next to it. CI runs this from bench-history-append.sh so the trend branch (`bench-history`
for main, `bench-history-dev` for dev) renders as a dashboard when browsed on GitHub — no Pages
setup, no external services, works on a private repo. stdlib only: the runner's system python3 is
the whole toolchain.

The optional branch-label arg (default `main`) is the source branch this trend belongs to; it is
substituted into the human copy only, so the same chart logic renders both trends.

Usage: bench-dashboard.py <bench-history.jsonl> <outdir> [branch-label]

Any subset of the data must render: a series with only one layer, only stubs, or a missing
case skips that chart/section instead of crashing (the append script treats a crash as
best-effort, so a crash here means a stale dashboard).

Chart styling follows the reference data-viz palette (validated for CVD separation in both
modes); identity is never color-alone — every series carries a direct end label and every
number is repeated in a table.
"""

import json
import math
import os
import sys
from collections import defaultdict

# A first data point can be a registration stub (an operator whose workload landed a commit
# later benches as a ~11-Ir no-op once). Anything below this floor is not a real measurement;
# the cheapest real case (subpatch, the no-op format anchor) sits around 500k Ir. The floor
# must live here (not at harvest): the recorded history already contains stub rows.
STUB_FLOOR = 1_000

# The dedicated per-node overhead case: a bench-only no-op operator (bench_support.rs) whose
# entire Ir is the engine's per-node stepping overhead (edge clear, routing, materialize, `Io`
# build). The proxy is the cheapest value-rate case — whose `process` does ~nothing, so ~99%
# of its Ir is the same overhead — charted alongside to cover history recorded before the
# dedicated case landed.
OVERHEAD_CASE = "overhead"
OVERHEAD_FALLBACK = "abs_f32_value"

# Keep in lockstep with `bench_support::BLOCKS` (the fixed schedule's single source of truth):
# 375 * 128 frames @ 48 kHz == exactly 1 s. Feeds the instructions-per-block figure only.
BLOCKS = 375

# Bold threshold for table deltas — keep in lockstep with WARN_PCT in perf-gate.sh.
WARN_PCT = 3.0

REPO = os.environ.get("GITHUB_REPOSITORY", "Impractical-Instruments/reuben")

# Reference categorical palette, slots 1..8, stepped per mode. Ordering is the CVD-safety
# mechanism — never reorder or cycle.
CATEGORICAL = {
    "light": ["#2a78d6", "#1baf7a", "#eda100", "#008300", "#4a3aa7", "#e34948", "#e87ba4", "#eb6834"],
    "dark": ["#3987e5", "#199e70", "#c98500", "#008300", "#9085e9", "#e66767", "#d55181", "#d95926"],
}
CHROME = {
    "light": {
        "surface": "#fcfcfb", "ink": "#0b0b0b", "ink2": "#52514e", "muted": "#898781",
        "grid": "#e1e0d9", "axis": "#c3c2b7",
    },
    "dark": {
        "surface": "#1a1a19", "ink": "#ffffff", "ink2": "#c3c2b7", "muted": "#898781",
        "grid": "#2c2c2a", "axis": "#383835",
    },
}
FONT = 'system-ui, -apple-system, "Segoe UI", sans-serif'


def load(path):
    """Parse the JSONL into (ordered commits, {(layer, case): {commit_index: ir}})."""
    order, seen = [], {}
    series = defaultdict(dict)
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            if r["sha"] not in seen:
                seen[r["sha"]] = len(order)
                order.append({"sha": r["sha"], "date": r["date"]})
            series[(r["layer"], r["case"])][seen[r["sha"]]] = r["ir"]
    return order, series


def real_points(points_by_commit):
    """Series points with leading registration stubs dropped (sorted (idx, ir) list)."""
    items = sorted(points_by_commit.items())
    while items and items[0][1] < STUB_FLOOR:
        items.pop(0)
    return items


def fmt_ir(v):
    if v >= 10_000_000:
        return f"{v / 1e6:,.1f}M"
    if v >= 1_000_000:
        return f"{v / 1e6:,.2f}M"
    if v >= 10_000:
        return f"{v / 1e3:,.0f}k"
    return f"{v:,}"


def nice_ticks(lo, hi, n=5):
    """Clean 1-2-5 axis ticks covering [lo, hi]."""
    if hi <= lo:
        hi = lo + 1
    raw = (hi - lo) / n
    mag = 10 ** math.floor(math.log10(raw))
    # Fallback covers float underestimation of log10 at a power-of-ten boundary (raw/mag can
    # then sit epsilon above 10, exhausting the candidates).
    step = 10 * mag
    for s in (1, 2, 2.5, 5, 10):
        if s * mag >= raw:
            step = s * mag
            break
    start = math.floor(lo / step) * step
    ticks = []
    t = start
    while t <= hi + step * 0.001:
        if t >= lo - step * 0.001:
            ticks.append(t)
        t += step
    return ticks


def esc(s):
    """Escape text for SVG/HTML content AND double-quoted attributes (aria-label, alt)."""
    return (
        s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")
    )


def line_chart(mode, title, subtitle, order, named_series, unit_div, unit_label,
               zero_base=True, width=880, height=340):
    """One themed SVG line chart. named_series: [(name, [(commit_idx, ir), ...])] — already
    stub-stripped and non-empty (write_chart guards); slots assigned in list order."""
    c = CHROME[mode]
    pal = CATEGORICAL[mode]
    pad_l, pad_r, pad_t, pad_b = 64, 150, 56, 34
    pw, ph = width - pad_l - pad_r, height - pad_t - pad_b

    all_vals = [v / unit_div for _, p in named_series for _, v in p]
    n_commits = len(order)
    lo = 0.0 if zero_base else min(all_vals)
    hi = max(all_vals)
    if not zero_base:
        span = max(hi - lo, hi * 0.001)
        lo, hi = lo - span * 0.08, hi + span * 0.08
    ticks = nice_ticks(lo, hi)
    lo, hi = min(lo, ticks[0]), max(hi, ticks[-1])

    def x(i):
        return pad_l + (pw * i / max(n_commits - 1, 1))

    def y(v):
        return pad_t + ph - ph * (v - lo) / (hi - lo)

    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        + f'viewBox="0 0 {width} {height}" role="img" aria-label="{esc(title)}">',
        f'<rect width="{width}" height="{height}" rx="6" fill="{c["surface"]}"/>',
        f'<text x="{pad_l}" y="24" font-family=\'{FONT}\' font-size="14" font-weight="600" '
        + f'fill="{c["ink"]}">{esc(title)}</text>',
        f'<text x="{pad_l}" y="41" font-family=\'{FONT}\' font-size="11" '
        + f'fill="{c["ink2"]}">{esc(subtitle)}</text>',
    ]

    # Gridlines + y ticks (hairline, recessive; tabular figures).
    for t in ticks:
        ty = y(t)
        out.append(f'<line x1="{pad_l}" y1="{ty:.1f}" x2="{pad_l + pw}" y2="{ty:.1f}" '
                   f'stroke="{c["grid"]}" stroke-width="1"/>')
        label = f"{t:,.6g}"
        out.append(f'<text x="{pad_l - 8}" y="{ty + 3.5:.1f}" text-anchor="end" '
                   f'font-family=\'{FONT}\' font-size="11" '
                   f'style="font-variant-numeric: tabular-nums" fill="{c["muted"]}">{label}</text>')
    out.append(f'<text x="{pad_l - 8}" y="{pad_t - 10}" text-anchor="end" font-family=\'{FONT}\' '
               f'font-size="10" fill="{c["muted"]}">{esc(unit_label)}</text>')

    # Baseline + x ticks: first commit of each new date, thinned to <= 8 labels.
    base_y = pad_t + ph
    out.append(f'<line x1="{pad_l}" y1="{base_y}" x2="{pad_l + pw}" y2="{base_y}" '
               f'stroke="{c["axis"]}" stroke-width="1"/>')
    day_firsts = []
    prev_day = None
    for i, cm in enumerate(order):
        day = cm["date"][:10]
        if day != prev_day:
            day_firsts.append((i, day[5:]))
            prev_day = day
    step = max(1, math.ceil(len(day_firsts) / 8))
    for i, label in day_firsts[::step]:
        out.append(f'<text x="{x(i):.1f}" y="{base_y + 16}" text-anchor="middle" '
                   f'font-family=\'{FONT}\' font-size="11" '
                   f'style="font-variant-numeric: tabular-nums" fill="{c["muted"]}">{label}</text>')

    # Series: 2px lines broken at gaps, end dot with surface ring, direct end labels
    # (collision-resolved; leader line when a label had to move off its line end).
    labels = []
    for si, (name, pts) in enumerate(named_series):
        color = pal[si % len(pal)]
        runs, run = [], [pts[0]]
        for a, b in zip(pts, pts[1:]):
            if b[0] == a[0] + 1:
                run.append(b)
            else:
                runs.append(run)
                run = [b]
        runs.append(run)
        for r in runs:
            d = " ".join(f"{'M' if j == 0 else 'L'}{x(i):.1f},{y(v / unit_div):.1f}"
                         for j, (i, v) in enumerate(r))
            if len(r) == 1:
                i, v = r[0]
                out.append(f'<circle cx="{x(i):.1f}" cy="{y(v / unit_div):.1f}" r="2.5" '
                           f'fill="{color}"/>')
            else:
                out.append(f'<path d="{d}" fill="none" stroke="{color}" stroke-width="2" '
                           f'stroke-linejoin="round" stroke-linecap="round"/>')
        li, lv = pts[-1]
        ex, ey = x(li), y(lv / unit_div)
        out.append(f'<circle cx="{ex:.1f}" cy="{ey:.1f}" r="6" fill="{c["surface"]}"/>')
        out.append(f'<circle cx="{ex:.1f}" cy="{ey:.1f}" r="4" fill="{color}"/>')
        labels.append({"name": name, "value": lv, "color": color, "lx": ex, "ly": ey, "ty": ey})

    # Resolve end-label collisions: spread downward to min_gap, then shift the block back up if
    # it overflowed the baseline. The uniform shift preserves the gaps, so no second pass.
    labels.sort(key=lambda l: l["ty"])
    min_gap = 28  # each end label is two 11px lines (name + value)
    for a, b in zip(labels, labels[1:]):
        if b["ty"] - a["ty"] < min_gap:
            b["ty"] = a["ty"] + min_gap
    over = (labels[-1]["ty"] - (pad_t + ph + 4)) if labels else 0
    if over > 0:
        for l in labels:
            l["ty"] -= over
    for l in labels:
        tx = l["lx"] + 12
        if abs(l["ty"] - l["ly"]) > 4:  # leader line: the label moved off its line end
            out.append(f'<line x1="{l["lx"] + 6:.1f}" y1="{l["ly"]:.1f}" x2="{tx - 2:.1f}" '
                       f'y2="{l["ty"]:.1f}" stroke="{CHROME[mode]["axis"]}" stroke-width="1"/>')
        out.append(f'<circle cx="{tx + 3}" cy="{l["ty"] - 3.5:.1f}" r="3.5" fill="{l["color"]}"/>')
        out.append(f'<text x="{tx + 11}" y="{l["ty"]:.1f}" font-family=\'{FONT}\' font-size="11" '
                   f'fill="{c["ink"]}">{esc(l["name"])}</text>')
        out.append(f'<text x="{tx + 11}" y="{l["ty"] + 12:.1f}" font-family=\'{FONT}\' '
                   f'font-size="10" style="font-variant-numeric: tabular-nums" '
                   f'fill="{c["ink2"]}">{fmt_ir(l["value"])}</text>')

    out.append("</svg>")
    return "\n".join(out)


def picture(basename, alt):
    return (
        "<picture>\n"
        f'  <source media="(prefers-color-scheme: dark)" srcset="charts/{basename}-dark.svg">\n'
        f'  <img alt="{esc(alt)}" src="charts/{basename}-light.svg">\n'
        "</picture>"
    )


def write_chart(outdir, basename, order, named_series, title, subtitle, unit_div, unit_label,
                zero_base=True):
    """Render both mode variants; returns False (writing nothing) when there is no data, so a
    one-layer or all-stub history skips the chart instead of crashing the render."""
    named_series = [(n, p) for n, p in named_series if p]
    if not named_series:
        return False
    for mode in ("light", "dark"):
        svg = line_chart(mode, title, subtitle, order, named_series, unit_div, unit_label,
                         zero_base=zero_base)
        with open(os.path.join(outdir, "charts", f"{basename}-{mode}.svg"), "w",
                  encoding="utf-8") as f:
            f.write(svg)
    return True


def delta_cell(cur, base):
    """Signed percent change, bolded past the gate's warn line; em dash when there is no base."""
    if not base:
        return "—"
    pct = 100.0 * (cur - base) / base
    s = "±0.0%" if abs(pct) < 0.05 else f"{pct:+.1f}%"
    return f"**{s}**" if abs(pct) > WARN_PCT else s


def series_row(name, points, order):
    """One table row: latest, Δ vs previous point, Δ vs first real point."""
    latest_i, latest = points[-1]
    prev = points[-2][1] if len(points) >= 2 else None
    first_i, first = points[0]
    return (f"| `{name}` | {fmt_ir(latest)} | {delta_cell(latest, prev)} | "
            f"{delta_cell(latest, first)} | {order[first_i]['date'][:10]} |")


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit("usage: bench-dashboard.py <bench-history.jsonl> <outdir> [branch-label]")
    jsonl, outdir = sys.argv[1], sys.argv[2]
    # The source branch this trend belongs to (main -> `bench-history`, dev -> `bench-history-dev`),
    # substituted into the human copy only — the chart-drawing logic is branch-agnostic. Defaults to
    # `main` so the pre-existing two-arg invocation renders exactly as before.
    label = sys.argv[3] if len(sys.argv) == 4 else "main"
    other_label, other_branch = (
        ("dev", "bench-history-dev") if label == "main" else ("main", "bench-history"))
    order, series = load(jsonl)
    if not order:
        print("bench-dashboard: no data points; nothing to render.")
        return
    os.makedirs(os.path.join(outdir, "charts"), exist_ok=True)

    # One stub-stripped view of every series, computed once — charts and tables all read this.
    pts = {k: p for k, p in ((k, real_points(s)) for k, s in series.items()) if p}
    macro = sorted(c for l, c in pts if l == "macro")
    micro = sorted(c for l, c in pts if l == "micro")
    last = order[-1]
    first_day, last_day = order[0]["date"][:10], last["date"][:10]
    n_points = sum(len(s) for s in series.values())
    charts_written = []

    def table(rows, col="Case"):
        """A delta table; rows are (display_name, layer, case) triples."""
        head = [f"| {col} | Latest Ir | vs prev | vs first | since |",
                "|---|---:|---:|---:|---|"]
        return head + [series_row(nm, pts[(layer, c)], order) for nm, layer, c in rows]

    if write_chart(
        outdir, "macro", order,
        [(c, pts[("macro", c)]) for c in macro],
        "Whole-instrument render cost",
        f"render_block of 1 s of audio - callgrind instructions (Ir), per {label} commit, "
        + f"{first_day} to {last_day}",
        1e6, "Ir (M)",
    ):
        charts_written.append("macro")

    # Per-node overhead: the dedicated case, with the proxy charted alongside for the history
    # recorded before it landed (their absolute levels differ — never stitched into one line).
    over_series = []
    if ("micro", OVERHEAD_CASE) in pts:
        over_series.append((OVERHEAD_CASE, pts[("micro", OVERHEAD_CASE)]))
    if ("micro", OVERHEAD_FALLBACK) in pts:
        over_series.append((f"proxy ({OVERHEAD_FALLBACK})", pts[("micro", OVERHEAD_FALLBACK)]))
    dedicated = over_series and over_series[0][0] == OVERHEAD_CASE
    if dedicated:
        overhead_title = "Per-node engine overhead"
        overhead_latest = over_series[0][1][-1][1]
        overhead_explain = (
            f"`{OVERHEAD_CASE}` is a bench-only no-op operator behind a typical port shape, so "
            + "its entire cost is the engine's per-node stepping overhead (edge clear, routing, "
            + "materialize, `Io` build — see `bench_support.rs`)."
        )
        if len(over_series) > 1:
            overhead_explain += (
                f" The `proxy ({OVERHEAD_FALLBACK})` line is the cheapest value-rate case — "
                + "~99% the same overhead — covering history from before the dedicated case "
                + "landed; its level differs (a smaller port surface), so the two are separate "
                + "lines, never stitched."
            )
    elif over_series:
        overhead_title = "Per-node engine overhead (proxy)"
        overhead_latest = over_series[0][1][-1][1]
        overhead_explain = (
            f"`{OVERHEAD_FALLBACK}` does almost no work of its own, so its cost is ~all engine "
            + "per-node stepping overhead (edge clear, routing, materialize, `Io` build — see "
            + "`bench_support.rs`). A dedicated `overhead` case takes over once it records."
        )
    if over_series and write_chart(
        outdir, "overhead", order, over_series,
        overhead_title,
        "callgrind instructions of a case that is ~pure engine stepping cost, gated like any "
        + "operator. Axis zoomed.",
        1e3, "Ir (k)", zero_base=False,
    ):
        charts_written.append("overhead")

    heavy = sorted(sorted(micro, key=lambda c: pts[("micro", c)][-1][1], reverse=True)[:6])
    if write_chart(
        outdir, "micro-heavy", order,
        [(c, pts[("micro", c)]) for c in heavy],
        "Heaviest operators (micro)",
        "per-operator step_node cost over the same 1 s schedule - top 6 by latest Ir",
        1e6, "Ir (M)",
    ):
        charts_written.append("micro-heavy")

    lines = [
        f"# reuben bench history{'' if label == 'main' else f' — {label}'}",
        "",
        "Deterministic CI performance trend: callgrind **instruction counts (Ir)** "
        + "for rendering **1 s of audio** (375 × 128-frame blocks @ 48 kHz), recorded on every "
        + f"direct push to `{label}`. Instruction counts don't jitter — every visible move is a real "
        + "code change (or a toolchain bump).",
        "",
        f"**{len(order)} commits** · {first_day} → {last_day} · {n_points} data points · "
        + f"last: `{last['sha']}` ({last['date']})",
        "",
        f"*Companion trend: the **{other_label}** series lives on the "
        + f"[`{other_branch}`](https://github.com/{REPO}/tree/{other_branch}) branch.*",
        "",
        "*This page is regenerated by CI on every append — see "
        + "`.github/scripts/bench-dashboard.py` on `main`. Raw series: "
        + "[`bench-history.jsonl`](./bench-history.jsonl).*",
        "",
        "## Whole-instrument render (macro)",
        "",
    ]
    if macro:
        if "macro" in charts_written:
            lines += [picture("macro", "Line chart of render_block instruction counts per "
                                       + f"instrument across {label} commits"), ""]
        lines += table([(c, "macro", c) for c in macro], col="Instrument")
    else:
        lines.append("_No macro data recorded yet._")
    lines += ["", f"## {overhead_title}" if over_series else "## Per-node engine overhead", ""]
    if over_series:
        if "overhead" in charts_written:
            lines += [picture("overhead", "Line chart of per-node engine overhead across "
                                          + f"{label} commits"), ""]
        lines += [
            f"{overhead_explain} Latest: **{fmt_ir(overhead_latest)} Ir** ≈ "
            + f"**{overhead_latest / BLOCKS:,.0f} instructions per node per block**. This "
            + "overhead is a constant offset on every micro case and scales with node count in "
            + "an instrument.",
        ]
    else:
        lines.append("_No overhead data recorded yet._")
    lines += ["", "## Heaviest operators (micro)", ""]
    if micro:
        if "micro-heavy" in charts_written:
            lines += [picture("micro-heavy", "Line chart of the six heaviest per-operator "
                                             + "micro benchmarks"), ""]
        lines += [
            "## All cases",
            "",
            "<details><summary>Full table — every benched case, latest vs previous and first "
            + "recording</summary>",
            "",
        ]
        micro_by_cost = sorted(micro, key=lambda c: -pts[("micro", c)][-1][1])
        lines += table([(f"macro/{c}", "macro", c) for c in macro]
                       + [(c, "micro", c) for c in micro_by_cost])
        lines += ["", "</details>"]
    else:
        lines.append("_No micro data recorded yet._")
    lines += [
        "",
        "## Reading notes",
        "",
        "- **Bold** deltas exceed the perf gate's 3% warn line.",
        "- Micro cases measure `step_node` — operator DSP **plus** the constant per-node engine "
        + "overhead above. Cheap (value-rate) cases are therefore dominated by that overhead: a "
        + "uniform absolute shift across all of them is an engine-overhead change, not operator "
        + "regressions.",
        "- A series that starts mid-chart is an operator that landed after recording began; its "
        + "*vs first* compares against its own first real measurement (registration stubs "
        + f"< {STUB_FLOOR} Ir are dropped).",
        "- Gaps are honest: a commit whose bench harness didn't compile against its baseline "
        + "records nothing.",
        "- Ir is not wall-clock. Counts shift when the pinned toolchain or target baseline "
        + "changes (e.g. the x86-64-v3 bump on 2026-06-29) — those steps are real cost changes "
        + "on the same workload, but not source-code regressions/wins.",
        "",
    ]
    with open(os.path.join(outdir, "README.md"), "w", encoding="utf-8") as f:
        f.write("\n".join(lines))
    print(f"bench-dashboard: rendered README.md + {2 * len(charts_written)} chart files "
          + f"({', '.join(charts_written) or 'none'}) for {len(order)} commits into {outdir}")


if __name__ == "__main__":
    main()
