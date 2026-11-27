import { expect } from "e2e";

import { test, WEB } from "./desktop.ts";

test(
  "A thread started with only a terminal excerpt gets a title in the app's language",
  WEB,
  async ({ onicode, desktop, app, browser, screen }) => {
    await onicode.project("demo");
    await onicode.open("/settings/general");
    await screen.getByLabel("Language", { exact: true }).tap();
    await screen.getByText("日本語", { exact: true }).tap();
    await screen.getByLabel("言語", { exact: true }).waitFor();

    await onicode.open();
    const toggle = screen.getByRole("button", "ターミナルドロワーの切り替え");
    await toggle.tap();
    const input = browser.locator('[aria-label="ターミナルの入力"][data-workspace-focus-default]');
    await input.waitFor();
    await input.focus();
    await browser.keyboard.type("printf 'first\\nsecond\\n'\n");
    // The desktop app shows the terminal's context menu natively, and the terminal takes
    // Control+Shift+A as input, so the menu selects the output.
    const canvas = browser.locator("canvas.cursor-text");
    await desktop.chooseFromContextMenu(canvas, "すべて選択");
    await desktop.chooseFromContextMenu(canvas, "チャットに追加");

    const excerpt = browser.locator('[data-testid="composer-editor"]').getByText(/行目/);
    await excerpt.waitFor();
    const expectedTitle = (await excerpt.textContent())!.trim();
    await app.screenshot("terminal-excerpt-draft-row");
    await expect(browser.locator('[data-testid="sidebar-draft-row"]')).toContainText(expectedTitle);
    await toggle.tap();
    await screen.getByRole("button", "メッセージを送信", { exact: true }).tap();
    await browser.locator("[data-thread-title-chevron]").waitFor({ state: "attached" });
    const title = screen.getByRole("navigation", "スレッドのパンくずリスト").getByRole("heading");

    await app.screenshot("terminal-excerpt-and-thread-title");
    await expect(title).toHaveText(expectedTitle);
  },
);
