#!/usr/bin/env python3
"""Bounded, read-only native Codex voice probe. Never starts inference/audio."""

import asyncio
import contextlib
import json
import shutil

METHODS = (
    (
        "initialize",
        {"clientInfo": {"name": "onicode-voice-probe", "version": "1"}, "capabilities": {"experimentalApi": True}},
    ),
    ("account/read", {"refreshToken": False}),
    ("account/rateLimits/read", {}),
    ("thread/realtime/listVoices", {}),
)


async def probe() -> None:
    """Ask a local Codex app server which voice methods it supports and print the answers."""
    executable = shutil.which("codex")
    if not executable:
        msg = "Codex is not installed"
        raise RuntimeError(msg)
    child = await asyncio.create_subprocess_exec(
        executable,
        "app-server",
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
        limit=256 * 1024,
    )
    pending = {}

    async def read() -> None:
        try:
            while line := await child.stdout.readline():
                message = json.loads(line)
                future = pending.get(message.get("id"))
                if future and not future.done():
                    future.set_result(message)
        finally:
            for future in pending.values():
                if not future.done():
                    future.set_exception(RuntimeError("Codex stdout closed"))

    reader = asyncio.create_task(read())
    result = {}
    try:
        for identifier, (method, params) in enumerate(METHODS, 1):
            future = asyncio.get_running_loop().create_future()
            pending[identifier] = future
            child.stdin.write((json.dumps({"id": identifier, "method": method, "params": params}) + "\n").encode())
            await child.stdin.drain()
            try:
                message = await asyncio.wait_for(future, timeout=8)
            finally:
                pending.pop(identifier, None)
            # Expose only successful method support and account type. Never
            # print raw errors, account identifiers, balances, tokens or audio.
            result[method] = {"supported": "result" in message}
            if method == "account/read":
                account = (message.get("result") or {}).get("account") or {}
                kind = account.get("type")
                result[method]["chatgpt"] = kind == "chatgpt" if kind else None
        result["creditExclusionVerified"] = False
        print(json.dumps(result, indent=2))
    finally:
        reader.cancel()
        await asyncio.gather(reader, return_exceptions=True)
        await stop(child)


async def stop(child: asyncio.subprocess.Process) -> None:
    """Terminate the app server, killing it after two seconds."""
    if child.returncode is None:
        with contextlib.suppress(ProcessLookupError):
            child.terminate()
        try:
            await asyncio.wait_for(child.wait(), 2)
        except asyncio.TimeoutError:
            child.kill()
            await child.wait()


if __name__ == "__main__":
    asyncio.run(probe())
