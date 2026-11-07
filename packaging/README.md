# Packaging

Release assets live here rather than under `dist/`, which better-update leaves out of its staging copy.

## macOS

```sh
cargo build --profile <profile> -p onicode -p onicode-engine [-p onicode-desktopd]
scripts/native/bundle-native-macos.sh [profile] [target dir] [version]
```

`bundle-native-macos.sh` assembles `Onicode.app` from what cargo built, in `<target dir>/<profile>`:

- `Contents/MacOS/onicode`: the app (`CFBundleExecutable`)
- `Contents/MacOS/Onicode Engine`: `onicode-engine`, which the app launches detached beside it, so TCC
  attributes what it does to the app (docs/onicode/decisions/engine.md, "Step 8 as built")
- `Contents/Library/LoginItems/Onicode Desktop.app` and its launchd agent plist: desktopd's helper, when built
  (`scripts/macos-desktop-helper.sh`)
- `Contents/Resources`: `onicode.icns` from `macos/icon-1024.png`, and `LICENSE` with
  `THIRD_PARTY_NOTICES.md` under `licenses/`

The release profile takes `macos/Info.plist`; any other takes `macos/Info-dev.plist`, whose data dir sits under
the target dir. The bundle is signed inside out, nested code first: ad hoc, unless
`ONICODE_NATIVE_SIGNING_IDENTITY` names a Developer ID (with `ONICODE_NATIVE_SIGNING_KEYCHAIN` when it lives in
another keychain), which adds the hardened runtime, a secure timestamp and `macos/Onicode.entitlements`. The
engine is signed under the fixed identifier `<app id>.engine` and desktopd's helper under `<app id>.desktop`, so
TCC's requirement for either never names a binary's hash. The script ends by verifying the bundle and the
engine's identifier. CI's `mac` job runs it on its dev build.

A release is `scripts/release/bundle-macos.sh`, which better-update runs (`eas.json`, profile `production`;
docs/onicode/decisions/releases.md): the release build of all three binaries for Apple silicon, the bundle
signed with the vault's Developer ID, copied to `target/package/Onicode.app`, where better-update notarizes,
staples, zips and uploads it.

## Linux

Linux is compiled and linted, not packaged: no script builds a Linux package. `onicode.desktop`, `onicode.png`
and `onicode.svg` are the desktop entry and icons such a package would carry.
