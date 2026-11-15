# Scenarios

A scenario is an [e2e](https://e2e.com/docs) test that shows one behavior in the real app: the dev
desktop app on its own Onicode home, with its real server, HTTP and WebSocket, or an iPhone
simulator running the mobile app. The dev replay agent stands in for the model, so a run is free
and repeatable. Together the desktop scenarios are the desktop app's end-to-end suite. One of them
is also the proof for a bug fix: it fails on the base commit and passes on the fix.

The harness is upstream t3code's, with its browser target replaced by the desktop app, so an
upstream scenario ports through `scripts/upstream-sync.ts` like any other file. Keep it that way:
a scenario that came from upstream changes only where Onicode's app differs.

Scenarios live in [`scripts/scenarios`](../../scripts/scenarios) as `*.e2e.ts`. e2e is the
runner; its docs ship in `scripts/node_modules/e2e/docs` and `e2e guide` prints them.

## Write one

```ts
import { expect } from "e2e";

import { test, WEB } from "./onicode.ts";

test(
  "Tab moves from the add-project Git URL field to its submit button",
  WEB,
  async ({ onicode, app, screen, browser }) => {
    await onicode.project("demo");
    await onicode.open();

    await screen.getByRole("button", "Add project").tap();
    await screen.getByText("Git URL").tap();
    await screen.getByRole("combobox").fill("https://github.com/aislopware/onicode.git");
    const submit = screen.getByRole("button", "Continue (Enter)");
    await expect(submit).toBeEnabled();

    await browser.keyboard.press("Tab");
    await app.screenshot("after-tab");
    await expect(submit).toBeFocused();
  },
);
```

`screen`, `browser`, `app`, and `expect` are e2e's own. [`onicode.ts`](../../scripts/scenarios/onicode.ts)
adds one fixture for what e2e cannot know about Onicode:

- `onicode.project(title, files)` creates a git repository and adds it as a project.
- `onicode.thread({ project, title, messages })` starts a thread and plays one replayed turn per
  message before returning, so the thread has real history. `model` picks the recording. It
  returns the thread, which `onicode.open` takes.
- `onicode.open(path)` opens the app at a path. `onicode.open(thread)` opens it on a thread.

`WEB` marks a scenario for the desktop window. The name is upstream's: the window is a web page to
e2e, which attaches to it over the Chrome DevTools Protocol.

Each scenario's projects are removed when it ends, and the window's stored state (local storage,
IndexedDB, zoom) is emptied before the next one starts, so it begins from an empty sidebar and a
fresh client. What a scenario changes on the server or in the desktop shell (a setting, a
keybinding, a saved environment) it puts back itself. One that fails before it does can fail the
scenarios after it, so read a red run from its first failure.

State is created through the app's own CLI, so it is real events and works on a base commit
whose schema differs. New threads default to the replay agent's `showcase` recording: type a
message and send it. To play something else, add a recording to the
[replay agent](development.md#replay-provider).

Write one test per file: a proof runs the file and needs exactly one test in it.

Take an `app.screenshot("label")` where the bug shows. The proof pairs base and head captures by
label. e2e adds a capture at the point of failure itself.

Write only `expect` assertions for the thing the bug breaks. A proof counts a failed assertion as
the bug. A control that cannot be found, or an app that does not start, counts as a scenario that
could not run.

Keep `agent.act` and `agent.assert` out of scenarios: they call a model, and no model is
configured.

### The desktop shell

A scenario about what the window's page cannot show imports `test` from
[`desktop.ts`](../../scripts/scenarios/desktop.ts) and takes `desktop`, which talks to Electron's
main process:

```ts
import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test("Archive a thread from its context menu", WEB, async ({ onicode, desktop, screen }) => {
  const project = await onicode.project("demo");
  const thread = await onicode.thread({ project, title: "Old work", messages: ["first"] });
  await onicode.open(thread);

  const row = screen.getByRole("navigation", "Threads").getByTestId("sidebar-row-card");
  await desktop.chooseFromContextMenu(row, "Archive thread");
  await expect(row).toBeHidden();
});
```

- **Context menus are native.** Thread, project and file menus never reach the page.
  `desktop.chooseFromContextMenu(target, ...labels)` right-clicks and picks an item by its label
  path; `desktop.contextMenu()` returns the open menu's items.
- **So are message boxes and the folder picker.** The stack answers a message box with its first
  button and records it for `desktop.takeMessageBoxes()`. `desktop.answerFolderPicker(path)` sets
  what the next folder picker returns; without it the picker cancels.
- **The application menu** is `desktop.applicationMenu()` and
  `desktop.chooseApplicationMenuItem(...labels)`. Its first menu is the app's name on macOS and
  `File` elsewhere.
- **The window** is `desktop.window()`: title, zoom, full screen, the native theme source.
- **Shortcuts use the platform's mod key.** Press `ControlOrMeta+k`.
- **The clipboard is the machine's own.** A scenario that copies replaces what the person running
  it had copied.
- **A screenshot hangs while a Browser panel webview shows a page.** Take it before the page loads
  or after the panel closes; the video still records.
- **Stop then send.** The send button ignores clicks for half a second after a turn stops, so
  that a click meant for Stop does not send. Send the next message with Enter.

### Getting to the screen

Most scenarios that could not run stopped on the way to the screen the bug is on, not on the
bug. Three rules avoid that:

- **Open by link, not by sidebar.** `await onicode.open(thread)` lands on the thread
  ([`thread-opens-by-link.e2e.ts`](../../scripts/scenarios/thread-opens-by-link.e2e.ts)), and
  `await onicode.open("/settings/general")` on a settings page. A sidebar row is a button that covers
  its own title, so a tap on `getByText("Earlier work")` is intercepted. When the sidebar is what
  the scenario is about, target the row's button: `screen.getByRole("button", "Earlier work")`.
- **Navigation is by route.** e2e opens `http://127.0.0.1:<port>/<route>`; the stack's launcher
  answers with a page that sends the window to the same route inside the app. `app.baseUrl` also
  reaches the server's own paths (`/api`, `/mcp`, `/.well-known`, `/attachments`).
- **Seed before opening.** Create projects and threads first, then call `onicode.open`. With no
  project the app opens its first-run page instead of the one the scenario expects.

The app starts with `PATH`, `HOME`, the temp directory and what a desktop session needs
(`DISPLAY`, `XDG_*` and the like) in its environment, whatever else the shell that ran the
scenario has set. Name what it needs in `ONICODE_SCENARIO_INHERIT_ENV`: exact
names or prefixes ending in `*`, separated by commas.

```sh
ONICODE_SCENARIO_INHERIT_ENV='SHELL,MY_FIXTURE_*' vp run scenarios:prove scripts/scenarios/name.e2e.ts --base main
```

## Run

```sh
vp run scenarios                                                  # all of them
vp run scenarios --target desktop                                 # the desktop suite
vp run scenarios --target desktop scenarios/replay-turn-streams.e2e.ts   # one file, from scripts/
```

e2e starts the stack through [`start-app.ts`](../../scripts/scenarios/start-app.ts), runs the
tests, and stops it. The stack is `vp run dev` on the throwaway home `.onicode/scenarios`: the
renderer's Vite server, the server, and a dev Electron window that the scenarios drive. Real
providers are switched off in that home, the agents' own config directories point at empty ones,
and the window keeps its Chromium profile there too, so nothing reads or writes the developer's
Onicode. Results land in `scripts/.e2e`: `report.json`, `summary.md`, and per test a `trace.md`,
its screenshots, and a video. Flags go straight after the script name, without `--`.

The window opens on your screen and takes the focus while a run lasts. e2e downloads no browser
for the desktop target, but each scenario's video needs Playwright's ffmpeg (a few MB). Fetch it
once per machine, from `scripts` after `vp i`:

```sh
node "$(realpath node_modules/@e2e-dev/web)/../../playwright-core/cli.js" install ffmpeg
```

To iterate on one scenario without paying for the stack each time, leave a stack running and
point runs at it:

```sh
node scripts/scenarios/start-app.ts desktop "$PWD/.onicode/scenarios" 127.0.0.1 47811   # terminal 1
ONICODE_SCENARIO_APP_URL=http://127.0.0.1:47811 vp run scenarios --target desktop scenarios/name.e2e.ts
```

State then carries over between runs: a scenario that failed halfway can leave a setting or a
saved environment behind. Restart the stack when a result looks wrong.

`e2e run --help` lists the rest: `--grep`, `--watch`, `--last-failed`, `--changed-since`.

Run one at a time per checkout: a run recreates the checkout's scenario Onicode home and its `.e2e`
output, and a starting dev window closes the checkout's other dev windows. That includes a
`vp run dev` of your own in the same checkout. Use a second worktree for a second run.

## Prove a fix

```sh
vp run scenarios:prove scripts/scenarios/add-project-tab-reaches-submit.e2e.ts \
  --base e509db3a2f^ --head e509db3a2f --upload-command "ghdrop upload"
```

[`prove-scenario.ts`](../../scripts/prove-scenario.ts) runs `e2e run` for that scenario on both
commits and compares the two reports. It prints, and writes to `scripts/.e2e/prove/proof.json`,
`failsOnBase`, `passesOnHead`, `baseSha`, `headSha`, `verdict`, each side's `steps`, `captures`,
`videos`, and `markdown`: a table with base and head captures side by side, each labelled with
its commit, ready for a pull request description. `--upload-command` names a command that takes
the capture paths and prints one URL per line. `--head` defaults to `HEAD`.

It exits 0 only when the scenario fails on base and passes on head, and 1 when it ran and did not
prove the fix. A scenario that passes on base does not reproduce the bug, and the verdict says so.

Two more codes mean there is no verdict on the behavior, and `couldNotRun` in the proof says
which:

- **3, `scenario`:** the app ran and the scenario stopped before its assertion, usually on a
  control it did not find. Read that side's `message` and `steps`, fix the scenario, and run it
  again.
- **2, `app`:** the app did not start or no test ran: a bad revision, a failed install, a port in
  use. `scripts/.e2e/app.log` has the app's output.

Each side's result is also written as it ends, to `scripts/.e2e/prove/base.json` and
`head.json`, so a run that is cut short keeps the side it finished.

It runs the scenario on one target: iOS when the file imports `oni-mobile.ts`, desktop otherwise.
`--target desktop` or `--target ios` overrides that.

The scenario and e2e always come from your checkout. Only the app comes from the commit under
test: a temporary worktree beside your checkout, installed with `vp i` and removed afterwards (one
left by a run that was killed is replaced). A
commit that is your clean `HEAD` runs in place. So a new scenario can prove a fix against a base
that predates it, as long as the base has the desktop target's two hooks in the dev app
(`ONICODE_DESKTOP_INSPECT_PORT` and `ONICODE_DESKTOP_APP_DATA_DIR`). A proof refuses an older
base before starting it.

## iOS

An iOS scenario imports from [`oni-mobile.ts`](../../scripts/scenarios/oni-mobile.ts) and passes
`IOS` where a desktop one passes `WEB`:

```ts
import { expect } from "e2e";

import { IOS, test } from "./oni-mobile.ts";

test(
  "The iOS app lists a seeded thread and opens its history",
  IOS,
  async ({ onicode, app, screen }) => {
    const project = await onicode.project("demo");
    await onicode.thread({ project, title: "Earlier work", messages: ["first", "second"] });
    await onicode.open();

    const thread = screen.getByText("Earlier work", { exact: false }).first();
    await expect(thread).toBeVisible({ timeout: 120_000 });
    await thread.tap();
    await expect(screen.getByText("second fixture turn complete", { exact: false })).toBeVisible();
  },
);
```

`onicode.project` and `onicode.thread` are the same. The run fetches or builds the dev client for the
checkout's native fingerprint before the first test; `onicode.open()` installs it, loads it from
Metro, and pairs it with the scenario's server. `device` is
e2e's [device fixture](https://e2e.com/docs/mobile#the-device-fixture) for system actions.

The run drives a booted simulator on this machine; boot one first, and name it with
`E2E_IOS_DEVICE="iPhone 18 Pro"` when several are booted. The first `onicode.open()` of a run
removes the dev client from that simulator before installing it, so whatever the app held there
is gone. A desktop-only run touches no device. The server and Metro listen on ports 13990 and
8190; set `ONICODE_SCENARIO_SERVER_PORT` and `ONICODE_SCENARIO_METRO_PORT` when another run on the
machine holds them. `e2e run --target desktop` or `--target ios` runs one side.

[`apps/mobile/e2e`](../../apps/mobile/e2e) is a separate suite: whole mobile flows on a local
simulator or emulator, with its own stack. Scenarios keep the iOS target so that upstream's mobile
scenarios port, and so that `scenarios:prove` can run one on two commits.

`scenarios:prove` works the same for an iOS scenario. Each commit gets the dev client built for
its own native code.

## Other tiers

- **Android.** Add a target with `mobile({ platform: "android" })`
  and open the dev client with the Android form of the Expo link. Not built.
- **Live provider.** Point the default model in `start-app.ts` at a real provider's cheapest
  model. Assertions on exact reply text would not hold.

## In CI

[`scenarios.yml`](../../.github/workflows/scenarios.yml) runs the desktop suite on Linux under
xvfb when a pull request changes a scenario, the harness, or what the harness starts the app
through, and by hand from the Actions tab. It is not part of the `Check` gate: an app change that
breaks a scenario does not fail its own pull request. Run the suite locally before a change to a
surface the scenarios cover. The iOS target does not run in CI.

## What a scenario cannot cover

- Android.
- What only the operating system draws: the Dock, the tray, notifications, a real file picker.
  The stack replaces native menus and dialogs with recorders, so a scenario sees what the app
  asked for, not what the system showed.
- Updates. The dev app has no update feed.
- Provider-specific behavior. The replay agent speaks generic ACP.
- State no command can create, such as a run stopped by a usage limit.
- Timing. Replay streaming is simulated, so a scenario cannot show a performance regression.
