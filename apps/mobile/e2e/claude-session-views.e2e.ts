import { expect } from "e2e";

import { test } from "./claudeThread.ts";

// Golden paths through the real server, Claude session and app. The decoding, mapping and
// error cases live in ClaudeSessionSettings.test.ts and ClaudeAdapterV2.test.ts.

test("the context ring opens Claude's breakdown and agrees with its window", async ({
  claudeThread: _thread,
  screen,
}) => {
  await screen.getByRole("button", /of context used/).tap();
  await expect(screen.getByText("System prompt")).toBeVisible();
  const summary = await screen.getByText(/ of [\d.]+[KM]? tokens \(\d+%\)$/).textContent();
  const window = / of ([\d.]+[KM]?) tokens/.exec(summary ?? "")?.[1];
  expect(window).toBeDefined();
  await screen.getByRole("button", "Close").tap();
  await expect(screen.getByRole("button", new RegExp(`/ ${window} tokens$`))).toBeVisible();
});

const VIEWS = [
  { command: "status", title: "Status", loaded: "Version" },
  { command: "agents", title: "Agents", loaded: "general-purpose" },
  { command: "skills", title: "Skills", loaded: /tokens$|^No skills are installed\.$/ },
  { command: "memory", title: "Memory", loaded: "Memory files" },
  { command: "output-style", title: "Output style", loaded: "Current" },
  { command: "hooks", title: "Hooks", loaded: /^No hooks are configured\.$|settings/ },
  { command: "permissions", title: "Permissions", loaded: "Allow" },
  { command: "plugin", title: "Plugins", loaded: "Marketplaces" },
  { command: "mcp", title: "MCP servers", loaded: /tools?$|^No MCP servers/ },
] as const;

for (const view of VIEWS) {
  test(`/${view.command} opens ${view.title} from the composer`, async ({
    claudeThread: _thread,
    screen,
  }) => {
    await screen.getByRole("textbox").fill(`/${view.command}`);
    await screen.getByRole("button", new RegExp(`/${view.command},`)).tap();
    await expect(screen.getByText(view.title)).toBeVisible();
    await expect(screen.getByText(view.loaded).first()).toBeVisible();
    await screen.getByRole("button", "Close").tap();
  });
}

test("a memory file opens read-only with its contents", async ({
  claudeThread: _thread,
  screen,
}) => {
  await screen.getByRole("textbox").fill("/memory");
  await screen.getByRole("button", /\/memory,/).tap();
  await screen.getByRole("button", /^Project instructions/).tap();
  await expect(screen.getByText(/# E2E scratch/)).toBeVisible();
  await expect(screen.getByRole("button", "Save")).toBeHidden();
});
