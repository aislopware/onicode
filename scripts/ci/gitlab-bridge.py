#!/usr/bin/env python3
"""Run scripts/ci/mac.sh for a commit on a GitLab office Mac and mirror its result.

    gitlab-bridge.py SHA SUITE

Triggers the static pipeline of the GitLab project `cong.tran/onicode-ci` (its .gitlab-ci.yml
is scripts/ci/office-mac.gitlab-ci.yml) on the nearly idle office Mac first, then on the other
when the first does not start the job within START_SECONDS. Prints the job log tail and exits
0 (passed), 1 (failed) or 3 (no office Mac free: the caller falls back to a hosted runner).

Environment: GITLAB_TRIGGER_TOKEN (pipeline trigger token) and GITLAB_READ_TOKEN (project
access token, read_api).
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.parse
import urllib.request

API = "https://gitlab.jmango360.com/api/v4"
PROJECT = urllib.parse.quote("cong.tran/onicode-ci", safe="")
MACS = ("office-mac-runner-02", "office-mac-runner")
START_SECONDS = 120
POLL = 10
DONE = {"success": 0, "failed": 1, "canceled": 1, "skipped": 1}


def call(method: str, path: str, data: dict | None = None, raw: bool = False):
    body = urllib.parse.urlencode(data).encode() if data else None
    req = urllib.request.Request(f"{API}/projects/{PROJECT}{path}", data=body, method=method)
    if data is None:
        req.add_header("PRIVATE-TOKEN", os.environ["GITLAB_READ_TOKEN"])
    with urllib.request.urlopen(req, timeout=30) as resp:
        text = resp.read().decode(errors="replace")
    return text if raw else json.loads(text)


def trigger(sha: str, suite: str, mac: str) -> int:
    pipeline = call("POST", "/trigger/pipeline", {
        "token": os.environ["GITLAB_TRIGGER_TOKEN"], "ref": "main",
        "variables[SHA]": sha, "variables[SUITE]": suite, "variables[MAC_TAG]": mac,
    })
    print(f"{mac}: pipeline {pipeline['web_url']}", flush=True)
    return pipeline["id"]


def job(pipeline: int) -> dict | None:
    jobs = call("GET", f"/pipelines/{pipeline}/jobs")
    return jobs[0] if jobs else None


def cancel(pipeline: int) -> None:
    req = urllib.request.Request(f"{API}/projects/{PROJECT}/pipelines/{pipeline}/cancel", method="POST")
    req.add_header("PRIVATE-TOKEN", os.environ["GITLAB_READ_TOKEN"])
    try:
        urllib.request.urlopen(req, timeout=30)
    except OSError:
        pass  # a read-only token cannot cancel; the job is interruptible and short anyway


def main() -> int:
    sha, suite = sys.argv[1], sys.argv[2]
    for mac in MACS:
        pipeline = trigger(sha, suite, mac)
        deadline = time.monotonic() + START_SECONDS
        current = None
        while time.monotonic() < deadline:
            current = job(pipeline)
            if current and current["status"] not in ("created", "pending", "waiting_for_resource"):
                break
            time.sleep(POLL)
        else:
            print(f"{mac}: not started within {START_SECONDS} s", flush=True)
            cancel(pipeline)
            continue
        while current["status"] not in DONE:
            time.sleep(POLL)
            current = job(pipeline)
        log = call("GET", f"/jobs/{current['id']}/trace", raw=True)
        print("\n".join(log.splitlines()[-200:]))
        print(f"{mac}: {current['status']} in {current.get('duration') or 0:.0f} s")
        return DONE[current["status"]]
    return 3


if __name__ == "__main__":
    sys.exit(main())
