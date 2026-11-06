#!/usr/bin/env python3
"""Report what moved upstream of onicode, for the session that adopts it.

    watch.py round            one round: print a line per change since the last round
    watch.py loop [-i SEC]    rounds forever (run it under a Monitor; default every 300 s)
    watch.py status [NAME]    per source: commits past `base` that touch its paths, and their files
    watch.py deps             toolchain and crate updates (rustup, cargo upgrade --incompatible)

Sources and their `base` are in sources.toml beside this file: what onicode builds on, and the
references it studies (peers that drive coding agents, Claude Code and its SDKs). References may
also report GitHub releases (`releases`), RSS items (`feed`), release-notes headings (`page`) or
npm versions (`npm`), whose published artifacts are studied with REA (docs/onicode/upstream.md). A head move is named only when
the commits since the last look touch the source's `paths`, a pull request only when its files
do. Nothing is applied: changes are read and ported by intent (docs/onicode/upstream.md), and
`base` moves in the adopting commit. What a round saw is kept in
~/.cache/onicode/upstream-watch.json, so a restart does not announce it again; the first round
for a source only records. An upstream that does not answer is skipped for the round.
"""

from __future__ import annotations

import argparse
import html
import re
import urllib.request
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


def releases(src: dict) -> list[str] | None:
    if not src.get("releases"):
        return []
    data = gh_json("release", "list", "-R", src["upstream"], "--limit", "10", "--json", "tagName")
    return None if data is None else [r["tagName"] for r in data]


def fetch(url: str) -> str | None:
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (onicode upstream watch)"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.read().decode(errors="replace")
    except OSError:
        return None


def npm_versions(package: str) -> list[str] | None:
    """The package's published versions, newest first (the registry lists them oldest first)."""
    text = fetch(f"https://registry.npmjs.org/{package}")
    if text is None:
        return None
    times = json.loads(text).get("time", {})
    return [v for v, _ in sorted(((v, t) for v, t in times.items() if v not in ("created", "modified")),
                                 key=lambda vt: vt[1], reverse=True)][:20]


def entries(src: dict) -> list[str] | None:
    """The titles a feed (`feed`, RSS or Atom) or a release-notes page (`page`, its h2/h3) lists, newest first."""
    if "npm" in src:
        return npm_versions(src["npm"])
    text = fetch(src.get("feed") or src["page"])
    if text is None:
        return None
    pattern = r"<item>.*?<title>(.*?)</title>|<entry>.*?<title[^>]*>(.*?)</title>" if "feed" in src \
        else r"<h[23][^>]*>(.*?)</h[23]>"
    found = []
    for match in re.finditer(pattern, text, re.S):
        raw = next(g for g in match.groups() if g is not None)
        title = html.unescape(re.sub(r"<!\[CDATA\[|\]\]>|<[^>]+>", "", raw)).strip()
        if title and title not in found:
            found.append(title)
    return found


def watch_entries(name: str, src: dict, seen: dict, now: float) -> list[str]:
    current = entries(src)
    if current is None:
        return []
    lines = []
    if "entries" in seen:
        url = src.get("feed") or src.get("page") or f"https://www.npmjs.com/package/{src.get('npm')}"
        lines = [f"{name}: new: {title} ({url})" for title in current if title not in seen["entries"]]
    seen.update(entries=current, at=now)
    return lines


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
        if "feed" in src or "page" in src or "npm" in src:
            lines += watch_entries(name, src, seen, now)
            continue
        new = head(src)
        prs = pulls(src)
        tags = releases(src)
        if new is None or prs is None or tags is None:
            continue
        if "releases" in seen:
            lines += [f"{name}: release {tag} (https://github.com/{src['upstream']}/releases/tag/{tag})"
                      for tag in tags if tag not in seen["releases"]]
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
        seen.update(head=new, pulls=prs, releases=tags, at=now)
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
        if "feed" in src or "page" in src or "npm" in src:
            current = entries(src) or []
            print(f"{name}: latest: {current[0] if current else '(no answer)'}")
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
