import { describe, expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";
import type { ReactNode } from "react";

import { render, type Screen } from "../../test/render";
import { dispatchCommandPalette, handOverCommandPaletteOpener } from "../commandPaletteStore";
import { useCommitOnBlur } from "../hooks/useCommitOnBlur";
import { startWorkspaceFocus } from "../lib/workspaceFocus";
import { AppRouteModal, AppRouteModalClose } from "./AppRouteModal";
import { startMainAppLocationTracking } from "./sidebar/mainAppLocation";

vi.mock("./chat/GlobalVoiceInputIndicator", () => ({ GlobalVoiceInputIndicator: () => null }));

function CommitOnBlurField({ onCommit }: { onCommit: (next: string) => void }) {
  return <input aria-label="Instructions" {...useCommitOnBlur("", onCommit)} />;
}

/** The thread behind the modal: inert while it is open, with a side chat repeating its areas. */
function Thread({ background, sideChat = 0 }: { background: boolean; sideChat?: number }) {
  return (
    <div inert={background}>
      <input aria-label="Find in thread" />
      <div data-workspace-area="composer">
        <textarea aria-label="Main message" />
      </div>
      <div data-workspace-area="panel">
        <div key={sideChat} data-side-chat-pane="side">
          <div data-workspace-area="composer">
            <textarea aria-label="Side message" />
          </div>
        </div>
      </div>
    </div>
  );
}

function Home() {
  return (
    <main data-workspace-area="home">
      <button type="button">Start without a project</button>
      <button type="button">Add project</button>
    </main>
  );
}

function PullRequests() {
  return (
    <>
      <div data-workspace-area="pull-requests">
        <button type="button">Open pull request</button>
      </div>
      <div data-workspace-area="panel">
        <button type="button">Show file tree</button>
      </div>
    </>
  );
}

/** Base UI sends focus with nowhere to return to the page's first control, so one comes first. */
function Sidebar({ children }: { children?: ReactNode }) {
  return (
    <>
      <button type="button">New thread</button>
      <button type="button">Sidebar control</button>
      <div data-workspace-area="composer">
        <textarea aria-label="Message" />
      </div>
      {children}
    </>
  );
}

/** Opens the palette and closes it as a command does: one that navigates hands focus over. */
async function runPaletteCommand(screen: Screen, { navigates }: { navigates: boolean }) {
  dispatchCommandPalette({ _tag: "SetOpen", open: true });
  await screen.rerender(
    <Sidebar>
      <div data-command-palette="true">
        <input aria-label="Search commands" />
      </div>
    </Sidebar>,
  );
  (screen.getByRole("textbox", { name: "Search commands" }).element() as HTMLElement).focus();
  dispatchCommandPalette({ _tag: "SetOpen", open: false });
  if (navigates) handOverCommandPaletteOpener();
  await screen.rerender(<Sidebar />);
}

/** A key press ends the palette's handover, so no test inherits it. */
const endPaletteHandover = () => ({
  [Symbol.dispose]: () => document.dispatchEvent(new KeyboardEvent("keydown")),
});

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

  it.each([
    { opener: "a side chat", name: "Side message", remount: false },
    { opener: "a side chat that remounted under it", name: "Side message", remount: true },
    { opener: "a control outside any area", name: "Find in thread", remount: false },
  ])("returns focus to $opener once the thread is usable again", async ({ name, remount }) => {
    await using screen = await render(<Thread background={false} />, { router: "/" });
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    (screen.getByRole("textbox", { name }).element() as HTMLElement).focus();

    await screen.rerender(
      <>
        <Thread background sideChat={remount ? 1 : 0} />
        {usageModal}
      </>,
    );
    await closeWithEsc();
    await screen.rerender(<Thread background={false} sideChat={remount ? 1 : 0} />);

    await expect.element(screen.getByRole("textbox", { name })).toHaveFocus();
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

  it("keeps its opener's focus when it closes back onto the Pull Requests page", async () => {
    await using screen = await render(<PullRequests />, { router: "/pull-requests" });
    using _location = { [Symbol.dispose]: startMainAppLocationTracking(screen.router!) };
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    (screen.getByRole("button", { name: "Show file tree" }).element() as HTMLElement).focus();

    await screen.router!.navigate({ href: "/usage" });
    await screen.rerender(usageModal);
    await closeWithEsc();
    await vi.waitFor(() =>
      expect(screen.router!.state.resolvedLocation?.pathname).toBe("/pull-requests"),
    );
    await screen.rerender(<PullRequests />);

    await expect.element(screen.getByRole("button", { name: "Show file tree" })).toHaveFocus();
  });

  it("returns focus to the palette's opener when it mounts after the palette closed", async () => {
    using _palette = endPaletteHandover();
    await using screen = await render(<Sidebar />, { router: "/" });
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    (screen.getByRole("button", { name: "Sidebar control" }).element() as HTMLElement).focus();

    // "Open settings" closes the palette, then the route mounts once it has loaded.
    await runPaletteCommand(screen, { navigates: true });
    // Losing the palette's input sends focus to the composer while the route loads.
    await expect.element(screen.getByRole("textbox", { name: "Message" })).toHaveFocus();
    await screen.rerender(<Sidebar>{usageModal}</Sidebar>);
    await closeWithEsc();

    await expect.element(screen.getByRole("button", { name: "Sidebar control" })).toHaveFocus();
  });

  it.each([
    { after: "a palette that returned focus itself", navigates: false, elsewhere: false },
    { after: "a palette command that opened another page", navigates: true, elsewhere: true },
  ])("returns focus to its own opener after $after", async ({ navigates, elsewhere }) => {
    using _palette = endPaletteHandover();
    await using screen = await render(<Sidebar />, { router: "/" });
    using _location = { [Symbol.dispose]: startMainAppLocationTracking(screen.router!) };
    (screen.getByRole("button", { name: "Sidebar control" }).element() as HTMLElement).focus();

    await runPaletteCommand(screen, { navigates });
    if (elsewhere) await screen.router!.navigate({ href: "/threads/other" });
    // The palette or the new page focuses the composer; the app menu then opens the modal.
    (screen.getByRole("textbox", { name: "Message" }).element() as HTMLElement).focus();
    await screen.rerender(<Sidebar>{usageModal}</Sidebar>);
    await closeWithEsc();

    await expect.element(screen.getByRole("textbox", { name: "Message" })).toHaveFocus();
  });

  it("leaves focus alone when the palette opened with nothing focused", async () => {
    using _palette = endPaletteHandover();
    await using screen = await render(<Sidebar />, { router: "/" });

    await runPaletteCommand(screen, { navigates: true });
    (document.activeElement as HTMLElement | null)?.blur();
    await screen.rerender(<Sidebar>{usageModal}</Sidebar>);
    await closeWithEsc();

    expect(document.activeElement).not.toBe(
      screen.getByRole("button", { name: "New thread" }).element(),
    );
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
