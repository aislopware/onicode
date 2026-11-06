#!/usr/bin/env python3
"""Fake ACP agent whose turn calls one MCP tool.

Speaks just enough ACP over stdio (initialize, session/new|load, the
model/config sets, session/prompt, session/cancel). On each prompt it starts
the stdio MCP servers it received in `session/new` `mcpServers`, does the MCP
handshake, and calls one tool; the reply text is the `tools/call` result as
compact JSON (`{"content": [...], "structuredContent": ..., "isError": ...}`),
or a one-line `fake-acp-tool: ...` message when nothing could be called.

Which tool:
  FAKE_ACP_CALL_TOOL=<name>   the tool to call (required, unless the prompt
                              names one, below)
  FAKE_ACP_CALL_ARGS=<json>   its arguments (default {})
  A prompt `call-tool <name> [<json args>]` overrides both for that turn.

Which server:
  FAKE_ACP_MCP_SERVER=<name>  call it on this `mcpServers` entry only;
                              otherwise each entry is asked (`tools/list`) in
                              order and the first that lists the tool is used.
  FAKE_ACP_TOOL_TIMEOUT=<s>   per MCP request (default 60)

Run it as any ACP slot, e.g. GROK_EXECUTABLE=/abs/path/fake-acp-tool.py.
Diagnostics go to stderr; stdout carries only ACP frames.
"""

from __future__ import annotations

import contextlib
import json
import os
import queue
import subprocess
import sys
import threading
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterator

SESSION = {"id": "fake-acp-tool-session"}
TIMEOUT = float(os.environ.get("FAKE_ACP_TOOL_TIMEOUT", "60"))


def emit(frame: object) -> None:
    """Write one ACP frame to stdout."""
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def log(text: str) -> None:
    """Write a diagnostic line to stderr."""
    sys.stderr.write(f"fake-acp-tool: {text}\n")
    sys.stderr.flush()


def reply_text(text: str) -> None:
    """Send `text` as an agent message chunk."""
    emit(
        {
            "method": "session/update",
            "params": {
                "sessionId": SESSION["id"],
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}},
            },
        }
    )


class McpServer:
    """One stdio MCP server child, newline-delimited JSON-RPC."""

    def __init__(self, spec: dict, cwd: str | None) -> None:
        """Start the server `spec` describes (an ACP `mcpServers` entry)."""
        env = dict(os.environ)
        for pair in spec.get("env") or []:
            env[pair["name"]] = pair["value"]
        self.name = spec.get("name", "")
        self.proc = subprocess.Popen(
            [spec["command"], *(spec.get("args") or [])],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=sys.stderr,
            env=env,
            cwd=cwd or None,
            text=True,
            bufsize=1,
        )
        self.lines: queue.Queue[str | None] = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.next_id = 0

    def _read(self) -> None:
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def notify(self, method: str, params: dict | None = None) -> None:
        """Send a JSON-RPC notification."""
        frame = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            frame["params"] = params
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()

    def request(self, method: str, params: dict) -> dict | None:
        """Send a request and wait up to TIMEOUT for its result."""
        self.next_id += 1
        ident = self.next_id
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        while True:
            try:
                line = self.lines.get(timeout=TIMEOUT)
            except queue.Empty:
                msg = f"{self.name}: no answer to {method} within {TIMEOUT:g}s"
                raise RuntimeError(msg) from None
            if line is None:
                msg = f"{self.name}: server exited during {method}"
                raise RuntimeError(msg)
            try:
                message = json.loads(line)
            except ValueError:
                continue
            # Server notifications (e.g. notifications/tools/list_changed)
            # and anything else without our id are skipped.
            if message.get("id") != ident or "method" in message:
                continue
            if "error" in message:
                msg = f"{self.name}: {method}: {message['error'].get('message')}"
                raise RuntimeError(msg)
            return message.get("result")

    def start(self) -> McpServer:
        """Do the MCP handshake."""
        self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "fake-acp-tool", "version": "1"},
            },
        )
        self.notify("notifications/initialized")
        return self

    def close(self) -> None:
        """Close stdin and reap the child, killing it after five seconds."""
        with contextlib.suppress(OSError):
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def call_tool(servers: list[dict], cwd: str | None, tool: str, args: object) -> str:
    """Call `tool` on the first server that has it; the reply text for the turn."""
    pinned = os.environ.get("FAKE_ACP_MCP_SERVER")
    candidates = [s for s in servers if not pinned or s.get("name") == pinned]
    if not candidates:
        return f"fake-acp-tool: no MCP server{' named ' + pinned if pinned else 's'} in session/new"
    for spec in candidates:
        server = McpServer(spec, cwd)
        try:
            server.start()
            if not pinned:
                listed = server.request("tools/list", {}) or {}
                names = [t.get("name") for t in listed.get("tools", [])]
                if tool not in names:
                    log(f"{server.name} does not list {tool} ({names})")
                    continue
            result = server.request("tools/call", {"name": tool, "arguments": args})
            return json.dumps(result, separators=(",", ":"))
        except (RuntimeError, OSError, ValueError, KeyError) as error:  # report it as the reply, keep serving
            log(str(error))
            if pinned:
                return f"fake-acp-tool: {error}"
        finally:
            server.close()
    return f"fake-acp-tool: no MCP server lists {tool}"


