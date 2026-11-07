# Releases: better-update

macOS builds are built, signed, notarized, stapled and uploaded by better-update (`eas.json`, profile
`production`), on a Mac where the owner unlocks the vault: `better-update build --platform macos --profile
production` from a clean clone. Release assets live in `packaging/`, because better-update always leaves any
directory named `dist` out of its staging copy. Updates will come from the better-update feed; until then the
in-app updater reads the feed named by `ONICODE_UPDATE_FEED` and stays off without one.

## The bundle

The profile's custom command is `scripts/release/bundle-macos.sh`. It builds `onicode`, `onicode-engine` and
`onicode-desktopd` in the release profile and assembles `Onicode.app` with
`scripts/native/bundle-native-macos.sh`, the same script a local build uses, so the two layouts cannot drift:
the app as `Contents/MacOS/onicode`, the engine beside it as `Contents/MacOS/Onicode Engine` (the app launches
it detached, engine.md "Process"), desktopd's helper under `Contents/Library/LoginItems`, and the licenses.
The build is Apple silicon only: macOS 27.2, the floor, runs on nothing else, so there is no universal binary
and no lipo.

The script signs the bundle itself, inside out, with the Developer ID identity better-update hands every
custom build (`BETTER_UPDATE_MACOS_SIGNING_IDENTITY`, its SHA-1, in the keychain named by
`BETTER_UPDATE_MACOS_KEYCHAIN`): the hardened runtime, a secure timestamp and `Onicode.entitlements` on the
engine, the helper and the app, the engine first under the identifier `<app id>.engine`. That matters because
better-update's own signer names loose code it re-signs `<app id>.<file name>`
(`dev.aislopware.onicode.Onicode-Engine`) whenever the code carries no real identity yet, which would change
the engine's designated requirement, and with it every TCC grant, from one release to the next. A bundle
already signed with the Developer ID passes better-update's audit untouched; anything it does re-sign keeps
its identifier and entitlements. better-update then notarizes the zip, staples the app and uploads it, so the
engine is notarized inside the bundle with everything else. CI's `mac` job assembles the same layout from
its dev build, signed ad hoc, and verifies it (`codesign --verify --strict --deep` and the engine's
identifier), so a broken engine target or bundle fails a pull request rather than a release.

Releases are not built in GitHub Actions: signing needs the vault, which only the owner unlocks, and the repo
holds no signing or notarization secrets.

## The in-app updater

The app updates through `onicode-update`, in monocode's own places: the sidebar update row and its
"Updated to …" card, General → About's Version row with a Stable / Beta Channel row under it, the app
menu's "Check for Updates…" and What's new. monocode's Minisign feed is gone. The feed is
`ONICODE_UPDATE_FEED` at run time (empty turns updates off), else the one the build was made with. Its
`manifest.json` carries the release's `notes` (Markdown) and `date`, which What's new shows. A row click
stages the release (download with progress, checksum, the staged binary's `--version`, which must print
`onicode <version>`), swaps the bundle and relaunches. The engine comes with the bundle: the relaunched app
finds the engine of the old build still running, and its link replaces it (engine.md, "Step 8 as built").
An update another device asks of the engine leaves its `update-transaction.json` record; the app reads it at
launch and shows "Rolled back to …" in the same card when the trial didn't hold.
`onicode_update::app_version()` is the app's one version.
