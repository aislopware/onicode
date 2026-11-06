#!/usr/bin/python3
"""Initialize-only peer: models and commands must share one subprocess."""

import json
import pathlib
import sys

if "--version" in sys.argv:
    print("2.1.228 (Claude Code)")
    sys.exit(0)
root = pathlib.Path(__file__).parent
request = json.loads(sys.stdin.readline())
if request["type"] != "control_request" or request["request"]["subtype"] != "initialize":
    msg = "expected an initialize control request"
    raise ValueError(msg)
with (root / "calls").open("a") as calls:
    calls.write("initialize\n")
response = json.loads((root / "response.json").read_text())
response["request_id"] = request["request_id"]
print(json.dumps({"type": "control_response", "response": response}), flush=True)
