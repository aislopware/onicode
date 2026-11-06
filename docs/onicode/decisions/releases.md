# Releases: better-update

macOS builds are built, signed, notarized, stapled and uploaded by better-update (`eas.json`, profile
`production`). `scripts/bundle-macos.sh` assembles an unsigned `Onicode.app`, and better-update signs it inside
out with the Developer ID identity in its vault. Release assets live in `packaging/`, because better-update
always leaves any directory named `dist` out of its staging copy. Updates will come from the better-update feed
instead of zeron's edge; until then the in-app updater has no feed and stays off.
