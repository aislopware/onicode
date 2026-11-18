import { describe, expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";
import type { ReactNode } from "react";

import { render } from "../../test/render";
import { dispatchCommandPalette } from "../commandPaletteStore";
import { useCommitOnBlur } from "../hooks/useCommitOnBlur";
import { startWorkspaceFocus } from "../lib/workspaceFocus";
import { AppRouteModal, AppRouteModalClose } from "./AppRouteModal";

vi.mock("./chat/GlobalVoiceInputIndicator", () => ({ GlobalVoiceInputIndicator: () => null }));

function CommitOnBlurField({ onCommit }: { onCommit: (next: string) => void }) {
  return <input aria-label="Instructions" {...useCommitOnBlur("", onCommit)} />;
}

/** The thread behind the modal: inert while it is open, with a side chat repeating its areas. */
function Thread({ background }: { background: boolean }) {
  return (
    <div inert={background}>
      <div data-workspace-area="composer">
        <textarea aria-label="Main message" />
      </div>
      <div data-side-chat-pane="side">
        <div data-workspace-area="composer">
          <textarea aria-label="Side message" />
        </div>
      </div>
    </div>
  );
}

function Home() {
  return (
    <main data-workspace-area="home">
      <button type="button">Add project</button>
    </main>
  );
}

const usageModal = (
  <AppRouteModal label="Usage">
    <p>Usage</p>
  </AppRouteModal>
);

async function closeWithEsc() {
  await expect.element(page.getByRole("dialog", { name: "Usage" })).toHaveFocus();
  await userEvent.keyboard("{Escape}");
  await expect.element(page.getByRole("dialog", { name: "Usage" })).not.toBeInTheDocument();
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

  it("returns focus to the opener when Esc closes it", async () => {
    const opener = <button type="button">Open usage</button>;
    await using screen = await render(opener, { router: "/" });
    const openerButton = screen.getByRole("button", { name: "Open usage" });
    (openerButton.element() as HTMLElement).focus();

    await screen.rerender(
      <>
        {opener}
        <AppRouteModal label="Usage">
          <p>Usage</p>
        </AppRouteModal>
      </>,
    );
    await expect.element(page.getByRole("dialog", { name: "Usage" })).toHaveFocus();
    await userEvent.keyboard("{Escape}");

    await expect.element(page.getByRole("dialog", { name: "Usage" })).not.toBeInTheDocument();
    await expect.element(openerButton).toHaveFocus();
  });

  it("returns focus to the side chat it opened from once the thread is usable again", async () => {
    await using screen = await render(<Thread background={false} />, { router: "/" });
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    (screen.getByRole("textbox", { name: "Side message" }).element() as HTMLElement).focus();

    await screen.rerender(
      <>
        <Thread background />
        {usageModal}
      </>,
    );
    await closeWithEsc();
    await screen.rerender(<Thread background={false} />);

    await expect.element(screen.getByRole("textbox", { name: "Side message" })).toHaveFocus();
  });

  it("returns focus to the opener's replacement when the page it opened from mounts again", async () => {
    await using screen = await render(<Home />, { router: "/" });
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    (screen.getByRole("button", { name: "Add project" }).element() as HTMLElement).focus();

    await screen.rerender(usageModal);
    await closeWithEsc();
    await screen.rerender(<Home />);

    await expect.element(screen.getByRole("button", { name: "Add project" })).toHaveFocus();
  });

  it("returns focus to the palette's opener when it mounts after the palette closed", async () => {
    using _palette = {
      [Symbol.dispose]: () => dispatchCommandPalette({ _tag: "SetOpen", open: false }),
    };
    // Base UI sends focus with nowhere to return to the page's first control, so put one first.
    const Sidebar = ({ children }: { children?: ReactNode }) => (
      <>
        <button type="button">New thread</button>
        <button type="button">Sidebar control</button>
        <div data-workspace-area="composer">
          <textarea aria-label="Message" />
        </div>
        {children}
      </>
    );
    await using screen = await render(<Sidebar />, { router: "/" });
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    (screen.getByRole("button", { name: "Sidebar control" }).element() as HTMLElement).focus();
    dispatchCommandPalette({ _tag: "SetOpen", open: true });
    await screen.rerender(
      <Sidebar>
        <div data-command-palette="true">
          <input aria-label="Search commands" />
        </div>
      </Sidebar>,
    );
    (screen.getByRole("textbox", { name: "Search commands" }).element() as HTMLElement).focus();

    // "Open settings" closes the palette, then the route mounts once it has loaded.
    dispatchCommandPalette({ _tag: "SetOpen", open: false });
    await screen.rerender(<Sidebar />);
    // Losing the palette's input sends focus to the composer while the route loads.
    await expect.element(screen.getByRole("textbox", { name: "Message" })).toHaveFocus();
    await screen.rerender(<Sidebar>{usageModal}</Sidebar>);
    await closeWithEsc();

    await expect.element(screen.getByRole("button", { name: "Sidebar control" })).toHaveFocus();
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
