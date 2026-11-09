---
name: test-oni-app
description: Test Onicode's web and desktop UI through its built-in Browser panel against isolated development state. Use for browser verification, browser pairing recovery, and test fixtures. Use test-oni-mobile for native mobile verification.
---

# Test Onicode web and desktop

Use Onicode's built-in Browser panel for verification. Follow the recovery steps
below before declaring it unavailable. Do not install or switch to another
automation system. For native mobile testing, use
[test-oni-mobile](../test-oni-mobile/SKILL.md).

## Start the app

Reuse this task's healthy dev server. Otherwise run `vp run dev` from the
repository root and retain its terminal session. Use the worktree's ignored
`.onicode` state and read the actual ports and pairing URL from the dev-runner output.
Never run against `~/.onicode/userdata` or set `VITE_HTTP_URL` or `VITE_WS_URL`.

Test with meaningful project and thread data. Read
[references/sqlite-fixtures.md](references/sqlite-fixtures.md) only when
inspecting or seeding SQLite. Stop the test server before direct fixture writes.

## Drive agent turns for free

Do not spend a paid model to see a turn render. `vp run dev` registers the
`Replay (dev)` provider, which replays recorded turns with simulated streaming
through the real adapter and orchestration. Its models are scenarios:
`showcase` covers thinking, a plan, tool calls, an approval, and markdown.
Press `Ctrl+Alt+Shift+R` for a picker (GNOME takes that key for screen
recording), call `window.onidev.replay("showcase")`
with `preview_evaluate`, or run
`node apps/server/src/bin.ts thread start --base-dir .onicode --provider devReplay --model showcase <project> "go"`.
When the test needs real provider behavior, use that provider's cheapest
model, such as Haiku for Claude or Luna for Codex.

## Use the Browser panel

Call `preview_status`. If no tab can be automated, call `preview_open`, including
when status reports `available: false` or `tabId: null`. Status can report
unavailable simply because no tab exists yet; opening a tab needs no user action.
Stop verification and explain the blocker only if the tools are absent or
`preview_open` reports unavailable or fails.

Navigate to the complete startup pairing URL once with
`preview_navigate`, then use `preview_snapshot` and Onicode's interaction tools.
If the token was consumed or expired, run `node apps/server/src/bin.ts pair`
for a fresh one. Keep using the same tab.

## Verify and retain

Exercise the affected flow and capture the state that proves it works. Keep
the server, state, and panel available while the user inspects or iterates.
An assistant turn ending is not teardown. Stop only processes you started,
using retained terminal sessions or captured PIDs.

When sharing is requested, start with `vp run dev --share` and give the user
a fresh complete pairing URL that you have not consumed. Keep other credentials
out of screenshots, commits, and replies.
