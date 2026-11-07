# Packaging

## Linux (implemented)

```sh
scripts/package-linux.sh            # release build (thin LTO, stripped)
PROFILE=debug scripts/package-linux.sh   # fast smoke package
```

Produces `target/package/onicode-<version>-linux-<arch>.tar.gz` containing:

- `onicode` — the app
- `onicode-engine` — the engine, which the app starts beside itself (engine.md, "Step 8 as
  built")
- `onicode.desktop` — XDG desktop entry template (`Exec=onicode` for packagers;
  the installers rewrite `Exec`, `TryExec`, and `Icon` to absolute paths under
  `~/.onicode/app/current`, since `~/.local/bin` is often not on a desktop
  session's `PATH`)
- `onicode.png` — 1024×1024 Onicode app icon
- `install.sh` — installs into `~/.onicode/app/<version>` behind a `current`
  symlink (the curl installer's layout, which the in-app updater manages),
  links `~/.local/bin/onicode` to it, and writes the desktop entry and icon under
  `$XDG_DATA_HOME` (default `~/.local/share`)

The release profile in the root `Cargo.toml` sets `lto = "thin"` and
`strip = "symbols"` for distribution builds.

## macOS

```sh
cargo build --release -p onicode -p onicode-engine [-p onicode-desktopd]
scripts/native/bundle-native-macos.sh [profile] [target dir] [version]
```

Assembles `Onicode.app` from what cargo built: the app as `Contents/MacOS/onicode`, the engine
as `Contents/MacOS/Onicode Engine` (the app launches it detached, so TCC attributes what it does
to the app), desktopd's helper when built, Info.plist and icns. It signs ad hoc unless
`ONICODE_NATIVE_SIGNING_IDENTITY` names a Developer ID, the engine first under the identifier
`<app id>.engine`. A dmg wraps the bundle for release. The auto-update tarball retains an internal `Onicode.app` path so older
installed builds can update into Onicode. CI runs this on tags
(`.github/workflows/release.yml`). The manual steps it automates, for reference
(run on a macOS host — gpui needs Metal; no cross-build from Linux):

1. Build the universal (or per-arch) binary:
   ```sh
   cargo build --release -p onicode -p onicode-engine --target aarch64-apple-darwin
   cargo build --release -p onicode -p onicode-engine --target x86_64-apple-darwin
   lipo -create -output onicode \
     target/aarch64-apple-darwin/release/onicode \
     target/x86_64-apple-darwin/release/onicode
   ```
2. Assemble the bundle (the same for `onicode-engine`, lipo'd alike, as
   `Contents/MacOS/Onicode Engine`):
   ```sh
   mkdir -p Onicode.app/Contents/{MacOS,Resources}
   cp onicode Onicode.app/Contents/MacOS/onicode
   cp onicode-engine "Onicode.app/Contents/MacOS/Onicode Engine"
   sed "s/__VERSION__/$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')/" \
     packaging/macos/Info.plist > Onicode.app/Contents/Info.plist
   ```
3. Icon: generate `onicode.icns` from `packaging/macos/icon-1024.png` (the macOS-shaped
   variant of the artwork — squircle mask, margins, and shadow pre-baked, since
   `sips` can't apply an alpha mask) and place it at
   `Onicode.app/Contents/Resources/onicode.icns`:
   ```sh
   mkdir onicode.iconset && sips -z 256 256 packaging/macos/icon-1024.png --out onicode.iconset/icon_256x256.png
   iconutil -c icns onicode.iconset -o Onicode.app/Contents/Resources/onicode.icns
   ```
4. Sign + notarize (required for distribution), nested code first:
   ```sh
   codesign --force --options runtime --entitlements packaging/macos/Onicode.entitlements \
     --identifier dev.aislopware.onicode.engine --sign "Developer ID Application: …" \
     "Onicode.app/Contents/MacOS/Onicode Engine"
   codesign --force --options runtime --entitlements packaging/macos/Onicode.entitlements \
     --sign "Developer ID Application: …" Onicode.app
   xcrun notarytool submit Onicode.zip --keychain-profile … --wait
   xcrun stapler staple Onicode.app
   ```
5. Ship as a `.dmg` (`hdiutil create -volname Onicode -srcfolder Onicode.app -ov -format UDZO Onicode.dmg`).
