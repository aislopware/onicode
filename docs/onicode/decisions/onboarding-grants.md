# Every permission is asked once, at onboarding

Onicode asks for every privilege its features need in one checklist when it is set up, not when a feature
first runs. In slopty, remote desktop failed with "not granted" mid-session, and agents stalled on Files &
Folders prompts halfway through a run. Apple's HIG prefers asking in context. It allows onboarding when the
app can't do its job without the resource, and an agent control plane that streams and drives its Mac is
that case. Every row explains why it is needed and can be skipped, and the checklist reopens from Settings.

## Who holds the grant
- **Onicode.app** holds the grants its engine and every agent CLI need. TCC attributes a spawned child and
  its grandchildren to the responsible app, so `claude`, `codex`, their shells, `osascript` and builds run
  with Onicode's Full Disk Access, Automation and Developer Tools.
- Onicode never disclaims responsibility for its children. Claude Desktop does, and its agents lose Full
  Disk Access because of it. Ghostty rejected disclaiming as a privilege-escalation path.
- **Onicode Desktop.app**, the remote-desktop helper, holds Screen Recording, Accessibility and Post Event
  for itself. A UI process can't see another bundle's state, and a prompt belongs to the process that
  raises it, so the app starts the helper to read its rows (`--probe-grants`) and to raise one alert
  (`--request-grant <privilege>`). It starts it through Launch Services (`open -n -g -W --stdout`), never as
  a child, because TCC would judge a child of the app as the app.
- Grants follow the designated requirement: bundle id plus Team ID. Updates keep them; an ad-hoc dev build
  loses them.

## The checklist (macOS 27.2)

| Group | Privilege | Ask | Status | Relaunch |
|---|---|---|---|---|
| Remote desktop (helper) | Screen & System Audio Recording | `CGRequestScreenCaptureAccess` in the helper, then the drag panel | the helper's preflight in a fresh process | helper restarts itself |
| | Device Control and Data Access (Accessibility) | `AXIsProcessTrustedWithOptions(prompt)` in the helper | `AXIsProcessTrusted` | no |
| | Post Event | `CGRequestPostEventAccess` in the helper | `CGPreflightPostEventAccess` | no |
| | Background App Activity | `SMAppService` register, `openSystemSettingsLoginItems` on `requiresApproval` | `SMAppService.status` | no |
| Viewer | Input Monitoring | `CGRequestListenEventAccess` | `IOHIDCheckAccess` | yes |
| Agents | Full Disk Access | drag panel | spot check: `opendir` of protected folders, contents never read | yes |
| | Desktop, Documents, Downloads, network and removable volumes | covered by Full Disk Access; otherwise `opendir` each now, so the prompt comes at onboarding | the same probe | no |
| | Automation: System Events, Finder | `AEDeterminePermissionToAutomateTarget(ask)` once the target is running | the same call without asking | no |
| | App Management | drag panel | none; the person confirms it | yes |
| | Developer Tools | drag panel; it removes the XProtect first-run scan from every build an agent runs | none; the person confirms it | yes |
| Voice | Microphone | `AVCaptureDevice.requestAccess` | `authorizationStatus` | no |
| Alerts | Notifications | `UNUserNotificationCenter.requestAuthorization` | `getNotificationSettings` | no |
| Clipboard | Paste from Other Apps | one paste raises the first alert, then Settings | `NSPasteboard.accessBehavior` | yes |

Left out:
- Local Network: the tailnet is a VPN interface, which local network privacy doesn't cover.
- Camera, Bluetooth and Speech: no feature uses them.
- Remote Desktop: it needs the persistent-content-capture entitlement, which comes later.

How the checklist behaves:
- It is a window of its own, opened at the first launch, after a "Quit & Reopen" it asked for, and when a
  grant it saw on is off; Settings → General → Permissions reopens it. What the person did (alerts raised,
  rows put off with Later, the two confirmed rows, grants seen on) is kept in `permissions.json` beside
  `ui-settings.json`.
