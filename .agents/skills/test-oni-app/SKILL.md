---
name: test-oni-app
description: Test Onicode's desktop UI in a dev Electron window against isolated development state, driven over the Chrome DevTools Protocol. Use for desktop verification and test fixtures. Use test-oni-mobile for native mobile verification.
---

# Test Onicode desktop

The renderer only runs inside Electron, so verify UI changes in the dev desktop
app. For native mobile testing, use
[test-oni-mobile](../test-oni-mobile/SKILL.md).

## Start the app

Reuse this task's healthy dev app. Otherwise run `vp run dev` from the
repository root in the background and retain its terminal session. It builds
the server, starts the renderer's Vite server, and opens a dev Electron window
that starts its own server. In a worktree, state lives in the worktree's ignored
`.onicode`. Read the real ports from the `[dev-runner]` line:
`serverPort`, `webPort`, and `cdpPort`. Never run against `~/.onicode/userdata`.

The window signs in to its own server by itself; there is no pairing step.

Test with meaningful project and thread data. Read
[references/sqlite-fixtures.md](references/sqlite-fixtures.md) only when
inspecting or seeding SQLite. Stop the dev app before direct fixture writes.

## Drive the window

The window serves the Chrome DevTools Protocol on `http://127.0.0.1:<cdpPort>`.
Attach to the page whose URL starts with `onicode-dev://app`, not the splash or
preview targets.

- With `agent-browser` installed: `agent-browser --session oni-dev --cdp <cdpPort>`,
  then `snapshot`, `click`, `fill`, `screenshot`.
- Otherwise use Playwright, which the repo already installs: from `apps/desktop`,
  `chromium.connectOverCDP("http://127.0.0.1:<cdpPort>")` from `playwright-core`,
  then pick the `onicode-dev://app` page from `browser.contexts()[0].pages()`.

Do not close the browser over CDP; disconnect instead, or the dev app quits.

## Drive agent turns for free

Do not spend a paid model to see a turn render. The dev server registers the
`Replay (dev)` provider, which replays recorded turns with simulated streaming
through the real adapter and orchestration. Its models are scenarios:
`showcase` covers thinking, a plan, tool calls, an approval, and markdown.
Press `Ctrl+Alt+Shift+R` for a picker (GNOME takes that key for screen
recording), evaluate `window.onidev.replay("showcase")` in the page, or run
`node apps/server/src/bin.ts thread start --base-dir .onicode --provider devReplay --model showcase <project> "go"`.
When the test needs real provider behavior, use that provider's cheapest
model, such as Haiku for Claude or Luna for Codex.

## Verify and retain

Exercise the affected flow and capture the state that proves it works. Keep
the app and state available while the user inspects or iterates. An assistant
turn ending is not teardown. Stop only processes you started, using retained
terminal sessions or captured PIDs.
