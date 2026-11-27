# Third-Party Notices

## Ghostty / libghostty-vt

The iOS and Android terminal renderers vendor upstream `libghostty-vt` (a static xcframework on
iOS, shared libraries on Android); its C headers live in the repository-root `native/libghostty-vt`.
The web terminal vendors a WebAssembly build from the same revision and uses the same C ABI.

- Upstream project: https://github.com/ghostty-org/ghostty
- Vendored revision: `d69f9ed83ca7275fa5afbba0fa968d0d9ada9f5a`
- License: MIT

Ghostty's MIT license applies to the vendored iOS, Android, and web libraries. Keep this notice and
every artifact in sync when updating the repository-root `native/libghostty-vt`.
