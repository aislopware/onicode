# Third-Party Notices

## Ghostty / libghostty-vt

The iOS and Android terminal renderers vendor upstream `libghostty-vt` (a static xcframework on
iOS, shared libraries on Android); its C headers live in the repository-root `native/libghostty-vt`.
The web terminal vendors a WebAssembly build from the same revision and uses the same C ABI.

- Upstream project: https://github.com/ghostty-org/ghostty
- Vendored revision: `6d9577708a211b0e6c5e6c6886f3cfb7efc47555`
- License: MIT

Ghostty's MIT license applies to the vendored iOS, Android, and web libraries. Keep this notice and
every artifact in sync when updating the repository-root `native/libghostty-vt`.

## MesloLGS NF (terminal font)

- Files: `android/src/main/assets/fonts/MesloLGS-NF-{Regular,Bold}.ttf`, also bundled into the iOS
  pod
- Source: https://github.com/romkatv/powerlevel10k-media (Meslo LG patched with Nerd Fonts glyphs)
- Upstream: Meslo LG by André Berg (customization of Apple's Menlo), Nerd Fonts patcher
- License: Apache License 2.0
