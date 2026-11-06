# Releases: better-update

macOS builds are built, signed, notarized, stapled and uploaded by better-update (`eas.json`, profile
`production`). `scripts/bundle-macos.sh` assembles an unsigned `Onicode.app`, and better-update signs it inside
out with the Developer ID identity in its vault. Release assets live in `packaging/`, because better-update
always leaves any directory named `dist` out of its staging copy. Updates will come from the better-update feed;
until then the in-app updater reads the feed named by `ONICODE_UPDATE_FEED` and stays off without one.

## The in-app updater

The app updates through `onicode-update`, in monocode's own places: the sidebar update row and its
"Updated to …" card, General → About's Version row with a Stable / Beta Channel row under it, the app
menu's "Check for Updates…" and What's new. monocode's Minisign feed is gone. The feed is
`ONICODE_UPDATE_FEED` at run time (empty turns updates off), else the one the build was made with. Its
`manifest.json` carries the release's `notes` (Markdown) and `date`, which What's new shows. A row click
stages the release (download with progress, checksum, the staged binary's `--version`, which must print
`onicode <version>`), swaps the bundle and relaunches. An update another device asks of the engine leaves
its `update-transaction.json` record; the app reads it at launch and shows "Rolled back to …" in the same
card when the trial didn't hold. `onicode_update::app_version()` is the app's one version.
