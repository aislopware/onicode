# Release

> For maintainers. Using Onicode? See [docs/user](../user/).

Onicode has one release channel. Desktop (macOS on Apple silicon; Linux AppImage and deb) and
mobile (iOS and Android) build and release through better-update at
`https://updates.jmango360.dev`, project `onicode`
(`59625ece-aea5-49b4-8978-1f045f907d92`). Desktop, server and mobile share one version.

## Setup

The [Release workflow](../../.github/workflows/release.yml) needs two repository secrets:

- `BETTER_UPDATE_ROBOT`: the `onicode-release` robot of project `onicode`, a developer with vault
  access. `better-update credentials robot create` prints it once; `credentials access grant` gives
  it the vault.
- `HOMEBREW_TAP_DEPLOY_KEY`: the private half of a write deploy key on `aislopware/homebrew-tap`.
  The org refuses fine-grained tokens that live over a year, and a deploy key never expires.

Both are also in Bitwarden, as `onicode GitHub Actions - <secret name>`.

To release from a Mac instead, on that Mac:

- `better-update login`, `gh` logged in with write access to `aislopware/onicode`, and SSH push
  access to `aislopware/homebrew-tap`. The vault holds team `UK58J62H8L`'s Developer ID Application
  certificate and App Store Connect key; builds ask for the vault passphrase.
- Xcode Command Line Tools, Rust stable, and the repo's Node and pnpm.
- Docker Desktop or OrbStack running, for the Linux build.

## Version

```sh
node scripts/update-release-package-versions.ts 0.4.1
```

This sets the server, desktop, web and contracts versions; mobile reads the server's. Commit it and
push to `main`.

## Desktop

Run the Release workflow on `main` with the release notes:

```sh
gh workflow run release.yml --ref main -f notes="$(cat notes.md)"
```

Without notes it only builds, which is how to try a build before releasing it. From a Mac, a clean
checkout of `main` as pushed does the same with `scripts/release/publish.sh notes.md`.

Both tag the commit `v<version>` first, build macOS and Linux, check the macOS zip, release both
builds to `latest`, publish the GitHub release for that tag with the zip, AppImage, deb and their
`SHA256SUMS` attached (`scripts/release/github-release.sh`), then point the `onicode` cask in
`aislopware/homebrew-tap` at the new macOS release. The cask's version is `<version>,<release id>`
because the download URL carries the release id. The steps it runs, for
doing one by hand from the repo root:

```sh
better-update build --platform macos --profile production
better-update build --platform linux --profile production
better-update builds list --json
better-update macos release create <build-id> --channel latest --notes-file notes.md
better-update linux release create <build-id>... --channel latest --notes-file notes.md
```

The root `eas.json` holds both profiles:

- **macOS** runs `scripts/build-desktop-artifact.ts --target dir --signed` in better-update's
  staged copy, signing with the vault identity it passes as `CSC_NAME`/`CSC_KEYCHAIN`. better-update
  then notarizes, staples and zips `release/mac-arm64/Onicode.app`.
- **Linux** runs `scripts/release/build-linux.sh`, which builds x64 in a container so native modules
  and Rust helpers are Linux builds. It writes `release/linux/`. To build without better-update, run
  `scripts/release/build-linux.sh` yourself and upload with
  `better-update builds upload --platform linux`.

electron-builder downloads its 7-Zip, AppImage and fpm tools from GitHub on first use. If that
fails, point `ELECTRON_BUILDER_7ZIP_PATH` at a local `7zz` (`brew install sevenzip`); the macOS
build needs nothing else from there.

Apps read their feed from `app-update.yml`: `…/feeds/<project-id>/macos` and `…/linux`. A managed
`feedUrl` policy overrides it.

### Taking over from the native Onicode

The macOS feed used to serve the native (Rust) Onicode, and its updater installs this build only
when all of these hold. `publish.sh` checks them before it releases:

- the zip holds `Onicode.app` alone;
- `codesign --verify --deep --strict` meets
  `identifier "dev.aislopware.onicode" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] and certificate leaf[field.1.2.840.113635.100.6.1.13] and certificate leaf[subject.OU] = "UK58J62H8L"`;
- `Onicode.app/Contents/MacOS/onicode --version` prints exactly `onicode <version>`.

The version must also be newer than the native app's last release, 0.3.0.

## Mobile

`gh workflow run release.yml --ref main -f mobile=true` makes both store builds on GitHub Actions.
Locally, from `apps/mobile`, whose `eas.json` holds the profiles:

```sh
better-update build --platform ios --profile production
better-update build --platform android --profile production
```

Store build numbers follow the version (0.4.0 builds as 400), so every store build needs a new
version. Submit to the stores through `better-update submit` when ready.

Over-the-air updates come from better-update's manifest on the `production` channel. Production
builds use the fingerprint runtime version, so an update reaches only builds with the same native
code:

```sh
better-update update publish --channel production --message "<what changed>"
```

## CLI

`onicode update` and `install.sh` download the CLI archive (`oni-<version>-<platform>-<arch>.tar.gz`,
listed in `SHA256SUMS`) from the newest GitHub release of `aislopware/onicode`. The release
workflow does not build those archives yet, so both report that the release has no archive for the
platform; `scripts/build-cli-archive.ts` makes one per platform and architecture.
