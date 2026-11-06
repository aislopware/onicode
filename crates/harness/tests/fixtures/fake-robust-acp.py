#!/usr/bin/env python3
"""ACP wire edge cases shared by adapter specifications."""

from __future__ import annotations

import json
import os
import signal
import sys
import threading
import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from types import FrameType

FOREIGN_PERMISSION = 900


def emit(frame: object) -> None:
    """Write one JSON-RPC frame, CRLF-terminated."""
    sys.stdout.write(json.dumps(frame) + "\r\n")
    sys.stdout.flush()


def update(text: str) -> None:
    """Send an agent message chunk on the parent session."""
    emit(
        {
            "method": "session/update",
            "params": {
                "sessionId": "parent",
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}},
            },
        }
    )


pending = None
for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get("method")
    ident = frame.get("id")
    if method is None and ident == FOREIGN_PERMISSION:
        if frame["result"]["outcome"]["outcome"] != "cancelled":
            raise ValueError(frame)
        update("foreign permission rejected")
        emit({"id": pending, "result": {"stopReason": "end_turn"}})
    elif method == "initialize":
        emit({"id": ident, "result": {"protocolVersion": 1, "agentCapabilities": {}}})
    elif method == "session/new":
        for index in range(40):
            emit(
                {
                    "method": "session/update",
                    "params": {
                        "sessionId": f"foreign-{index}",
                        "update": {"sessionUpdate": "current_mode_update", "currentModeId": "wrong"},
                    },
                }
            )
        for kind, key, value in [
            ("available_commands_update", "availableCommands", [{"name": "early", "description": "Early command"}]),
            ("config_option_update", "configOptions", []),
            ("current_mode_update", "currentModeId", "plan"),
        ]:
            emit(
                {
                    "method": "session/update",
                    "params": {"sessionId": "parent", "update": {"sessionUpdate": kind, key: value}},
                }
            )
        emit({"id": ident, "result": {"sessionId": "parent"}})
    elif method == "session/prompt":
        prompt = frame["params"]["prompt"][0]["text"]
        if prompt == "steer-live":
            update("first")

            def finish_first(prompt_id: object = ident) -> None:
                """Settle the first prompt after the steer arrives."""
                time.sleep(0.2)
                emit({"id": prompt_id, "result": {"stopReason": "end_turn"}})

            threading.Thread(target=finish_first).start()
            continue
        if prompt in ("steer-idle", "second"):
            update(prompt)
        if prompt == "idle-pid":
            update(str(os.getpid()))
        if prompt in ("wedge", "late-settle"):
            pending = ident
            if prompt == "late-settle":

                def late_response(_signal: int, _frame: FrameType | None, prompt_id: object = ident) -> None:
                    """Answer the wedged prompt only once told to stop."""
                    emit(
                        {
                            "id": prompt_id,
                            "result": {"stopReason": "end_turn", "usage": {"inputTokens": 900, "outputTokens": 900}},
                        }
                    )

                signal.signal(signal.SIGTERM, late_response)
            else:
                signal.signal(signal.SIGTERM, signal.SIG_IGN)
            update("ready")
            continue
        if prompt == "foreign":
            pending = ident
            emit(
                {"method": "_x.ai/session/prompt_complete", "params": {"sessionId": "child", "stopReason": "end_turn"}}
            )
            emit(
                {
                    "method": "_x.ai/session_notification",
                    "params": {
                        "sessionId": "child",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": "foreign"},
                        },
                    },
                }
            )
            emit(
                {
                    "method": "session/update",
                    "params": {
                        "sessionId": "child",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": "foreign"},
                        },
                    },
                }
            )
            emit(
                {
                    "id": FOREIGN_PERMISSION,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "child",
                        "options": [{"optionId": "allow", "kind": "allow_once", "name": "Allow"}],
                    },
                }
            )
            continue
        if prompt == "frames":
            print("diagnostic noise", flush=True)
            for noise in [None, 42, [], "plain", {"unrelated": True}]:
                emit(noise)
            emit({"id": ident, "diagnostic": "not a response"})
            emit({"jsonrpc": "other", "id": ident, "result": {}})
            update("x" * (2 * 1024 * 1024))
        elif prompt == "burst":
            for index in range(300):
                update(f"{index},")
        emit({"id": ident, "result": {"stopReason": "end_turn"}})
