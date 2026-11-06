# Desktop grants: a helper bundle, one signing identity, a probe and a drag

Slopty's owner could never use its remote desktop: the host always said Screen Recording was not granted. Slopty's
own records name four traps. A TCC grant belongs to the designated requirement, and an ad hoc rebuild is a new
executable, so the grant is gone. A process that only preflights is never listed. A running process never sees a
Screen Recording grant made after it started. And the grant belongs to the process that asks, the launchd daemon,
not the app. Onicode answers each one, in `onicode-desktop-grants` (`crates/desktop-grants`) and the bundling
scripts.

**A helper bundle.** `onicode-desktopd` ships as `Onicode.app/Contents/Library/LoginItems/Onicode Desktop.app`,
with its own Info.plist (bundle id `dev.aislopware.onicode.desktop`; a development build's is its bundle id plus
`.desktop`), `LSUIElement`, the Onicode icon and its executable named "Onicode Desktop". The app registers it with
`SMAppService.agent` from `Contents/Library/LaunchAgents/<id>.plist`, which names it with `BundleProgram` and the app
with `AssociatedBundleIdentifiers`. So it shows under Onicode in Login Items, and in the privacy lists as "Onicode
Desktop" with an icon, rather than as a bare path. The grant is the helper's: the app captures nothing, and the
engine only brokers.

**One signing identity.** The helper is signed inside out with the fixed identifier, before the engine and the
app, so its designated requirement is that identifier plus the certificate, never the binary's hash. Releases are
signed by better-update with team UK58J62H8L's Developer ID. `scripts/run-macos-dev.sh` uses
`ONICODE_DEV_CODESIGN_IDENTITY` when it is set, else the one valid "Developer ID Application" identity of team
UK58J62H8L, picked by its SHA-1. With none, or with several, it stops and says why, unless `ONICODE_DEV_ADHOC=1`
accepts an ad hoc signature whose grants last one build. It never takes the first of several: this Mac holds two
Apple Development and two Developer ID certificates, and a flip between builds silently drops every grant. An Apple
Development certificate also expires within the year and takes the grant with it.

**Probe and restart.** desktopd only ever preflights (`CGPreflightScreenCaptureAccess`, `AXIsProcessTrusted`,
`CGPreflightPostEventAccess`) and never calls a Request function, so no prompt appears unasked. Its grants report is
`Grants::fresh`: what it sees, plus what a new copy of itself sees. That copy is the same executable started with
`--probe-grants`, and launchd makes desktopd its own responsible process, so TCC judges the child as desktopd.
While the guide is open, `restart_when_granted` probes every couple of seconds. Once the child sees a grant
desktopd cannot, desktopd exits with status 75 and launchd's `KeepAlive = { SuccessfulExit = false }` starts it
again, now seeing the grant. Accessibility is live and never needs the restart. Posting events comes with
Accessibility, so the guide has two panes, Screen Recording and Accessibility.

**The panel.** Because a process that only preflights is never listed, the person adds the helper to the list
themselves, as the Codex app has them do. The ideas are ported from Pika (superhighfives/pika#340) and LobeHub
(lobehub/lobehub#20932). The app opens the list by its deep link
(`x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture`, `?Privacy_Accessibility`),
then activates System Settings itself, since a link into a running System Settings switches lists without raising
it. It never pre-registers the helper, because that adds it to the list switched off. A borderless, non-activating
floating `NSPanel` in `NSGlassEffectView` glass shows only while System Settings or its authentication agent is in
front. It sits in the bottom strip of System Settings' window, centred on the content beside the sidebar (232 points
on macOS 27), or just below a short window, and is clamped to the screen. It follows the window's bounds from
`CGWindowListCopyWindowInfo`, which needs no Screen Recording. Its icon is the helper bundle's, an
`NSDraggingSource` that carries the bundle's file URL. Its phases are a state machine, pure and property-tested: drag,
confirming (dropped, or the Touch ID or password prompt is up), granted (desktopd says so; the panel lingers, then
closes), Settings closed, list left (a new window title, or a click on the sidebar or back button when titles are
unreadable; "Go Back" reopens it) and authentication cancelled (drag again). Reduce Motion stills the icon's bob and
the fade-in.

**Not done.** macOS 15 and later ask again about every 30 days whether a screen-recording app may continue. The
`com.apple.developer.persistent-content-capture` entitlement avoids that prompt, and the owner deferred it, since the
prompt is acceptable. To get it later, request it at https://developer.apple.com/contact/request/persistent-content-capture/
for App ID `dev.aislopware.onicode.desktop` under team UK58J62H8L, then make a Developer ID provisioning profile with
it for better-update to embed. macOS 27 also has a separate "Remote Desktop" privacy list, apart from Screen
Recording. Its state is unknown to Onicode: no public API reads it, and System Settings' Privacy & Security extension
(27.2) offers no deep-link anchor for it. Its anchors are `Privacy_ScreenCapture`, `Privacy_Accessibility`,
`Privacy_AudioCapture` and the like, and none names Remote Desktop, so it is not a `Pane`.

The owner does a few things by hand once per signing identity. Allow "Onicode Desktop" under Onicode in Login Items
if macOS asks. Then drag it into Screen Recording and Accessibility through the guide, confirming each with Touch ID
or the password. A development build is "Onicode Dev Desktop", a separate entry with its own grants.