- Status is polled every second while the window shows and on activation, off the main thread and never
  taking focus. No read prompts: a preflight that only says "off" counts as "never asked" until the
  checklist raised its alert, and the folders are opened only after their alerts were.
- A row whose API prompts gets one button that raises the system alert; once refused it opens Settings. A
  list that takes a bundle (Screen Recording, Accessibility, Input Monitoring, Full Disk Access, App
  Management, Developer Tools) opens with the drag panel beside System Settings, carrying the app or the
  helper. The other lists already hold the app once it was asked, so they open plain.
- After a grant that only takes effect after a relaunch, it offers "Quit & Reopen" and resumes where it was.
  The helper's Screen Recording restarts the helper instead: the checklist tells desktopd the guide is open,
  and desktopd exits for launchd once a fresh copy of itself sees the grant.
- It refuses to run from a disk image (read-only under `/Volumes`) or a translocated path
  (`/private/var/folders/…/AppTranslocation/…`), because grants would land on a copy. A writable drive under
  `/Volumes`, such as the owner's build disk, is a lasting place.
- Nothing prompts unasked: not at launch, not from a test, not from a background process. The helper's
  login item is registered only by the checklist's click.

## The helper bundle and its signature
- desktopd ships as `Onicode.app/Contents/Library/LoginItems/Onicode Desktop.app` (bundle id
  `dev.aislopware.onicode.desktop`; a development build adds `.desktop` to its own), with `LSUIElement` and
  the Onicode icon. `SMAppService.agent` registers it from `Contents/Library/LaunchAgents/<id>.plist`
  (`BundleProgram`, `AssociatedBundleIdentifiers`), so it shows under Onicode in Background App Activity and
  in the privacy lists as "Onicode Desktop".
- It is signed inside out with its fixed identifier before the engine and the app. Releases are signed by
  better-update with team UK58J62H8L's Developer ID (`scripts/release/bundle-macos.sh`). A dev bundle
  (`scripts/native/bundle-native-macos.sh`) is ad hoc unless `ONICODE_NATIVE_SIGNING_IDENTITY` names a
  Developer ID; keep one choice per machine, because a flip between identities silently drops every grant.
- A running process reads Screen Recording once. desktopd's report is `Grants::fresh`: what it sees plus what
  a copy of itself started with `--probe-grants` sees. While a guide is open, `restart_when_granted` exits
  with status 75 once the copy sees a grant desktopd can't, and launchd's `KeepAlive` starts it again.

## The drag panel
A borderless, non-activating `NSPanel` in glass shows only while System Settings or its authentication
agent is in front, in the bottom strip of System Settings' window beside the sidebar, following its bounds
from `CGWindowListCopyWindowInfo` (no Screen Recording needed). Its icon is a file drag source carrying the
bundle. Its phases are a pure, property-tested state machine: drag, confirming, granted, Settings closed,
list left ("Go Back"), authentication cancelled. A row that reads on only after a relaunch takes the
authentication prompt's going as the grant. The ideas come from Codex, PermissionFlow, Pika
(superhighfives/pika#340) and LobeHub (lobehub/lobehub#20932).

## Not done
- macOS 15 and later ask about every 30 days whether a screen-recording app may continue. The
  `com.apple.developer.persistent-content-capture` entitlement avoids it; request it for App ID
  `dev.aislopware.onicode.desktop` under UK58J62H8L and embed a Developer ID profile with it.
- macOS 27's "Remote Desktop" list has no status API and no anchor that works, so it is not a row.

## iOS
Onboarding asks for Notifications and the Microphone. Local Network is never prompted, because engines are reached
through Tailscale's VPN interface. Its usage string stays in `Info.plist` all the same, so a route that ever falls
back to the LAN asks rather than failing silently. Camera and Photos stay just-in-time, since only a picked
attachment uses them.

Sources: `.research/onboarding-grants.md` (local only).