def prompt_text(params: dict) -> str:
    """Join a prompt's text blocks."""
    return "".join(
        block.get("text", "")
        for block in params.get("prompt", [])
        if isinstance(block, dict) and block.get("type") == "text"
    )


def pick_tool(text: str) -> tuple[str | None, object]:
    """Pick the tool and arguments for this turn: from the prompt, else from the environment."""
    text = text.strip()
    if text.startswith("call-tool "):
        rest = text[len("call-tool ") :].strip()
        name, _, raw = rest.partition(" ")
        return name, json.loads(raw) if raw.strip() else {}
    tool = os.environ.get("FAKE_ACP_CALL_TOOL")
    raw = os.environ.get("FAKE_ACP_CALL_ARGS") or "{}"
    return tool, json.loads(raw)


def frames() -> Iterator[dict]:
    """Yield each JSON frame on stdin, skipping blank and malformed lines."""
    for line in sys.stdin:
        text = line.strip()
        if not text:
            continue
        try:
            yield json.loads(text)
        except ValueError:
            continue


def prompt(ident: object, params: dict, servers: list[dict], cwd: str | None) -> None:
    """Run one turn: call the tool and answer with its result."""
    try:
        tool, args = pick_tool(prompt_text(params))
    except ValueError as error:
        tool, args = None, None
        reply_text(f"fake-acp-tool: bad tool arguments: {error}")
    if tool:
        reply_text(call_tool(servers, cwd, tool, args))
    elif args is not None:
        reply_text("fake-acp-tool: set FAKE_ACP_CALL_TOOL or prompt `call-tool <name> [json]`")
    emit({"id": ident, "result": {"stopReason": "end_turn"}})


def main() -> None:
    """Serve ACP on stdio until stdin closes."""
    servers, cwd = [], None
    for frame in frames():
        method, ident = frame.get("method"), frame.get("id")
        params = frame.get("params") or {}
        if method is None:
            continue  # a response to something we never asked
        if method == "initialize":
            emit({"id": ident, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}})
        elif method in ("session/new", "session/load"):
            # Discovery opens sessions with no servers; keep the last real set.
            if params.get("mcpServers"):
                servers = params["mcpServers"]
            cwd = params.get("cwd") or cwd
            SESSION["id"] = params.get("sessionId") or SESSION["id"]
            if method == "session/load":
                emit({"id": ident, "result": {}})
            else:
                emit({"id": ident, "result": {"sessionId": SESSION["id"]}})
        elif method == "session/prompt":
            prompt(ident, params, servers, cwd)
        elif ident is not None:
            # session/set_model, session/set_config_option, session/set_mode, …
            emit({"id": ident, "result": {}})


if __name__ == "__main__":
    main()
