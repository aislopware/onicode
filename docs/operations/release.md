# Release

> For maintainers. Using Onicode? See [docs/user](../user/).

Onicode has one release channel. Desktop (macOS on Apple silicon; Linux AppImage and deb) and
mobile (iOS and Android) build and release through better-update at
`https://updates.jmango360.dev`, project `onicode`
(`59625ece-aea5-49b4-8978-1f045f907d92`). Desktop, server and mobile share one version.

## Setup

The [Release workflow](../../.github/workflows/release.yml) needs one repository secret,
`BETTER_UPDATE_ROBOT`: the `onicode-release` robot of project `onicode`, a developer with vault
access. `better-update credentials robot create` prints it once; `credentials access grant` gives
it the vault. It is also in Bitwarden, as `onicode GitHub Actions - BETTER_UPDATE_ROBOT`.

Our [push gateway](./push-gateway.md) and Firebase project are not in the repository, so release
builds are told about them. Without these a release still ships, with push notifications off:

- `ONICODE_PUSH_GATEWAY_URLS`, a variable of better-update's `production` environment
  (`better-update env set`): the gateway origin desktop builds bake into their server, so an
  installed app delivers through it with no setup. better-update passes it to the build command,
  from the workflow and from a Mac alike.
- `ONICODE_PUSH_GATEWAY_URL`, a repository secret: the gateway origin mobile builds register
  with. The workflow warns when it is missing.
- `ONICODE_ANDROID_GOOGLE_SERVICES_JSON`, a repository secret: the contents of the Firebase
  project's `google-services.json`. It is part of the Android fingerprint, so adding, changing or
  removing it makes the next Android release a store build.

To release from a Mac instead, on that Mac:

- `better-update login` and `gh` logged in with write access to `aislopware/onicode`. The vault
  holds team `UK58J62H8L`'s Developer ID Application certificate and App Store Connect key; builds
  ask for the vault passphrase.
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
builds to `latest`, then publish the GitHub release for that tag with the notes
(`scripts/release/github-release.sh`). The feeds are the only place the app is downloaded from:
the GitHub release carries no builds, and there is no Homebrew cask. The steps it runs, for doing
one by hand from the repo root:

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

`gh workflow run release.yml --ref main -f mobile=both -f desktop=false` ships both platforms on
GitHub Actions (`ios` or `android` ships one); add `mobile=both` to a desktop release to ship them
alongside it.

For each platform the workflow computes the production native fingerprint, which is the runtime
version. If better-update already has a production build with that runtime version, only JS
changed, so it publishes an OTA update to the `production` channel instead of building. Otherwise it
makes a store build, and fails early when the version already has one, because the build number
would repeat. `-f mobile_store_build=true` builds even when the fingerprint already has a build.

A build that exists in better-update counts as shipped, whether or not it reached the stores. An
update only helps people who installed a build with its runtime version, so submit the Android build
before relying on updates for it.

The workflow passes the planned fingerprint to the native build as
`EXPO_UPDATES_FINGERPRINT_OVERRIDE`, as EAS Build does. A build made by hand lets expo-updates
compute it again during the native build; pass the same variable if the two must agree.

iOS builds on the self-hosted runner labelled `onicode-ios`, a macOS VM on the maintainers' Mac Studio
that keeps ccache, the pnpm store and CocoaPods between runs. If it is offline the job waits in the
queue; cancel it and rerun with `-f ios_runner=github` for a hosted macOS runner, which is far slower.
The repository is public, so the organization's runner group admits only
`.github/workflows/release.yml` on `main`, and the runner's job-started hook refuses any other
repository, workflow or event.
Locally, from `apps/mobile`, whose `eas.json` holds the profiles:

```sh
better-update build --platform ios --profile production
better-update build --platform android --profile production
```

Android store builds contain arm64-v8a only (`apps/mobile/eas.json`), since every ABI adds a full
native compile. Store build numbers follow the version (0.4.0 builds as 400), so every store build
needs a new version. The workflow sends each iOS build to TestFlight with the release notes (or the
build message) as What to Test, through the `production` submit profile's ASC API key; submit Android
with `better-update submit --platform android` when ready.

Over-the-air updates come from better-update's manifest on the `production` channel. Production
builds use the fingerprint runtime version, so an update reaches only builds with the same native
code. To publish one by hand:

```sh
better-update update publish --channel production --profile production --message "<what changed>"
```

## CLI

`onicode update` and `install.sh` download the CLI archive (`oni-<version>-<platform>-<arch>.tar.gz`,
listed in `SHA256SUMS`) from the newest GitHub release of `aislopware/onicode`. The release
workflow does not build those archives yet, so both report that the release has no archive for the
platform; `scripts/build-cli-archive.ts` makes one per platform and architecture.
