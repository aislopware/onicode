# Release

> For maintainers. Using Onicode? See [docs/user](../user/).

Onicode has one release channel. Desktop (macOS on Apple silicon; Linux AppImage and deb; Windows
x64 NSIS installer) and mobile (iOS and Android) build and release through better-update at
`https://updates.jmango360.dev`, project `onicode`
(`59625ece-aea5-49b4-8978-1f045f907d92`). Desktop, server and mobile share one version.

## Setup

The [Release workflow](../../.github/workflows/release.yml) needs one repository secret,
`BETTER_UPDATE_ROBOT`: the `onicode-release` robot of project `onicode`, a maintainer with vault
access. A developer can build, but publishing an update to the protected `production` environment
needs the maintainer role (`better-update credentials robot update <id> --role maintainer`).
`better-update credentials robot create` prints the secret once; `credentials access grant` gives
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

Error reports go to our Sentry, which is also left out of the repository. Without these a release
ships with error reporting off ([Error reports](./observability.md#error-reports-sentry)):

- `ONICODE_SENTRY_DESKTOP_DSN`, `ONICODE_SENTRY_SERVER_DSN` and `ONICODE_SENTRY_MOBILE_DSN`,
  repository secrets: the projects each build reports to.
- `SENTRY_AUTH_TOKEN`, a repository secret: an organization token (`org:ci`) that uploads source
  maps and records each shipped release's commits and deploy, with the repository variables
  `SENTRY_URL` and `SENTRY_ORG`.

A release from a Mac reads the same names from `.env.local` or better-update's `production`
environment.

To release from a Mac instead, on that Mac:

- `better-update login` and `gh` logged in with write access to `aislopware/onicode`. The vault
  holds team `UK58J62H8L`'s Developer ID Application certificate and App Store Connect key; builds
  ask for the vault passphrase.
- Xcode 26 or later (the desktop build compiles the Apple Speech helper against the macOS 26
  SDK), Rust stable, and the repo's Node and pnpm.
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
checkout of `main` as pushed does the same with `scripts/release/publish.sh notes.md`, but for
macOS and Linux only: Windows needs a Windows host, so only the workflow ships it.

Both tag the commit `v<version>` first, build each platform, check the macOS zip, release the
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
better-update builds upload --platform windows --profile production release/Onicode-<version>-x64.exe
better-update windows release create <build-id> --channel latest --notes-file notes.md
```

The root `eas.json` holds the profiles:

- **macOS** runs `scripts/build-desktop-artifact.ts --target dir --signed` in better-update's
  staged copy, signing with the vault identity it passes as `CSC_NAME`/`CSC_KEYCHAIN`. better-update
  then notarizes, staples and zips `release/mac-arm64/Onicode.app`.
- **Linux** runs `scripts/release/build-linux.sh`, which builds x64 in a container so native modules
  and Rust helpers are Linux builds. It writes `release/linux/`. To build without better-update, run
  `scripts/release/build-linux.sh` yourself and upload with
  `better-update builds upload --platform linux`.
- **Windows** is built by [desktop-windows.yml](../../.github/workflows/desktop-windows.yml) on a
  hosted Windows runner, not by better-update, whose Windows build has not run on a Windows host:
  the installer's native modules need one. It embeds the Linux CLI archive as the WSL runtime,
  installs the result silently, checks `--version` and opens the app on a seeded thread
  (`apps/desktop/scripts/smoke-packaged-app.mjs`) before the Release workflow uploads it. The
  installer is not code-signed, so `app-update.yml` carries no `publisherName` and electron-updater
  does not check signatures. Signing it later means every update after that must carry the same
  publisher. The push gateway reaches this build from the `ONICODE_PUSH_GATEWAY_URL` secret, not
  better-update's environment.

electron-builder downloads its 7-Zip, AppImage and fpm tools from GitHub on first use. If that
fails, point `ELECTRON_BUILDER_7ZIP_PATH` at a local `7zz` (`brew install sevenzip`); the macOS
build needs nothing else from there.

Apps read their feed from `app-update.yml`: `…/feeds/<project-id>/macos`, `…/linux` and
`…/windows`. A managed
`feedUrl` policy overrides it.

better-update also serves every deb release as a signed APT repository at `…/linux/apt`, suite
`latest`, with nothing extra to run at release time. Its key derives from the server's
`APT_SIGNING_SECRET`; changing that secret breaks `apt update` on every machine that added the
repository.

Users add it as `https://onicode-apt.jmango360.dev`, a Cloudflare Worker that forwards to that
feed. Machines keep the hostname in their sources, so it has to keep answering.

### Requiring an update

`release create --critical` makes the release required: every older desktop app shows **Update
required** until it restarts into the newest release. better-update writes the newest critical
release's version into the channel file as `minimumSupportedVersion`, so the requirement outlives
later releases that are not critical. `release critical <releaseId>` marks a release that is
already out, and `--off`, halting or deleting it lifts the requirement. Apps learn
of it at their next check, up to four hours later, and an app that cannot download or install the
update lets its user dismiss the dialog.

The mobile app reads the same key from the macOS channel file, since every surface shares one
version. A phone below it installs the pending OTA update at once, or shows **Update required**
with a link to TestFlight or Google Play when only a store build lifts it. It never blocks on a
version it has no way to reach, so ship the mobile release before or with the critical one.

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

A build that exists in better-update counts as shipped, whether or not it reached the stores. If a
store build's submit step fails, do not rerun the release, which would publish an update instead:
send that build with `better-update submit --platform <ios|android> --id <build>` from `apps/mobile`.

When the log says the upload was delivered and TestFlight configuration then failed, App Store
Connect was not ready to give the build to a group. The workflow tries the assignment again for ten
minutes. After that, do not submit again, which would upload the same build number:
`better-update testflight group add-build --build-version <number> --group "Onicode Team"`.

The workflow passes the planned fingerprint to the native build as
`EXPO_UPDATES_FINGERPRINT_OVERRIDE`, as EAS Build does. A build made by hand lets expo-updates
compute it again during the native build; pass the same variable if the two must agree.

iOS builds on the self-hosted runner labelled `onicode-ios`, a macOS VM on the maintainers' Mac Studio
that keeps ccache, the pnpm store and CocoaPods between runs. The macOS desktop build runs there too
when the run ships no iOS build; with one, it takes a hosted runner so the desktop release does not
wait behind iOS. If the runner is offline its jobs wait in the queue; cancel the run and rerun with
`-f mac_runner=github` for hosted macOS runners, which are far slower.

The repository is public, and code from a fork's pull request must never run there. The
organization's runner group admits only this repository and `oni-browser-use`, but it cannot be
pinned to workflows: GitHub accepts only refs that exist, never `refs/pull/<n>/merge`, so a
restricted group would refuse every pull request run. The runner's job-started hook
(`~/runner-hooks/job-started.sh` in the VM) is the check. Before any step runs it refuses every
job but `release.yml` dispatched on `main` (here and in `oni-browser-use`), `ios-dev-client.yml`
and `scenarios.yml` on `main`, and `scenarios.yml` on a pull request whose head is a branch of
this repository. A workflow that
starts using the runner needs an entry there.
Locally, from `apps/mobile`, whose `eas.json` holds the profiles:

```sh
better-update build --platform ios --profile production
better-update build --platform android --profile production
```

Android store builds contain arm64-v8a only (`apps/mobile/eas.json`), since every ABI adds a full
native compile. Store build numbers follow the version (0.4.0 builds as 400), so every store build
needs a new version. The workflow submits each store build through the `production` submit profile:
iOS to TestFlight with the release notes (or the build message) as What to Test, Android to Google
Play's internal testing track. The Google service account behind it may release to testing tracks
only, so promoting a build further is done by hand in Play Console.

Over-the-air updates come from better-update's manifest on the `production` channel. Production
builds use the fingerprint runtime version, so an update reaches only builds with the same native
code. To publish one by hand:

```sh
better-update update publish --channel production --profile production --message "<what changed>"
```

### Telling phones about a store build

better-update serves `feeds/<project>/<ios|android>/store.json`, the newest store build it
submitted. A phone whose binary has another runtime version than that build offers the update with
a link to the store (`ONICODE_MOBILE_IOS_STORE_URL`, TestFlight by default; Google Play follows from
the package). `ONICODE_MOBILE_FEEDS_URL` names another feed root; a development build reads none
unless it is set.

## CLI

`onicode update` and `install.sh` download the CLI archive (`oni-<version>-<platform>-<arch>.tar.gz`,
listed in `SHA256SUMS`) from the newest GitHub release of `aislopware/onicode`. The release
workflow does not build those archives yet, so both report that the release has no archive for the
platform; `scripts/build-cli-archive.ts` makes one per platform and architecture.
