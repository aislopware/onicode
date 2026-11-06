#!/usr/bin/env python3
"""ACP contract fixture: saved policy differs from reported ask mode."""

import json
import sys

mode_written = False
model = "model-a"
pending = None


def emit(value: object) -> None:
    """Write one JSON-RPC frame to stdout."""
    print(json.dumps(value), flush=True)


def option(key: str, category: str, values: list[str], current: str) -> dict[str, object]:
    """One select-type session config option."""
    return {
        "id": key,
        "name": key,
        "category": category,
        "type": "select",
        "currentValue": current,
        "options": [{"value": v, "name": v} for v in values],
    }


for line in sys.stdin:
    msg = json.loads(line)
    method, params = msg.get("method"), msg.get("params", {})
    result = {}
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}
    elif method in ("session/new", "session/load"):
        mode_written = False
        result = {
            "sessionId": "fx-session",
            "configOptions": [
                option("provider", "model", ["backend"], "backend"),
                option("model", "model", ["model-a", "model-b"], model),
                option("mode", "mode", ["ask", "auto"], "ask"),
                option("effort", "thought_level", ["low", "medium", "high"], "medium"),
            ],
        }
    elif method == "session/set_config_option":
        key, value = params["configId"], params["value"]
        if key == "mode" and value == "ask":
            mode_written = True
        elif key == "model" and value != "rejected-model":
            model = value
        else:
            emit({"id": msg["id"], "error": {"code": -32602, "message": "invalid selection"}})
            continue
    elif method == "session/prompt":
        if not mode_written or model != "model-b":
            emit({"id": msg["id"], "error": {"code": -32603, "message": "unsafe or wrong model"}})
            continue
        if params.get("prompt", [{}])[0].get("text") == "permission":
            pending = msg["id"]
            emit(
                {
                    "id": "approve",
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "fx-session",
                        "toolCall": {"toolCallId": "write-1", "title": "Write file", "kind": "edit"},
                        "options": [
                            {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                            {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
                        ],
                    },
                }
            )
            continue
        emit(
            {
                "method": "session/update",
                "params": {
                    "sessionId": "fx-session",
                    "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "fx reply"}},
                },
            }
        )
        result = {"stopReason": "end_turn"}
    elif method is None:
        if msg.get("id") == "approve":
            selected = msg.get("result", {}).get("outcome", {}).get("optionId", "deny")
            emit(
                {
                    "method": "session/update",
                    "params": {
                        "sessionId": "fx-session",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": selected},
                        },
                    },
                }
            )
            emit({"id": pending, "result": {"stopReason": "end_turn"}})
        continue
    if "id" in msg:
        emit({"id": msg["id"], "result": result})
