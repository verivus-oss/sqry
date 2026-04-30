#!/usr/bin/env python3
"""Render cargo-mutants outcomes.json into a Markdown summary for
GITHUB_STEP_SUMMARY.

Wired by `.github/workflows/mutation-gate.yml`. Strategy reference:
/srv/repos/Verivus_Test_Strategy.md §2.4, §7.1 #7, §7.4 #4.

Output is two sections:

  1. Score line (caught / total / pct), computed against in-scope
     mutants only.
  2. Survivor table (file, line, mutation), capped at 50 rows. Full
     list is in the uploaded `mutants.out/` artifact.

Exit code is always 0 — the workflow decides whether survivors gate
the build via `continue-on-error`. This script is a renderer, not a
gate.
"""

from __future__ import annotations

import json
import sys
from collections import Counter
from pathlib import Path

MAX_TABLE_ROWS = 50


def load_outcomes(path: Path) -> list[dict]:
    """Read outcomes.json. cargo-mutants emits one JSON object per
    line in some versions and a JSON array in others; handle both.
    """
    text = path.read_text(encoding="utf-8").strip()
    if not text:
        return []
    if text.startswith("["):
        data = json.loads(text)
        return data if isinstance(data, list) else []
    outcomes = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        outcomes.append(json.loads(line))
    return outcomes


def summarise(outcomes: list[dict]) -> str:
    if not outcomes:
        return "## Mutation gate\n\n_No mutants in scope (no Rust changes touching mutable code, or empty diff)._\n"

    counter: Counter[str] = Counter()
    survivors: list[dict] = []
    for o in outcomes:
        # cargo-mutants outcome strings: "Caught", "Missed",
        # "Timeout", "Unviable", "Success" (the unmutated baseline).
        outcome = o.get("summary") or o.get("outcome") or "Unknown"
        counter[outcome] += 1
        if outcome == "Missed":
            survivors.append(o)

    caught = counter.get("Caught", 0)
    missed = counter.get("Missed", 0)
    timeout = counter.get("Timeout", 0)
    unviable = counter.get("Unviable", 0)
    total_scored = caught + missed + timeout
    pct = (caught / total_scored * 100.0) if total_scored else 0.0

    lines: list[str] = []
    lines.append("## Mutation gate")
    lines.append("")
    lines.append(
        f"**Score**: {caught}/{total_scored} caught ({pct:.1f} %) "
        f"— missed: {missed}, timeout: {timeout}, unviable: {unviable}"
    )
    lines.append("")
    lines.append(
        "_Phase A warm-up: this is a non-blocking ratchet. See "
        "`docs/reviews/test-strategy-audit-2026-04-30/CATALOGUE.md` "
        "P0 #1 for promotion criteria._"
    )
    lines.append("")

    if survivors:
        lines.append(f"### Surviving mutants ({len(survivors)})")
        lines.append("")
        lines.append("| File | Line | Mutation |")
        lines.append("| --- | ---: | --- |")
        for s in survivors[:MAX_TABLE_ROWS]:
            scenario = s.get("scenario", {})
            mutant = scenario.get("Mutant") or s.get("mutant") or {}
            file_ = mutant.get("source_file") or s.get("source_file") or "?"
            line = mutant.get("line") or s.get("line") or "?"
            description = (
                mutant.get("function", {}).get("name")
                or mutant.get("genre")
                or s.get("diff")
                or "?"
            )
            description_short = str(description).split("\n", 1)[0][:120]
            lines.append(
                f"| `{file_}` | {line} | `{description_short}` |"
            )
        if len(survivors) > MAX_TABLE_ROWS:
            lines.append("")
            lines.append(
                f"_… {len(survivors) - MAX_TABLE_ROWS} more survivors "
                "elided — see `mutants.out/` artifact._"
            )
        lines.append("")

    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: mutants_summary.py <outcomes.json>", file=sys.stderr)
        return 2
    path = Path(argv[1])
    if not path.exists():
        print("## Mutation gate\n\n_outcomes.json not produced — see job logs._\n")
        return 0
    print(summarise(load_outcomes(path)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
