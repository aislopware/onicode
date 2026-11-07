#!/usr/bin/env python3
"""Report what moved upstream of onicode, for the session that adopts it.

    watch.py round            one round: print a line per change since the last round
    watch.py loop [-i SEC]    rounds forever (run it under a Monitor; default every 300 s)
    watch.py status [NAME]    per source: commits past `base` that touch its paths, and their files
    watch.py deps             toolchain and crate updates (rustup, cargo upgrade --incompatible)

Sources and their `base` are in sources.toml beside this file: what onicode builds on, and the
references it studies (peers that drive coding agents, Claude Code and its SDKs). References may
also report GitHub releases (`releases`), RSS items (`feed`), release-notes headings (`page`) or
npm versions (`npm`), whose published artifacts are studied with REA (docs/onicode/upstream.md).
A head move is named only when the commits since the last look touch the source's `paths`, a pull
request only when its files do. A source with `branches = true` also reports its other branches (work
pushed before any pull request) when one appears or moves and its diff from the main branch touches
`paths`. A source with a `fork` also reports how many upstream commits the
fork's `main` lacks, whenever that number changes, so a fork never drifts behind unnoticed. Nothing
is applied: changes are read and ported by intent (docs/onicode/upstream.md), and `base` moves in
the adopting commit. What a round saw is kept in
~/.cache/onicode/upstream-watch.json, so a restart does not announce it again; the first round
for a source only records. An upstream that does not answer is skipped for the round.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import subprocess
import sys
import time
import tomllib
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
STATE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "onicode" / "upstream-watch.json"
PULLS = 20


def sources() -> dict[str, dict]:
    """Load sources.toml."""
    with (HERE / "sources.toml").open("rb") as f:
        return tomllib.load(f)


def run(*args: str, timeout: int = 60, ok: tuple[int, ...] = (0,)) -> str | None:
    """Run a command and return its stdout, or None when it exits outside `ok`, times out or is missing."""
    try:
        out = subprocess.run(args, capture_output=True, text=True, timeout=timeout, check=False)
    except (subprocess.TimeoutExpired, FileNotFoundError):
        return None
    return out.stdout if out.returncode in ok else None


def gh_json(*args: str) -> object:
    """Run `gh` and parse its JSON output; None when it fails."""
    out = run("gh", *args)
    return None if out is None else json.loads(out)


def head(src: dict) -> str | None:
    """Return the commit at the tip of the source's branch."""
    out = run("git", "ls-remote", f"https://github.com/{src['upstream']}", f"refs/heads/{src['branch']}")
    return out.split()[0] if out else None


def relevant(files: list[str], paths: list[str]) -> list[str]:
    """Keep the files under `paths` (all of them when `paths` is empty)."""
    return [f for f in files if not paths or any(f.startswith(p) for p in paths)]


def compare(src: dict, old: str, new: str) -> tuple[list[dict], list[str]] | None:
    """Return the commits and touched files from `old` to `new` (GitHub caps at 250 commits, 300 files)."""
    data = gh_json("api", f"repos/{src['upstream']}/compare/{old}...{new}")
    if data is None:
        return None
    files = [f["filename"] for f in data.get("files", [])]
    return data.get("commits", []), files


def pulls(src: dict) -> dict[str, dict] | None:
    """Return the recent pull requests whose files touch the source's paths, by number."""
    if src.get("pulls", True) is False:
        return {}
    data = gh_json(
        "pr",
        "list",
        "-R",
        src["upstream"],
        "--state",
        "all",
        "--limit",
        str(PULLS),
        "--json",
        "number,state,updatedAt,title,files",
    )
    if data is None:
        return None
    paths = src.get("paths", [])
    return {
        str(p["number"]): {"state": p["state"], "updated": p["updatedAt"], "title": p["title"]}
        for p in data
        if relevant([f["path"] for f in p.get("files") or []], paths)
    }


def branch_heads(src: dict) -> dict[str, str] | None:
    """Return the tip of every branch but the main one, by name, when the source watches branches."""
    if not src.get("branches"):
        return {}
    out = run("git", "ls-remote", "--heads", f"https://github.com/{src['upstream']}")
    if out is None:
        return None
    heads = {}
    for line in out.splitlines():
        sha, ref = line.split()
        name = ref.removeprefix("refs/heads/")
        if name != src["branch"]:
            heads[name] = sha
    return heads


