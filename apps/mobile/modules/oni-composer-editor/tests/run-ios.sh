#!/bin/bash
set -euo pipefail
module_dir=$(cd "$(dirname "$0")/.." && pwd)
sdk=$(xcrun --sdk macosx --show-sdk-path)
compiler=$(xcrun --find swiftc)
output=$(mktemp -d)
trap 'rm -rf "$output"' EXIT
# Compile the real editor view and clipboard helpers; only the Expo bridge is stubbed.
python3 - "$module_dir" "$output/main.swift" <<'PYTHON'
import sys
from pathlib import Path
module = Path(sys.argv[1])
source = (module / "ios/OniComposerEditorView.swift").read_text()
source = source.replace("import ExpoModulesCore\n", "")
stubs = """
public final class AppContext {}
open class ExpoView: UIView {
  public required init(appContext: AppContext? = nil) { super.init(frame: .zero) }
  @available(*, unavailable)
  public required init?(coder: NSCoder) { fatalError("init(coder:) is unused") }
}
final class EventDispatcher {
  var payloads: [[String: Any]] = []
  func callAsFunction(_ payload: [String: Any] = [:]) { payloads.append(payload) }
}
"""
# The clipboard helpers are plain UIKit, so the real ones compile beside the view.
bridge = (module / "ios/OniComposerEditorModule.swift").read_text()
start = bridge.index("enum OniComposerClipboard {")
clipboard = bridge[start : bridge.index("\n}\n", start) + 3]
Path(sys.argv[2]).write_text(
  source + "\n" + stubs + "\n" + clipboard + "\n" + (module / "tests/ios/main.swift").read_text()
)
PYTHON
"$compiler" -target "$(uname -m)-apple-ios16.1-macabi" -sdk "$sdk" \
  -F "$sdk/System/iOSSupport/System/Library/Frameworks" \
  "$output/main.swift" -o "$output/editor-tests"
"$output/editor-tests"
