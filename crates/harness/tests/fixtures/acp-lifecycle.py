#!/usr/bin/env python3
"""Stateful ACP peer: reject overlapping prompts and expose real error.data."""

from __future__ import annotations

import json
import os
import select
import sys
import time


def emit(value: dict[str, object]) -> None:
    """Write one JSON-RPC 2.0 frame to stdout."""
    print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)


def update(value: dict[str, object]) -> None:
    """Send a session update."""
    emit({"method": "session/update", "params": {"sessionId": "s-1", "update": value}})


def reply(request: dict[str, object], result: dict[str, object]) -> None:
    """Answer a request with a result."""
    emit({"id": request["id"], "result": result})


def error(request: dict[str, object], data: dict[str, object]) -> None:
    """Reject a request, carrying `data` as error.data."""
    emit({"id": request["id"], "error": {"code": -32600, "message": "Invalid request", "data": data}})


# Unbuffered reads keep select honest, including multiple queued frames.
buffer = bytearray()


def read(timeout: float | None = None) -> dict | None:
    """Read one frame; None when `timeout` passes first, and exit at end of input."""
    deadline = None if timeout is None else time.monotonic() + timeout
    while b"\n" not in buffer:
        left = None if deadline is None else max(0, deadline - time.monotonic())
        if not select.select([sys.stdin], [], [], left)[0]:
            return None
        data = os.read(sys.stdin.fileno(), 65536)
        if not data:
            sys.exit(0)
        buffer.extend(data)
    end = buffer.index(b"\n")
    line = bytes(buffer[:end])
    del buffer[: end + 1]
    return json.loads(line)


turn = 0
while True:
    request = read()
    method = request.get("method")
    if method == "initialize":
        reply(request, {"protocolVersion": 1, "agentCapabilities": {}})
    elif method == "session/new":
        reply(request, {"sessionId": "s-1"})
    elif method == "session/prompt":
        turn += 1
        scenario = request["params"]["prompt"][0]["text"]
        if scenario == "error":
            error(request, {"reason": "A prompt is already running", "retryable": False})
            continue
        if turn > 1:
            update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": scenario}})
            reply(request, {"stopReason": "end_turn"})
            if scenario == "self-continue":
                time.sleep(1.3)
                update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "autonomous"}})
            continue
        if scenario != "reasoning":
            update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "working"}})
        if scenario in ("tools", "open-tool", "cancel", "wedge", "eof"):
            for i in range(4):
                update(
                    {
                        "sessionUpdate": "tool_call",
                        "toolCallId": str(i),
                        "kind": "read",
                        "title": "read source",
                        "status": "pending",
                        "rawInput": {"path": "/tmp/source.rs"},  # noqa: S108 (a path in a fake tool call; nothing opens it)
                    }
                )
                if scenario != "open-tool":
                    update(
                        {
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": str(i),
                            "status": "completed",
                            "content": [],
                        }
                    )
        if scenario == "usage":
            update(
                {
                    "sessionUpdate": "usage_update",
                    "used": 100,
                    "size": 10000,
                    "cost": {"amount": 0.01, "currency": "USD"},
                }
            )
        if scenario == "reasoning":
            update({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "thinking"}})
        # Three retired quiet windows. Any ordinary prompt during this gap
        # must fail, just like a real ACP agent that is awaiting its model.
        deadline = time.monotonic() + 3.6
        cancelled = False
        while time.monotonic() < deadline or scenario in ("cancel", "wedge"):
            incoming = read(max(0, deadline - time.monotonic()) if scenario not in ("cancel", "wedge") else None)
            if incoming is None:
                break
            if incoming.get("method") == "session/cancel":
                if scenario == "wedge":
                    continue
                cancelled = True
                break
            if "id" in incoming:
                error(incoming, {"reason": "A prompt is already running"})
        if scenario == "eof":
            sys.exit(0)
        if not cancelled:
            update({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "finished"}})
        reply(request, {"stopReason": "cancelled" if cancelled else "end_turn"})
    elif "id" in request:
        reply(request, {})
