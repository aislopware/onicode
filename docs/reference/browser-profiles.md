# Browser profiles and login persistence

The embedded browser (WebKit, macOS) keeps persistent website data. Closing tabs or restarting Onicode does not
clear persistent cookies, localStorage or IndexedDB. A website can still expire or revoke a login, and session-only
cookies stay session-only.

| Action | Effect |
| --- | --- |
| Switch tabs or hide the panel | Keeps website data |
| Close all browser tabs, then reopen | Reuses the profile |
| Restart Onicode | Reopens persistent data |
| Switch Onicode profile | Opens a separate store; returning recovers saved data |

## Identity and storage

Tabs and windows in one Onicode process share one browser context per profile. Chat, project, repository and
navigation changes do not choose a new profile. Website data stays on the device showing it, including when viewing
a remote engine's chat, and is never synchronised between devices.

Each profile is a named `WKWebsiteDataStore` whose UUID is derived from the canonical data directory and profile
identity, so the bundle identifier and data directory must stay stable to reuse a store. Separate installations and
test data directories select separate stores. Copying or removing Onicode's data directory does not copy or remove
the WebKit-managed stores. See [WebKit profile APIs](https://webkit.org/blog/14423/building-profiles-with-new-webkit-api/).

## Validation

```sh
scripts/dev/check.sh test 'package(onicode-ui) and test(/browser/)'
cargo build --locked -p onicode-ui --example browser-fixture --features browser-fixture
python3 scripts/test-browser-storage.py --fixture "$CARGO_TARGET_DIR/debug/examples/browser-fixture"
```

The storage driver needs a logged-in macOS desktop session. It serves a loopback-only site and launches independent
fixture processes with temporary profiles, checking persistent and HttpOnly cookies, cookie expiry and deletion,
localStorage, IndexedDB, closing and reopening the last tab, switching profiles with live pages, and returning to the
first profile. It runs under the release Info.plist through the fixture bundle wrapper and removes its named WebKit
stores afterwards. The tests use synthetic tokens, never personal accounts.