def branch_lines(name: str, src: dict, seen: dict[str, str], heads: dict[str, str]) -> list[str]:
    """Report branches that appeared or moved since the last round and touch the source's paths."""
    lines = []
    for branch, sha in sorted(heads.items()):
        if seen.get(branch) == sha:
            continue
        diff = compare(src, src["branch"], sha)
        if diff is None:
            continue
        commits, files = diff
        hits = relevant(files, src.get("paths", []))
        if not hits:
            continue
        what = "moved" if branch in seen else "appeared"
        subject = commits[-1]["commit"]["message"].splitlines()[0] if commits else ""
        url = f"https://github.com/{src['upstream']}/compare/{src['branch']}...{branch}"
        lines.append(
            f"{name}: branch {branch} {what}, {len(commits)} commits, {len(hits)} relevant files: {subject} ({url})"
        )
    return lines


def releases(src: dict) -> list[str] | None:
    """Return the source's latest release tags, when it reports releases."""
    if not src.get("releases"):
        return []
    data = gh_json("release", "list", "-R", src["upstream"], "--limit", "10", "--json", "tagName")
    return None if data is None else [r["tagName"] for r in data]


def fetch(url: str) -> str | None:
    """Fetch a page; None when it does not answer."""
    # S310: the URLs are the https feeds and pages listed in sources.toml.
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (onicode upstream watch)"})  # noqa: S310
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:  # noqa: S310
            return resp.read().decode(errors="replace")
    except OSError:
        return None


def npm_versions(package: str) -> list[str] | None:
    """List the package's published versions, newest first (the registry lists them oldest first)."""
    text = fetch(f"https://registry.npmjs.org/{package}")
    if text is None:
        return None
    times = json.loads(text).get("time", {})
    return [
        v
        for v, _ in sorted(
            ((v, t) for v, t in times.items() if v not in ("created", "modified")), key=lambda vt: vt[1], reverse=True
        )
    ][:20]


def entries(src: dict) -> list[str] | None:
    """List the titles of a feed (`feed`, RSS or Atom) or a release-notes page (`page`, its h2/h3), newest first."""
    if "npm" in src:
        return npm_versions(src["npm"])
    text = fetch(src.get("feed") or src["page"])
    if text is None:
        return None
    pattern = (
        r"<item>.*?<title>(.*?)</title>|<entry>.*?<title[^>]*>(.*?)</title>"
        if "feed" in src
        else r"<h[23][^>]*>(.*?)</h[23]>"
    )
    found = []
    for match in re.finditer(pattern, text, re.DOTALL):
        raw = next(g for g in match.groups() if g is not None)
        title = html.unescape(re.sub(r"<!\[CDATA\[|\]\]>|<[^>]+>", "", raw)).strip()
        if title and title not in found:
            found.append(title)
    return found


def watch_entries(name: str, src: dict, seen: dict, now: float) -> list[str]:
    """Report the entries not seen last round."""
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
    """Load what earlier rounds saw."""
    try:
        return json.loads(STATE.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


def save_state(state: dict) -> None:
    """Write the state atomically."""
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=1, sort_keys=True))
    tmp.replace(STATE)


def head_lines(name: str, src: dict, old: str | None, new: str) -> list[str]:
    """Report a head move whose commits touch the source's paths."""
    if not old or old == new:
        return []
    diff = compare(src, old, new)
    if diff is None:
        return []
    commits, files = diff
    hits = relevant(files, src.get("paths", []))
    if not hits:
        return []
    subjects = "; ".join(c["commit"]["message"].splitlines()[0] for c in commits[-3:])
    return [f"{name}: head {old[:9]}..{new[:9]}, {len(commits)} commits, {len(hits)} relevant files: {subjects}"]


def pull_lines(name: str, src: dict, seen_pulls: dict[str, dict], prs: dict[str, dict]) -> list[str]:
    """Report pull requests opened, merged, closed, reopened or updated since last round."""
    lines = []
    for number, pr in prs.items():
        before = seen_pulls.get(number)
        if before is None:
            what = "opened" if pr["state"] == "OPEN" else pr["state"].lower()
        elif before["state"] != pr["state"]:
            what = {"MERGED": "merged", "CLOSED": "closed", "OPEN": "reopened"}[pr["state"]]
        elif before["updated"] != pr["updated"] and pr["state"] == "OPEN":
            what = "updated"
        else:
            continue
        lines.append(f"{name}: #{number} {what}: {pr['title']} (https://github.com/{src['upstream']}/pull/{number})")
    return lines


