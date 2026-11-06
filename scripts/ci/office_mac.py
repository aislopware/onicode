#!/usr/bin/env python3
"""Run onicode's macOS and iOS lanes on the office Macs and report them to GitHub as commit statuses.

    office_mac.py serve              poll GitHub and run the lanes (the launch agent)
    office_mac.py run SHA LANE       run one lane for one commit now and print its log tail
    office_mac.py install            install and start the launch agent on this Mac

GitLab (gitlab.jmango360.com) refuses authenticated calls from outside the office network, so GitHub's
hosted runners cannot start these jobs. This poller runs on a Mac inside the network instead: it watches
main and the pull requests opened from aislopware/onicode itself (never forks), triggers the GitLab
pipeline (scripts/ci/office-mac.gitlab-ci.yml) for each new head, and posts `office-mac/<lane>` statuses
back. Nothing from GitHub runs on this Mac: the code runs only in the tart guest on the office runner.

Lanes: `mac` (scripts/ci/mac.sh pr) and `ios` (ios-pr) for each head; `nightly` and `ios-nightly` once a
day for main's head. A lane whose paths the head does not touch reports success at once.

Credentials: the GitHub token is `gh auth token`; the GitLab trigger token and a project access token
(api) sit in the login keychain as `onicode-ci-gitlab-trigger` and `onicode-ci-gitlab-read`.
"""

from __future__ import annotations

import contextlib
import datetime as dt
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Callable

REPO = "aislopware/onicode"
GITHUB = "https://api.github.com"
GITLAB = "https://gitlab.jmango360.com/api/v4/projects/2137"
MACS = ("office-mac-runner-02", "office-mac-runner")
START_SECONDS = 120
WAIT_SECONDS = 4 * 3600
POLL = 15
NIGHTLY_HOUR = 2
DONE = {"success": "success", "failed": "failure", "canceled": "error", "skipped": "error"}
SUITES = {"mac": "pr", "ios": "ios-pr", "nightly": "nightly", "ios-nightly": "ios-nightly"}
# The same split as ci.yml's `changes` job.
DOCS_ONLY = re.compile(r"^(docs/|.*\.md$|LICENSE|\.research/)")
DESKTOP = re.compile(
    r"^crates/(ui|engine|harness|preview|preview-label|cli|launch|notify|update|audio|mcp|rpc)/|^apps/onicode/"
)
IOS = re.compile(r"^(apps/ios/|crates/|Cargo\.(toml|lock)$|rust-toolchain\.toml$|scripts/ios/|scripts/ci/|\.github/)")
STATE = Path.home() / "Library/Application Support/onicode-ci/state.json"
LABEL = "dev.aislopware.onicode-ci"


def log(message: str) -> None:
    """One timestamped line to stdout (the launch agent's log)."""
    print(f"{dt.datetime.now().astimezone().isoformat(timespec='seconds')} {message}", flush=True)


