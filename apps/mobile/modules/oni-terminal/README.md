# Onicode Mobile Terminal Native Module

This local Expo module owns the native terminal surface for the mobile app.

The JavaScript contract is intentionally small:

- input from the native surface is emitted as `{ data: string }`
- resize from the native surface is emitted as `{ cols: number, rows: number }`
- remote PTY output is delivered by the existing `WsRpcClient.terminal` RPC stream

Both platforms use upstream `libghostty-vt` for terminal state, parsing, reflow, and scrollback.
`cpp/oni_terminal_core.{h,cpp}` wraps it in a small C API that both platforms compile: Android
through JNI (`android/src/main/cpp`), iOS from Swift through the pod. The core produces compact
viewport snapshots, and each platform draws them itself (an Android Canvas view, a CoreText
`UIView` on iOS), so the React Native screen and RPC code stay platform-neutral.

The vendored libraries and headers are pinned to the revision in `native/libghostty-vt/VERSION` at
the repository root. License details are in `THIRD_PARTY_NOTICES.md`.

## Rebuilding libghostty-vt

The scripts download Zig 0.17.1 when needed and check out the pinned upstream Ghostty revision.

Android rebuilds all four ABIs with 16 KB page-size support and refreshes the shared headers. Set
`ANDROID_NDK_HOME` and run:

```bash
apps/mobile/modules/oni-terminal/scripts/build-libghostty-android.sh
```

iOS rebuilds `Vendor/libghostty-vt.xcframework` (static, arm64 device and simulator) on macOS
with Xcode:

```bash
apps/mobile/modules/oni-terminal/scripts/build-libghostty-vt-ios.sh
```
