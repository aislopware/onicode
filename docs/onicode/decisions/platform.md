# Platform floor: macOS and iOS 27.2

Onicode runs on macOS 27.2 and iOS 27.2 and later, nothing older. The owner works on current systems, and every
older release kept would mean fallback paths, runtime availability checks and APIs resolved through `dlsym`, with
tests for each. Raising the floor is only worth what the new systems offer, so the 27 SDKs are surveyed and
what makes onicode better is adopted (`.research/sdk27/ADOPTION.md`), and every pre-27.2 fallback is deleted.

`LSMinimumSystemVersion`, the iOS deployment target and `MACOSX_DEPLOYMENT_TARGET`/`IPHONEOS_DEPLOYMENT_TARGET`
for every Rust build are 27.2. CI runs the macOS and iOS lanes on GitHub's hosted `xcode-27` image (macOS 27, Xcode 27.2
with the 27.2 SDKs and simulator). The libghostty-vt prebuilts from aislopware/ghostty target 27.2 as well.

Windows is not supported, and its code, dependencies, packaging and docs are deleted. The owner works on macOS and
iOS, the remote desktop streams only from a Mac host, and every platform kept costs code and testing. Linux keeps
compiling, held by a clippy check for its target, but gets no new work.
