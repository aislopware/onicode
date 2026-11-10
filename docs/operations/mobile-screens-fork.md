# Building the mobile Screens dependency

The mobile v5 stack uses a committed tarball built from `main` of
[aislopware/react-native-screens](https://github.com/aislopware/react-native-screens).
The fork owns the integration changes; the app does not patch this package.
Each archive includes compiled JavaScript, declarations, native source and
`oni-fork.json` recording its version, source commit and upstream base.

To reproduce an archive, use Node 24 from a clean fork checkout at the source
commit recorded in its `oni-fork.json`. The `pack:onicode` script installs the
locked dependencies with the repository's pinned Yarn before it builds, so run
it with Node on a fresh clone:

```sh
node scripts/pack-onicode.mjs /absolute/path/to/onicode/apps/mobile/deps
```

For an update, merge [upstream](https://github.com/software-mansion/react-native-screens)
and the required proposal branches into the fork's `main`, increment the
`5.0.0-oni.N` version, pin `oniFork.upstreamCommit` to the integrated upstream
SHA and commit. Run `pack:onicode` from that clean checkout and tag the source
as `oni-v<version>`. Push `main` and the tag to the fork before submitting the
app change. Replace the old archive, update `apps/mobile/package.json` and run
`vp i` to regenerate the lockfile. Verify mobile types and affected navigation
tests. Changes to native source also require rebuilding and testing the native
client.

The same iOS binary supports iPhone, iPad, and Duo. UIKit hinge updates enable
phone workspace columns at runtime; regular phones retain portrait rotation.
Use the normal EAS build profiles and update channels. Duo support requires a
binary built with the iOS 27.1 SDK; an OTA cannot add the native APIs.
