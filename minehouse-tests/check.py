#!/usr/bin/env python3
"""Checks a steel-flint summary against known-gaps.txt.

usage: check.py <flint_summary.json> [known-gaps.txt]

Exit code 1 when a spec fails that is not a known gap. Known gaps that pass
are listed so they can be removed. Writes a Markdown summary to
$GITHUB_STEP_SUMMARY when it is set.
"""
import json
import os
import sys
from pathlib import Path


def main() -> int:
    results = json.loads(Path(sys.argv[1]).read_text())
    gaps_file = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(__file__).with_name("known-gaps.txt")
    gaps = {l.strip() for l in gaps_file.read_text().splitlines() if l.strip() and not l.startswith("#")}
    ours = [r for r in results if r["name"].startswith("minehouse_")]
    names = {r["name"] for r in ours}
    regressions = sorted(r["name"] for r in ours if not r["success"] and r["name"] not in gaps)
    fixed = sorted(r["name"] for r in ours if r["success"] and r["name"] in gaps)
    unknown = sorted(gaps - names)
    passed = sum(r["success"] for r in ours)

    lines = [f"**MineHouse parity on SteelMC: {passed}/{len(ours)} pass**", ""]
    lines += ["| spec | result |", "|---|---|"]
    for r in sorted(ours, key=lambda r: r["name"]):
        mark = "✅" if r["success"] else ("🟡 known gap" if r["name"] in gaps else "❌ regression")
        lines.append(f"| `{r['name']}` | {mark} |")
    for title, items in (("Regressions", regressions), ("Known gaps that pass now (remove them)", fixed),
                         ("Listed gaps with no spec", unknown)):
        if items:
            lines += ["", f"**{title}:**", *[f"- {i}" for i in items]]
    text = "\n".join(lines) + "\n"
    print(text)
    if path := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(path, "a") as f:
            f.write(text)
    return 1 if regressions or unknown else 0


if __name__ == "__main__":
    sys.exit(main())
