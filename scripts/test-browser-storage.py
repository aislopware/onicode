#!/usr/bin/env python3
"""Exercise real browser storage with synthetic data across independent processes.

macOS: python3 scripts/test-browser-storage.py --fixture /path/to/browser-fixture
"""

from __future__ import annotations

import argparse
import http.server
import json
import os
import subprocess
import sys
import tempfile
import threading
from pathlib import Path
from urllib.parse import urlparse

PAGE = r"""<!doctype html><title>Storage fixture</title><script>
(async () => {
  const mode = location.pathname.slice(1);
  const check = (ok, message) => { if (!ok) throw Error(message); };
  const db = await new Promise((resolve, reject) => {
    const r = indexedDB.open('storage-fixture', 1);
    r.onupgradeneeded = () => r.result.createObjectStore('values');
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
  const transaction = (write, value) => new Promise((resolve, reject) => {
    const tx = db.transaction('values', write ? 'readwrite' : 'readonly');
    const store = tx.objectStore('values');
    const r = write ? (value === null ? store.delete('token') : store.put(value, 'token')) : store.get('token');
    tx.oncomplete = () => resolve(r.result);
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error || Error('transaction aborted'));
  });
  if (mode === 'write') {
    document.cookie = 'durable=fixture; Path=/; Max-Age=3600; SameSite=Lax';
    document.cookie = 'expired=fixture; Path=/; Max-Age=1';
    document.cookie = 'removed=fixture; Path=/; Max-Age=3600';
    document.cookie = 'removed=; Path=/; Max-Age=0';
    localStorage.setItem('token', 'fixture');
    await transaction(true, 'fixture');
  } else if (mode === 'delete') {
    document.cookie = 'durable=; Path=/; Max-Age=0';
    localStorage.removeItem('token');
    await transaction(true, null);
  }
  const present = mode === 'write' || mode === 'read';
  const cookies = await (await fetch('/cookies')).json();
  const actual = {
    cookie: document.cookie,
    cookies,
    localStorage: localStorage.getItem('token'),
    indexedDB: (await transaction(false)) ?? null,
  };
  const failures = [];
  const verify = (ok, message) => { if (!ok) failures.push(message); };
  verify(actual.cookie.includes('durable=fixture') === present, 'persistent cookie');
  if (mode !== 'write') verify(!actual.cookie.includes('expired='), 'expired cookie returned');
  verify(!actual.cookie.includes('removed='), 'deleted cookie returned');
  verify(actual.localStorage === (present ? 'fixture' : null), 'localStorage');
  verify(actual.indexedDB === (present ? 'fixture' : null), 'IndexedDB');
  verify(cookies.includes('httpOnlyToken=fixture') === present, 'HttpOnly cookie');
  verify(!actual.cookie.includes('httpOnlyToken'), 'HttpOnly cookie exposed');
  check(failures.length === 0, failures.join(', ') + '; state=' + JSON.stringify(actual));
  db.close();
  document.title = 'storage:pass';
})().catch(error => { document.title = 'storage:fail:' + error.message; });
</script>"""


class Handler(http.server.BaseHTTPRequestHandler):
    """Serve the storage page per mode, the HttpOnly cookie, and the cookies the browser sent."""

    def do_GET(self) -> None:
        """Answer one request."""
        mode = urlparse(self.path).path
        self.send_response(200)
        self.send_header("Cache-Control", "no-store")
        if mode == "/cookies":
            body = json.dumps(self.headers.get("Cookie", "")).encode()
            self.send_header("Content-Type", "application/json")
        else:
            body = PAGE.encode()
            self.send_header("Content-Type", "text/html")
            if mode in ("/write", "/delete"):
                age = 3600 if mode == "/write" else 0
                self.send_header("Set-Cookie", f"httpOnlyToken=fixture; Path=/; HttpOnly; SameSite=Lax; Max-Age={age}")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_: object) -> None:
        """Keep the test output quiet."""


def run(args: argparse.Namespace, root: Path, origin: str, profile: str, mode: str) -> None:
    """Load the page for `mode` on `profile` in the native fixture."""
    env = dict(
        os.environ,
        ONICODE_BROWSER_STORAGE_ROOT=str(root),
        ONICODE_BROWSER_STORAGE_PROFILE=profile,
        ONICODE_BROWSER_STORAGE_URL=origin + "/" + mode,
    )
    captures = root / f"{profile}-{mode}"
    result_file = captures / "storage-result.json"
    result_file.unlink(missing_ok=True)
    command = [
        "bash",
        str(Path(__file__).resolve().parent / "run-macos-browser-fixture.sh"),
        str(Path(args.fixture).resolve()),
        str(captures),
    ]
    subprocess.run(command, env=env, check=True, timeout=60)
    assert json.loads(result_file.read_text()) == {
        "profile": profile,
        "url": origin + "/" + mode,
        "status": "pass",
    }, f"missing or invalid storage result for {profile}: {mode}"
    print(f"PASS {profile}: {mode}", flush=True)


def cleanup(fixture: str, root: Path) -> list[Exception]:
    """Clear every synthetic macOS profile, trying them all; the failures."""
    errors = []
    for profile in ("a", "a-other", "b"):
        env = dict(
            os.environ,
            ONICODE_BROWSER_STORAGE_ROOT=str(root),
            ONICODE_BROWSER_STORAGE_PROFILE=profile,
            ONICODE_BROWSER_STORAGE_URL="cleanup",
        )
        captures = root / f"{profile}-cleanup"
        result_file = captures / "storage-result.json"
        try:
            subprocess.run(
                [
                    "bash",
                    str(Path(__file__).resolve().parent / "run-macos-browser-fixture.sh"),
                    str(Path(fixture).resolve()),
                    str(captures),
                ],
                env=env,
                check=True,
                timeout=60,
            )
            assert json.loads(result_file.read_text()) == {
                "profile": profile,
                "url": "cleanup",
                "status": "pass",
            }, f"missing or invalid cleanup result for {profile}"
            print(f"PASS cleanup: {profile}", flush=True)
        except (subprocess.SubprocessError, OSError, ValueError, AssertionError) as error:
            errors.append(error)
            print(f"FAIL cleanup: {profile}: {error}", file=sys.stderr, flush=True)
    return errors


def main() -> None:
    """Run the storage scenarios, then clear every synthetic profile."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=str, required=True)
    args = parser.parse_args()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = f"http://127.0.0.1:{server.server_port}"
    with tempfile.TemporaryDirectory(prefix="onicode-browser-storage-") as temporary:
        root = Path(temporary)
        try:
            for profile, mode in [
                ("a", "write"),
                ("a", "read"),
                ("b", "empty"),
                ("a", "read"),
                ("a", "delete"),
                ("a", "empty"),
            ]:
                run(args, root, origin, profile, mode)
        finally:
            # Cleanup must attempt every synthetic profile without replacing the test failure.
            primary_error = sys.exc_info()[1]
            cleanup_errors = cleanup(args.fixture, root)
            server.shutdown()
            server.server_close()
            if cleanup_errors:
                if primary_error is not None:
                    primary_error.add_note(f"Profile cleanup also failed: {cleanup_errors!r}")
                else:
                    msg = "Profile cleanup failed"
                    raise ExceptionGroup(msg, cleanup_errors)


if __name__ == "__main__":
    main()
