# Packaging

## Linux (implemented)

```sh
scripts/package-linux.sh            # release build (thin LTO, stripped)
PROFILE=debug scripts/package-linux.sh   # fast smoke package
```

Produces `target/package/onicode-<version>-linux-<arch>.tar.gz` containing:

- `onicode` — the binary (headed by default; `onicode headless` runs the engine alone)
- `onicode.desktop` — XDG desktop entry template (`Exec=onicode` for packagers;
  the installers rewrite `Exec`, `TryExec`, and `Icon` to absolute paths under
  `~/.onicode/app/current`, since `~/.local/bin` is often not on a desktop
  session's `PATH`)
- `onicode.png` — 1024×1024 Onicode app icon
- `install.sh` — installs into `~/.onicode/app/<version>` behind a `current`
  symlink (the curl installer's layout, which the in-app updater manages),
  links `~/.local/bin/onicode` to it, and writes the desktop entry and icon under
  `$XDG_DATA_HOME` (default `~/.local/share`). The curl installer does the same
  from the extracted tarball; `scripts/test-linux-desktop-entry.sh` checks both

The release profile in the root `Cargo.toml` sets `lto = "thin"` and
`strip = "symbols"` for distribution builds.

## macOS

```sh
scripts/package-macos.sh    # → target/package/onicode-<version>-macos-<arch>.dmg
```

Builds the release binary, assembles `Onicode.app` (Info.plist + icns), ad-hoc
signs it (set `CODESIGN_IDENTITY` for a real Developer ID), and wraps it in a
dmg. The auto-update tarball retains an internal `Onicode.app` path so older
installed builds can update into Onicode. CI runs this on tags
(`.github/workflows/release.yml`). The manual steps it automates, for reference
(run on a macOS host — gpui needs Metal; no cross-build from Linux):

1. Build the universal (or per-arch) binary:
   ```sh
   cargo build --release -p onicode --target aarch64-apple-darwin
   cargo build --release -p onicode --target x86_64-apple-darwin
   lipo -create -output onicode \
     target/aarch64-apple-darwin/release/onicode \
     target/x86_64-apple-darwin/release/onicode
   ```
2. Assemble the bundle:
   ```sh
   mkdir -p Onicode.app/Contents/{MacOS,Resources}
   cp onicode Onicode.app/Contents/MacOS/onicode
   sed "s/__VERSION__/$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')/" \
     dist/macos/Info.plist > Onicode.app/Contents/Info.plist
   ```
3. Icon: generate `onicode.icns` from `dist/macos/icon-1024.png` (the macOS-shaped
   variant of the artwork — squircle mask, margins, and shadow pre-baked, since
   `sips` can't apply an alpha mask) and place it at
   `Onicode.app/Contents/Resources/onicode.icns`:
   ```sh
   mkdir onicode.iconset && sips -z 256 256 dist/macos/icon-1024.png --out onicode.iconset/icon_256x256.png
   iconutil -c icns onicode.iconset -o Onicode.app/Contents/Resources/onicode.icns
   ```
4. Sign + notarize (required for distribution):
   ```sh
   codesign --deep --force --options runtime --sign "Developer ID Application: …" Onicode.app
   xcrun notarytool submit Onicode.zip --keychain-profile … --wait
   xcrun stapler staple Onicode.app
   ```
5. Ship as a `.dmg` (`hdiutil create -volname Onicode -srcfolder Onicode.app -ov -format UDZO Onicode.dmg`).

## Windows

```powershell
./scripts/package-windows.ps1 -ReleasesUrl https://github.com/zeronsh/onicode/releases/latest/download
```

Produces, under `target/package/`:

- `onicode-<version>-windows-<arch>-setup.exe` — the per-user installer built
  from `dist/windows/onicode.iss` with Inno Setup 6
- `onicode-<version>-windows-<arch>.zip` — the portable package
- `onicode-<version>-windows-<arch>.exe` — the bare executable the in-app
  updater downloads

The installer and the zip both carry `onicode-update.json`, the marker that lets
the app update itself in place. CI runs `scripts/test-windows-installer.ps1`
against the setup on every Windows build.
