# Platform floor: macOS and iOS 27.2

Onicode runs on macOS 27.2 and iOS 27.2 and later, nothing older. The owner works on current systems, and every
older release kept would mean fallback paths, runtime availability checks and APIs resolved through `dlsym`, with
tests for each. Raising the floor is only worth what the new systems offer, so the 27 SDKs are surveyed and
what makes onicode better is adopted (`.research/sdk27/ADOPTION.md`), and every pre-27.2 fallback is deleted.

`LSMinimumSystemVersion`, the iOS deployment target and `MACOSX_DEPLOYMENT_TARGET`/`IPHONEOS_DEPLOYMENT_TARGET`
for every Rust build are 27.2. CI runs the macOS and iOS lanes on GitHub's hosted `xcode-27` image (macOS 27, Xcode 27.2
with the 27.2 SDKs and simulator). The libghostty-vt prebuilts from aislopware/ghostty target 27.2 as well.

Onicode is built for macOS on Apple silicon and iOS alone (`aarch64` only; macOS 27.2 runs on nothing else, so there
is no Intel build, universal binary or x86 fallback anywhere). Windows and Linux are not supported, and their code, `cfg` branches,
dependencies, packaging, CI lanes and docs are deleted (Linux went on 2026-11-07, at the owner's word). The owner
works on macOS and iOS, the remote desktop streams only from a Mac host, and every platform kept costs code and
testing. Code may assume Apple platforms: a `cfg` only tells macOS from iOS. cargo-deny judges the graph for the
Apple targets alone, so a crate gpui-fast pulls only for Linux or the web (wgpu and its tree) never holds a
duplicate back.

The vendored crates follow the ruling: `vendor/noq-proto` and `vendor/noq-udp` keep only their Apple code (each
`SLOPTY.md` lists the sweep as a patch to re-apply on re-vendoring), and `vendor/portable-pty`, which nothing used and
whose one local patch was Windows-only, is deleted.
