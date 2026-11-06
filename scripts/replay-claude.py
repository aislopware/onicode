#!/usr/bin/env python3
"""Replay text/reasoning from a real Onicode Claude journal at a fixed cadence.

Set CLAUDE_CODE_EXECUTABLE to this file and ONICODE_REPLAY_JOURNAL to the JSONL
captured by resource-profile.mjs. This is a deterministic adapter replay, not
a live API run. Tool workloads must use the real harness or integration tests.
ONICODE_REPLAY_DELAY_MS defaults to 40; ONICODE_REPLAY_REPEAT defaults to 1.
No network or API calls are made. Repetition is a synthetic stress workload.
"""

import json
import os
import sys
import time
from pathlib import Path

if not any(flag in sys.argv for flag in ("--print", "-p")):
    print("2.1.228 (resource profile replay)")
    sys.exit(0)

with Path(os.environ["ONICODE_REPLAY_JOURNAL"]).open() as source:
    events = [json.loads(line)["event"] for line in source if line.strip()]
if not any(e["type"] == "done" and e["status"] == "completed" for e in events):
    msg = "Replay requires a successfully completed journal"
    raise ValueError(msg)
if any(e["type"] in ("toolCall", "toolResult", "subagent", "error") for e in events):
    msg = "Only text/reasoning journals are supported"
    raise ValueError(msg)


def emit(frame: object) -> None:
    """Write one stream-json frame to stdout."""
    print(json.dumps(frame), flush=True)


emit(
    {
        "type": "system",
        "subtype": "init",
        "model": "haiku",
        "tools": [],
        "cwd": str(Path.cwd()),
        "session_id": "resource-profile-replay",
    }
)
delay = float(os.environ.get("ONICODE_REPLAY_DELAY_MS", "40")) / 1000
repeats = int(os.environ.get("ONICODE_REPLAY_REPEAT", "1"))
if repeats < 1:
    msg = "ONICODE_REPLAY_REPEAT must be positive"
    raise ValueError(msg)
# The adapter keeps one CLI alive across turns. Replay each prompt so native
# composer tests can exercise sends into an already populated transcript.
for request in sys.stdin:
    if not request.strip():
        continue
    for _ in range(repeats):
        for event in events:
            kind = event["type"]
            if kind not in ("textDelta", "reasoningDelta"):
                continue
            text = event["text"]
            delta = (
                {"type": "text_delta", "text": text}
                if kind == "textDelta"
                else {"type": "thinking_delta", "thinking": text}
            )
            emit(
                {
                    "type": "stream_event",
                    "parent_tool_use_id": None,
                    "event": {"type": "content_block_delta", "delta": delta},
                }
            )
            time.sleep(delay)
    emit(
        {
            "type": "result",
            "subtype": "success",
            "is_error": False,
            "session_id": "resource-profile-replay",
            "result": "",
        }
    )