def fork_lag(src: dict) -> int | None:
    """Count the upstream commits missing from the fork's `main`; None when GitHub does not answer."""
    owner, repo = src["upstream"].split("/")
    data = gh_json("api", f"repos/{src['fork']}/compare/main...{owner}:{repo}:{src['branch']}")
    return None if data is None else int(data.get("ahead_by", 0))


def fork_lines(name: str, src: dict, seen: dict) -> list[str]:
    """Report the fork's lag behind upstream when it changed since the last look."""
    if "fork" not in src:
        return []
    lag = fork_lag(src)
    if lag is None or lag == seen.get("fork_lag"):
        return []
    seen["fork_lag"] = lag
    if lag == 0:
        return [f"{name}: fork {src['fork']} main has every upstream commit"]
    return [f"{name}: fork {src['fork']} main lacks {lag} upstream commits: merge them into the fork's main"]


def one_round(state: dict, now: float) -> list[str]:
    """Look at every source that is due and report what moved; updates `state`."""
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
        heads = branch_heads(src)
        if new is None or prs is None or tags is None or heads is None:
            continue
        if "releases" in seen:
            lines += [
                f"{name}: release {tag} (https://github.com/{src['upstream']}/releases/tag/{tag})"
                for tag in tags
                if tag not in seen["releases"]
            ]
        lines += head_lines(name, src, seen.get("head"), new)
        lines += fork_lines(name, src, seen)
        if "pulls" in seen:
            lines += pull_lines(name, src, seen["pulls"], prs)
        if "branches" in seen:
            lines += branch_lines(name, src, seen["branches"], heads)
        seen.update(head=new, pulls=prs, releases=tags, branches=heads, at=now)
    return lines


def cmd_round(_: argparse.Namespace) -> int:
    """Run one round and print its lines."""
    state = load_state()
    lines = one_round(state, time.time())
    save_state(state)
    for line in lines:
        print(line, flush=True)
    return 0


def cmd_loop(args: argparse.Namespace) -> int:
    """Run rounds forever."""
    while True:
        cmd_round(args)
        time.sleep(args.interval)


def print_past_base(name: str, src: dict, new: str, *, verbose: bool) -> None:
    """Print how far upstream's head `new` is past the source's base, commit by commit when verbose."""
    if new == src["base"]:
        print(f"{name}: current at {new[:9]}")
        return
    diff = compare(src, src["base"], new)
    if diff is None:
        print(f"{name}: {src['base'][:9]}..{new[:9]} (compare failed)")
        return
    commits, files = diff
    hits = relevant(files, src.get("paths", []))
    print(
        f"{name}: {len(commits)} commits past base {src['base'][:9]} ({src.get('base_date', '?')}), "
        f"{len(hits)}/{len(files)} files relevant, head {new[:9]}"
    )
    if verbose:
        for c in commits:
            print(f"  {c['sha'][:9]} {c['commit']['message'].splitlines()[0]}")
        for f in hits:
            print(f"    {f}")


def cmd_status(args: argparse.Namespace) -> int:
    """Print how far each source is past its base."""
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
        if "fork" in src:
            lag = fork_lag(src)
            print(f"{name}: fork main lacks {'?' if lag is None else lag} upstream commits")
        print_past_base(name, src, new, verbose=bool(args.name))
    return 0


def cmd_deps(_: argparse.Namespace) -> int:
    """Print toolchain and crate updates."""
    # rustup check exits 100 when an update is available.
    print((run("rustup", "check", timeout=120, ok=(0, 100)) or "rustup check failed\n").strip())
    out = run(
        "cargo",
        "upgrade",
        "--dry-run",
        "--incompatible",
        "allow",
        "--pinned",
        "allow",
        "--manifest-path",
        str(ROOT / "Cargo.toml"),
        timeout=600,
    )
    print(out.strip() if out else "cargo upgrade --dry-run failed")
    return 0


def main() -> int:
    """Dispatch the subcommand."""
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