def keychain(service: str) -> str:
    """Read a secret from the login keychain."""
    return subprocess.run(
        ["/usr/bin/security", "find-generic-password", "-a", "onicode-ci", "-s", service, "-w"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def github_token() -> str:
    """Return the signed-in gh account's token, read fresh so a refresh is picked up."""
    return subprocess.run(["gh", "auth", "token"], check=True, capture_output=True, text=True).stdout.strip()


def request(url: str, *, method: str = "GET", headers: dict[str, str], body: bytes | None = None) -> object:
    """Send one HTTPS request and decode its JSON (or return the text when it is not JSON)."""
    req = urllib.request.Request(url, data=body, method=method, headers=headers)  # noqa: S310 - fixed https hosts
    with urllib.request.urlopen(req, timeout=30) as resp:  # noqa: S310
        text = resp.read().decode(errors="replace")
    try:
        return json.loads(text)
    except ValueError:
        return text


def github(path: str, data: dict | None = None) -> object:
    """Call GitHub's REST API for the repository."""
    headers = {"Authorization": f"Bearer {github_token()}", "Accept": "application/vnd.github+json"}
    body = json.dumps(data).encode() if data is not None else None
    return request(f"{GITHUB}/repos/{REPO}{path}", method="POST" if data else "GET", headers=headers, body=body)


def gitlab(path: str, *, method: str = "GET") -> object:
    """Call the onicode-ci project's API with the poller's access token."""
    return request(f"{GITLAB}{path}", method=method, headers={"PRIVATE-TOKEN": keychain("onicode-ci-gitlab-read")})


def trigger(sha: str, suite: str, mac: str) -> dict:
    """Start the pipeline for `sha` on `mac`."""
    form = {
        "token": keychain("onicode-ci-gitlab-trigger"),
        "ref": "main",
        "variables[SHA]": sha,
        "variables[SUITE]": suite,
        "variables[MAC_TAG]": mac,
    }
    body = urllib.parse.urlencode(form).encode()
    return request(f"{GITLAB}/trigger/pipeline", method="POST", headers={}, body=body)


def first_job(pipeline: int) -> dict | None:
    """Return the pipeline's one job, once GitLab has created it."""
    jobs = gitlab(f"/pipelines/{pipeline}/jobs")
    return jobs[0] if jobs else None


def run_lane(sha: str, lane: str, on_start: Callable[[str, str], None] | None = None) -> tuple[str, str, str]:
    """Run one lane on the first office Mac that starts it; return (state, description, url)."""
    suite = SUITES[lane]
    for mac, wait in ((MACS[0], START_SECONDS), (MACS[1], START_SECONDS), (MACS[0], WAIT_SECONDS)):
        pipeline = trigger(sha, suite, mac)
        deadline = time.monotonic() + wait
        job = None
        while time.monotonic() < deadline:
            job = first_job(pipeline["id"])
            if job and job["status"] not in ("created", "pending", "waiting_for_resource"):
                break
            time.sleep(POLL)
        else:
            log(f"{sha[:8]} {lane}: {mac} did not start within {wait} s")
            with contextlib.suppress(OSError):
                gitlab(f"/pipelines/{pipeline['id']}/cancel", method="POST")
            continue
        if on_start:
            on_start(job["web_url"], mac)
        while job["status"] not in DONE:
            time.sleep(POLL)
            job = first_job(pipeline["id"])
        took = f"{(job.get('duration') or 0) / 60:.0f} min on {mac}"
        if job["status"] == "success":
            return "success", f"passed in {took}", job["web_url"]
        return DONE[job["status"]], f"{job['status']}: {first_failure(job['id'])} ({took})", job["web_url"]
    return "error", "no office Mac started the job", ""


def first_failure(job: int) -> str:
    """Return the first job log line that names what failed, for the status description."""
    with contextlib.suppress(OSError, urllib.error.URLError):
        log_text = gitlab(f"/jobs/{job}/trace")
        if isinstance(log_text, str):
            for line in log_text.splitlines():
                clean = re.sub(r"\x1b\[[0-9;]*m", "", line).strip()
                if re.search(r"^(FAIL |error(\[|:)|.*panicked at|\S+ failed)", clean):
                    return clean[:90]
    return "see the job log"


def touched(base: str | None, head: str) -> list[str] | None:
    """Files changed from `base` to `head`, or None when unknown (run every lane)."""
    if not base:
        return None
    with contextlib.suppress(urllib.error.HTTPError):
        compare = github(f"/compare/{base}...{head}")
        if len(compare.get("files", [])) < 300:  # noqa: PLR2004 - GitHub caps the list at 300
            return [f["filename"] for f in compare["files"]]
    return None


def affected(lane: str, files: list[str] | None) -> bool:
    """Whether the lane's paths are touched (always for nightly lanes and unknown diffs)."""
    if files is None or "nightly" in lane:
        return True
    if lane == "mac":
        return any(not DOCS_ONLY.match(f) for f in files)
    return any(IOS.match(f) and not DESKTOP.match(f) for f in files)


def heads() -> list[tuple[str, str | None]]:
    """(head SHA, base SHA) for main and for each open same-repository pull request."""
    main = github("/commits/main")
    parent = main["parents"][0]["sha"] if main["parents"] else None
    # Diff main against its parent only when the parent was tested; a push of several commits runs every lane.
    out = [(main["sha"], parent if parent and has_status(parent, "office-mac/mac") else None)]
    out += [
        (pr["head"]["sha"], pr["base"]["sha"])
        for pr in github("/pulls?state=open&per_page=50")
        if pr["head"]["repo"] and pr["head"]["repo"]["full_name"] == REPO
    ]
    return out


def has_status(sha: str, context: str) -> bool:
    """Whether some poller already claimed this lane for this commit."""
    statuses = github(f"/commits/{sha}/statuses?per_page=100")
    return any(s["context"] == context for s in statuses)


def status(sha: str, lane: str, state: str, description: str, url: str = "") -> None:
    """Post the lane's commit status."""
    data = {"state": state, "context": f"office-mac/{lane}", "description": description[:140]}
    if url:
        data["target_url"] = url
    github(f"/statuses/{sha}", data)
    log(f"{sha[:8]} {lane}: {state} {description}")


def lane_thread(sha: str, lane: str, running: set[str]) -> None:
    """Run one claimed lane and report it; never let an error leave the status pending."""
    try:
        state, description, url = run_lane(
            sha, lane, lambda url, mac: status(sha, lane, "pending", f"running on {mac}", url)
        )
        status(sha, lane, state, description, url)
    except Exception as err:  # noqa: BLE001 - report any failure on the commit rather than die silently
        with contextlib.suppress(Exception):
            status(sha, lane, "error", f"poller: {err}")
    finally:
        running.discard(f"{sha}:{lane}")


def load_state() -> dict:
    """Read the poller's small persistent state (the last nightly date)."""
    with contextlib.suppress(OSError, ValueError):
        return json.loads(STATE.read_text())
    return {}


def serve() -> None:
    """Poll forever: claim each untested head's lanes, run them in parallel, report statuses."""
    running: set[str] = set()
    state = load_state()
    log("office-mac poller started")
    while True:
        try:
            work = []
            for sha, base in heads():
                files = None
                for lane in ("mac", "ios"):
                    key = f"{sha}:{lane}"
                    if key in running or has_status(sha, f"office-mac/{lane}"):
                        continue
                    files = files if files is not None else touched(base, sha)
                    if not affected(lane, files):
                        status(sha, lane, "success", "not affected by this change")
                        continue
                    work.append((sha, lane))
            now = dt.datetime.now().astimezone()
            today = now.date().isoformat()
            if now.hour >= NIGHTLY_HOUR and state.get("nightly") != today:
                main = heads()[0][0]
                work += [(main, "nightly"), (main, "ios-nightly")]
                state["nightly"] = today
                STATE.parent.mkdir(parents=True, exist_ok=True)
                STATE.write_text(json.dumps(state))
            for sha, lane in work:
                status(sha, lane, "pending", "queued for an office Mac")
                running.add(f"{sha}:{lane}")
                threading.Thread(target=lane_thread, args=(sha, lane, running), daemon=True).start()
        except (OSError, urllib.error.URLError, subprocess.CalledProcessError, KeyError, ValueError) as err:
            log(f"poll failed: {err}")
        time.sleep(POLL)


def install() -> None:
    """Install the launch agent that runs `serve` at login and keeps it alive."""
    agent = Path.home() / f"Library/LaunchAgents/{LABEL}.plist"
    logfile = Path.home() / "Library/Logs/onicode-ci.log"
    script = Path(__file__).resolve()
    path = os.environ.get("PATH", "/usr/bin:/bin")
    agent.write_text(f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array>
    <string>/usr/bin/python3</string><string>-I</string><string>{script}</string><string>serve</string>
  </array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{logfile}</string>
  <key>StandardErrorPath</key><string>{logfile}</string>
</dict></plist>
""")
    domain = f"gui/{os.getuid()}"
    subprocess.run(["/bin/launchctl", "bootout", f"{domain}/{LABEL}"], check=False, capture_output=True)
    subprocess.run(["/bin/launchctl", "bootstrap", domain, str(agent)], check=True)
    log(f"installed {agent}; log {logfile}")


def main() -> int:
    """Dispatch the subcommand."""
    args = sys.argv[1:]
    if args == ["serve"]:
        serve()
    elif args == ["install"]:
        install()
    elif len(args) == 3 and args[0] == "run" and args[2] in SUITES:  # noqa: PLR2004 - run SHA LANE
        state, description, url = run_lane(args[1], args[2])
        print(f"{args[2]}: {state} {description} {url}")
        return 0 if state == "success" else 1
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
