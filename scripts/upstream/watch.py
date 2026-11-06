#!/usr/bin/env python3
"""Report what moved upstream of onicode, for the session that adopts it.

    watch.py round            one round: print a line per change since the last round
    watch.py loop [-i SEC]    rounds forever (run it under a Monitor; default every 300 s)
    watch.py status [NAME]    per source: commits past `base` that touch its paths, and their files
    watch.py deps             toolchain and crate updates (rustup, cargo upgrade --incompatible)

Sources and their `base` are in sources.toml beside this file. A head move is named only when
the commits since the last look touch the source's `paths`, a pull request only when its files
do. Nothing is applied: changes are read and ported by intent (docs/onicode/upstream.md), and
`base` moves in the adopting commit. What a round saw is kept in
~/.cache/onicode/upstream-watch.json, so a restart does not announce it again; the first round
for a source only records. An upstream that does not answer is skipped for the round.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
STATE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "onicode" / "upstream-watch.json"
PULLS = 20


def sources() -> dict[str, dict]:
    with open(HERE / "sources.toml", "rb") as f:
        return tomllib.load(f)


def run(*args: str, timeout: int = 60) -> str | None:
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=timeout, check=True)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        return None
    return out.stdout


def gh_json(*args: str):
    out = run("gh", *args)
    return None if out is None else json.loads(out)


def head(src: dict) -> str | None:
    out = run("git", "ls-remote", f"https://github.com/{src['upstream']}", f"refs/heads/{src['branch']}")
    return out.split()[0] if out else None


def relevant(files: list[str], paths: list[str]) -> list[str]:
    return [f for f in files if not paths or any(f.startswith(p) for p in paths)]


def compare(src: dict, old: str, new: str) -> tuple[list[dict], list[str]] | None:
    """Commits and touched files from `old` to `new` (GitHub caps at 250 commits, 300 files)."""
    data = gh_json("api", f"repos/{src['upstream']}/compare/{old}...{new}")
    if data is None:
        return None
    files = [f["filename"] for f in data.get("files", [])]
    return data.get("commits", []), files


def pulls(src: dict) -> dict[str, dict] | None:
    if src.get("pulls", True) is False:
        return {}
    data = gh_json("pr", "list", "-R", src["upstream"], "--state", "all", "--limit", str(PULLS),
                   "--json", "number,state,updatedAt,title,files")
    if data is None:
        return None
    paths = src.get("paths", [])
    return {str(p["number"]): {"state": p["state"], "updated": p["updatedAt"], "title": p["title"]}
            for p in data if relevant([f["path"] for f in p.get("files") or []], paths)}


def load_state() -> dict:
    try:
        return json.loads(STATE.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


def save_state(state: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=1, sort_keys=True))
    tmp.replace(STATE)


def one_round(state: dict, now: float) -> list[str]:
    lines = []
    for name, src in sources().items():
        seen = state.setdefault(name, {})
        every = src.get("every_minutes", 0) * 60
        if every and now - seen.get("at", 0) < every:
            continue
        new = head(src)
        prs = pulls(src)
        if new is None or prs is None:
            continue
        old = seen.get("head")
        if old and old != new:
            diff = compare(src, old, new)
            if diff is not None:
                commits, files = diff
                hits = relevant(files, src.get("paths", []))
                if hits:
                    subjects = "; ".join(c["commit"]["message"].splitlines()[0] for c in commits[-3:])
                    lines.append(f"{name}: head {old[:9]}..{new[:9]}, {len(commits)} commits, "
                                 f"{len(hits)} relevant files: {subjects}")
        if "pulls" in seen:
            for number, pr in prs.items():
                before = seen["pulls"].get(number)
                if before is None:
                    what = "opened" if pr["state"] == "OPEN" else pr["state"].lower()
                elif before["state"] != pr["state"]:
                    what = {"MERGED": "merged", "CLOSED": "closed", "OPEN": "reopened"}[pr["state"]]
                elif before["updated"] != pr["updated"] and pr["state"] == "OPEN":
                    what = "updated"
                else:
                    continue
                lines.append(f"{name}: #{number} {what}: {pr['title']} "
                             f"(https://github.com/{src['upstream']}/pull/{number})")
        seen.update(head=new, pulls=prs, at=now)
    return lines


def cmd_round(_: argparse.Namespace) -> int:
    state = load_state()
    lines = one_round(state, time.time())
    save_state(state)
    for line in lines:
        print(line, flush=True)
    return 0


def cmd_loop(args: argparse.Namespace) -> int:
    while True:
        cmd_round(args)
        time.sleep(args.interval)


def cmd_status(args: argparse.Namespace) -> int:
    for name, src in sources().items():
        if args.name and name != args.name:
            continue
        new = head(src)
        if new is None:
            print(f"{name}: upstream did not answer")
            continue
        if new == src["base"]:
            print(f"{name}: current at {new[:9]}")
            continue
        diff = compare(src, src["base"], new)
        if diff is None:
            print(f"{name}: {src['base'][:9]}..{new[:9]} (compare failed)")
            continue
        commits, files = diff
        hits = relevant(files, src.get("paths", []))
        print(f"{name}: {len(commits)} commits past base {src['base'][:9]} ({src.get('base_date', '?')}), "
              f"{len(hits)}/{len(files)} files relevant, head {new[:9]}")
        if args.name:
            for c in commits:
                print(f"  {c['sha'][:9]} {c['commit']['message'].splitlines()[0]}")
            for f in hits:
                print(f"    {f}")
    return 0


def cmd_deps(_: argparse.Namespace) -> int:
    print((run("rustup", "check", timeout=120) or "rustup check failed\n").strip())
    out = run("cargo", "upgrade", "--dry-run", "--incompatible", "allow", "--pinned", "allow",
              "--manifest-path", str(ROOT / "Cargo.toml"), timeout=600)
    print(out.strip() if out else "cargo upgrade --dry-run failed")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("round").set_defaults(func=cmd_round)
    loop = sub.add_parser("loop")
    loop.add_argument("-i", "--interval", type=int, default=300)
    loop.set_defaults(func=cmd_loop)
    status = sub.add_parser("status")
    status.add_argument("name", nargs="?")
    status.set_defaults(func=cmd_status)
    sub.add_parser("deps").set_defaults(func=cmd_deps)
    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
