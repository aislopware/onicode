#!/usr/bin/python3
"""Offline Codex native helper. Length-prefixed controls; never opens hardware/network."""

from __future__ import annotations

import json
import os
import pathlib
import struct
import sys
import time
from typing import Any

root = pathlib.Path(__file__).resolve().parents[3]
if sys.argv[1:] == ["--build-commit"]:
    print("fixture-build")
    sys.exit(0)
log = root / "helper-wire.jsonl"
(root / "helper.pid").write_text(str(os.getpid()))


def receive() -> dict[str, Any] | None:
    """Read one length-prefixed JSON frame from stdin; None at end of input."""
    header = sys.stdin.buffer.read(4)
    if not header:
        return None
    length = struct.unpack(">I", header)[0]
    if length > 128 * 1024:
        msg = "overflow"
        raise ValueError(msg)
    return json.loads(sys.stdin.buffer.read(length))


def send(value: object) -> None:
    """Write one length-prefixed JSON frame to stdout."""
    payload = json.dumps(value).encode()
    sys.stdout.buffer.write(struct.pack(">I", len(payload)) + payload)
    sys.stdout.buffer.flush()


def expect_equal(actual: object, expected: object) -> None:
    """Fail the helper when a control is not the one the host must send."""
    if actual != expected:
        msg = f"unexpected control: {actual!r}"
        raise ValueError(msg)


try:
    while (frame := receive()) is not None:
        with log.open("a") as out:
            out.write(json.dumps(frame) + "\n")
        kind = frame["type"]
        if kind == "hello":
            expect_equal(frame, {"type": "hello", "protocol": 1, "buildCommit": "fixture-build"})
            send({"type": "ready"})
        elif kind == "initializeRuntime":
            send({"type": "runtimeReady"})
        elif kind == "startTransport":
            send({"type": "offer", "sdp": "fixture-offer"})
        elif kind == "applyAnswer":
            expect_equal(frame["sdp"], "fixture-answer")
            send({"type": "transportReady"})
        elif kind == "openDevices":
            manifest = pathlib.Path(__file__).resolve().parents[1] / "manifest.json"
            version = json.loads(manifest.read_text())["appVersion"] if manifest.exists() else "0.159.0"
            expected = {"type": "openDevices"}
            if tuple(map(int, version.split("."))) >= (0, 161, 0):
                expected["selection"] = {}
            expect_equal(frame, expected)
            if (root / "helper-mode").exists() and (root / "helper-mode").read_text().strip() == "exit-open":
                sys.exit(25)
            send({"type": "devicesOpened"})
        elif kind == "setAudioControls":
            if (root / "helper-mode").exists() and (root / "helper-mode").read_text().strip() == "exit-controls":
                sys.exit(26)
            if (root / "helper-mode").exists() and (root / "helper-mode").read_text().strip() == "stall-controls":
                (root / "controls-stalled").write_text("true")
                time.sleep(60)
            send({"type": "audioControlsApplied"})
        elif kind == "inspectAudio":
            send({"type": "audioState", "state": {"microphonePeak": 1024, "speakerPeak": 0}})
        elif kind == "close":
            send({"type": "closed"})
            break
        else:
            msg = "unexpected command"
            raise ValueError(msg)
finally:
    with log.open("a") as out:
        out.write('{"type":"exited"}\n')
