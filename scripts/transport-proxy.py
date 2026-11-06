#!/usr/bin/env python3
"""Loopback impairment proxy to a real HTTPS/WSS endpoint, with verified TLS.

Limits apply to the sum of all connections in each direction. Counts include
HTTP/WebSocket framing, exclude TLS/TCP/IP overhead, and are not packet-loss
measurements. Never changes host routes or network interfaces.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import ssl
import time
from urllib.parse import parse_qs, urlsplit


class Proxy:
    """Shared bandwidth and latency budget, blackout switch and counters for every connection."""

    def __init__(self, args: argparse.Namespace) -> None:
        """Set up for the upstream and limits in `args`."""
        self.args = args
        self.host = urlsplit(args.upstream).hostname
        self.port = urlsplit(args.upstream).port or 443
        self.tls = ssl.create_default_context()
        self.next_byte = [0.0, 0.0]
        self.ready = asyncio.Event()
        self.ready.set()
        self.sessions: set[asyncio.Task | None] = set()
        self.resumer: asyncio.Task | None = None
        self.stats = {
            "connections": 0,
            "upgrades": 0,
            "blocked_upgrades": 0,
            "sent": 0,
            "received": 0,
            "lost_post_acks": 0,
            "resets": 0,
        }

    async def forward(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter, direction: int, initial: bytes = b""
    ) -> None:
        """Copy one direction (0 up, 1 down) through the shared rate limit and added latency."""
        queue = asyncio.Queue(16)

        async def read() -> None:
            data = initial
            while True:
                if not data:
                    data = await reader.read(1024)
                if not data:
                    await queue.put(None)
                    return
                for offset in range(0, len(data), 1024):
                    chunk = data[offset : offset + 1024]
                    now = time.monotonic()
                    finish = max(now, self.next_byte[direction]) + len(chunk) / self.args.rate
                    self.next_byte[direction] = finish
                    await queue.put((finish + self.args.latency_ms / 1000, chunk))
                data = b""

        async def write() -> None:
            while True:
                item = await queue.get()
                if item is None:
                    return
                at, data = item
                await asyncio.sleep(max(0, at - time.monotonic()))
                await self.ready.wait()
                writer.write(data)
                await writer.drain()
                self.stats["sent" if direction == 0 else "received"] += len(data)

        tasks = [asyncio.create_task(read()), asyncio.create_task(write())]
        try:
            await tasks[1]
        finally:
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)

    async def reply(self, writer: asyncio.StreamWriter, body: object, status: str = "200 OK") -> None:
        """Answer the client directly with a JSON body."""
        payload = json.dumps(body).encode()
        writer.write(
            f"HTTP/1.1 {status}\r\nContent-Length: {len(payload)}\r\nConnection: close\r\n".encode()
            + b"Content-Type: application/json\r\n\r\n"
            + payload
        )
        await writer.drain()

    async def resume(self, seconds: float) -> None:
        """End a blackout after `seconds`."""
        await asyncio.sleep(seconds)
        self.ready.set()

    async def control(self, path: str, writer: asyncio.StreamWriter) -> None:
        """Serve a /__test__/ request: a blackout drops every session; any answers with the counters."""
        parsed = urlsplit(path)
        if parsed.path == "/__test__/blackout":
            seconds = float(parse_qs(parsed.query).get("seconds", ["30"])[0])
            self.ready.clear()
            self.stats["resets"] += len(self.sessions)
            for task in list(self.sessions):
                task.cancel()
            self.resumer = asyncio.create_task(self.resume(seconds))
        await self.reply(writer, self.stats)

    def rewrite(self, lines: list[str], *, upgrade: bool) -> bytes:
        """Point the request head at the upstream host; plain HTTP closes after one exchange."""
        rewritten = []
        for line in lines:
            if line.lower().startswith("host:"):
                rewritten.append("Host: " + self.host)
            elif not upgrade and line.lower().startswith("connection:"):
                continue
            elif line:
                rewritten.append(line)
        if not upgrade:
            rewritten.append("Connection: close")
        return ("\r\n".join(rewritten) + "\r\n\r\n").encode("latin1")

    async def handle(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        """Proxy one client connection."""
        upstream_writer = None
        current = asyncio.current_task()
        tasks = []
        try:
            header = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 30)
            lines = header.decode("latin1").split("\r\n")
            method, path, _ = lines[0].split(" ", 2)
            if path.startswith("/__test__/"):
                await self.control(path, writer)
                return
            self.stats["connections"] += 1
            upgrade = any(line.lower() == "upgrade: websocket" for line in lines)
            if upgrade:
                self.stats["upgrades"] += 1
                if self.args.block_ws:
                    self.stats["blocked_upgrades"] += 1
                    await self.reply(writer, {"error": "test proxy blocks upgrades"}, "403 Forbidden")
                    return
            self.sessions.add(current)
            await self.ready.wait()
            upstream_reader, upstream_writer = await asyncio.open_connection(
                self.host, self.port, ssl=self.tls, server_hostname=self.host
            )
            initial = self.rewrite(lines, upgrade=upgrade)
            tasks.append(asyncio.create_task(self.forward(reader, upstream_writer, 0, initial)))
            response = await upstream_reader.readuntil(b"\r\n\r\n")
            if (
                self.args.lose_post_ack
                and not self.stats["lost_post_acks"]
                and method == "POST"
                and "/rows?" in path
                and response.startswith(b"HTTP/1.1 200")
            ):
                self.stats["lost_post_acks"] += 1
                return  # Server accepted the write; discard its response.
            tasks.append(asyncio.create_task(self.forward(upstream_reader, writer, 1, response)))
            await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        except (OSError, asyncio.IncompleteReadError, asyncio.TimeoutError):
            pass
        finally:
            self.sessions.discard(current)
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            for stream in [writer, upstream_writer]:
                if stream:
                    stream.close()
                    with contextlib.suppress(Exception):
                        await stream.wait_closed()


async def main() -> None:
    """Listen on loopback and print the listen URL as one JSON line."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", required=True)
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--rate", type=int, default=2048, help="bytes/s per direction, shared by all connections")
    parser.add_argument("--latency-ms", type=int, default=600, help="added one-way delay per chunk (pipelined)")
    parser.add_argument("--block-ws", action="store_true")
    parser.add_argument("--lose-post-ack", action="store_true")
    args = parser.parse_args()
    if urlsplit(args.upstream).scheme != "https" or args.rate <= 0:
        parser.error("upstream must use HTTPS and rate must be positive")
    proxy = Proxy(args)
    server = await asyncio.start_server(proxy.handle, "127.0.0.1", args.port)
    print(
        json.dumps(
            {
                "listen": f"http://127.0.0.1:{server.sockets[0].getsockname()[1]}",
                "upstream": args.upstream,
                "rate": args.rate,
                "latency_ms": args.latency_ms,
            }
        ),
        flush=True,
    )
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
