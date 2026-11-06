# Rendering: aislopware/gpui-fast, macOS GUI

The GUI moves from zeron's zui to aislopware/gpui-fast (retained mode) and from `gpui-base` to aislopware/gpui-kit.
The fork already carries the zui features zeron relies on: asset eviction, list tail reservation, rounded clips,
backdrop blur, transformed glyphs, masked images and present callbacks. What remains is the edge fade API, the
browser's native composition, blur under native views and call-site drift from zed.

gpui-fast draws those effects with Metal only, so the GUI becomes macOS only (plus iOS through its own app). The
engine and CLI stay portable and build without gpui, so Linux machines can still serve as headless peers.
Retained mode's risk is a view that silently stops updating; the switch is proved by A/B runs of the same build
with view retention off and pixel-identity checks.
