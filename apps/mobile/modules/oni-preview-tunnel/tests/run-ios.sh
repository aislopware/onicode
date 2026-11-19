#!/bin/bash
set -euo pipefail
module_dir=$(cd "$(dirname "$0")/.." && pwd)
output=$(mktemp -d)
trap 'rm -rf "$output"' EXIT
# Compile the real HTTP rewriter and stand-in mapping; nothing else in those files is needed.
python3 - "$module_dir" "$output/main.swift" <<'PYTHON'
import sys
from pathlib import Path
module = Path(sys.argv[1])
rewriter = (module / "ios/OniHttpRewriter.swift").read_text()
web_view = (module / "ios/OniTunnelWebView.swift").read_text()
loopback = web_view[web_view.index("enum OniLoopback") :]
tests = (module / "tests/ios/main.swift").read_text()
Path(sys.argv[2]).write_text("import Network\n" + rewriter + loopback + "\n" + tests)
PYTHON
xcrun swiftc "$output/main.swift" -o "$output/tunnel-tests"
"$output/tunnel-tests"
