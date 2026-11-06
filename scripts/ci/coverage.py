#!/usr/bin/env python3
"""The coverage ratchet. The line coverage measured on macOS may not fall below the committed baseline.

    coverage.py check SUMMARY   fail when cargo-llvm-cov's JSON summary is below the baseline
    coverage.py rises BASE_REF  fail when the committed baseline is lower than at BASE_REF

The baseline is scripts/ci/coverage-baseline.txt: one number, the percentage of lines covered. It only goes
up: when a nightly run measures more, raise it in a commit.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
BASELINE = Path("scripts/ci/coverage-baseline.txt")
# Measurements wobble by a few hundredths as tests change; ask for a raise only past this.
RAISE_STEP = 0.1


def parse(text: str) -> float:
    """Read a baseline file's number."""
    return float(text.strip())


def check(summary: Path) -> int:
    """Compare a cargo-llvm-cov JSON summary with the baseline."""
    measured = float(json.loads(summary.read_text())["data"][0]["totals"]["lines"]["percent"])
    baseline = parse((ROOT / BASELINE).read_text())
    print(f"line coverage {measured:.2f}% (baseline {baseline:.2f}%)")
    if measured < baseline:
        print(f"coverage.py: line coverage fell below the baseline in {BASELINE}", file=sys.stderr)
        return 1
    if measured >= baseline + RAISE_STEP:
        print(f"coverage.py: raise {BASELINE} to {measured:.2f}")
    return 0


def rises(base_ref: str) -> int:
    """Compare the committed baseline with the one at BASE_REF."""
    shown = subprocess.run(
        ["git", "show", f"{base_ref}:{BASELINE.as_posix()}"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if shown.returncode != 0:
        return 0
    before = parse(shown.stdout)
    now = parse((ROOT / BASELINE).read_text())
    if now < before:
        print(f"coverage.py: {BASELINE} went down from {before:.2f} to {now:.2f}; it only goes up", file=sys.stderr)
        return 1
    return 0


def main(argv: list[str]) -> int:
    """Dispatch a subcommand."""
    usage = "usage: coverage.py check SUMMARY | coverage.py rises BASE_REF"
    commands = {"check": lambda arg: check(Path(arg)), "rises": rises}
    command, *args = argv or [""]
    if command not in commands or len(args) != 1:
        print(usage, file=sys.stderr)
        return 2
    return commands[command](args[0])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
