import { describe, expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import { render } from "../../test/render";
import { useCommitOnBlur } from "../hooks/useCommitOnBlur";
import { AppRouteModal, AppRouteModalClose } from "./AppRouteModal";

vi.mock("./chat/GlobalVoiceInputIndicator", () => ({ GlobalVoiceInputIndicator: () => null }));

function CommitOnBlurField({ onCommit }: { onCommit: (next: string) => void }) {
  return <input aria-label="Instructions" {...useCommitOnBlur("", onCommit)} />;
}

describe("AppRouteModal", () => {
  it("commits a focused field when Esc closes it", async () => {
    const onCommit = vi.fn();
    await using _screen = await render(
      <AppRouteModal label="Settings">
        <CommitOnBlurField onCommit={onCommit} />
      </AppRouteModal>,
      { router: "/settings/general" },
    );

    await page.getByRole("textbox", { name: "Instructions" }).fill("Keep titles short");
    await userEvent.keyboard("{Escape}");

    expect(onCommit).toHaveBeenCalledWith("Keep titles short");
  });

  it("shows again when a jump made while it closed comes back to Settings", async () => {
    await using screen = await render(
      <AppRouteModal label="Settings">
        <AppRouteModalClose />
      </AppRouteModal>,
      { router: "/settings/general" },
    );
    const router = screen.router!;
    const settings = page.getByRole("dialog", { name: "Settings" });

    // A DOM click starts the jump in the same task as the close, ahead of the exit animation.
    (page.getByRole("button", { name: "Close" }).element() as HTMLElement).click();
    await router.navigate({ href: "/projects/demo" });
    await expect.element(settings).not.toBeInTheDocument();

    // The app redirects a project page to its Settings page; here the test navigates there.
    await router.navigate({ href: "/settings/projects" });
    await expect.element(settings).toBeVisible();
  });
});
