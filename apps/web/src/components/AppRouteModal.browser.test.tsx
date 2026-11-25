import { redirect, useLocation } from "@tanstack/react-router";
import { describe, expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";

import appCss from "~/index.css?inline";
import { act, render, type Screen } from "../../test/render";
import { dispatchCommandPalette, handOverCommandPaletteOpener } from "../commandPaletteStore";
import { useCommitOnBlur } from "../hooks/useCommitOnBlur";
import {
  getWorkspaceFocusRequestVersion,
  requestWorkspaceFocus,
  startWorkspaceFocus,
} from "../lib/workspaceFocus";
import { AppRouteModal, AppRouteModalClose } from "./AppRouteModal";
import { startMainAppLocationTracking } from "./sidebar/mainAppLocation";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "./ui/menu";

vi.mock("./chat/GlobalVoiceInputIndicator", () => ({ GlobalVoiceInputIndicator: () => null }));
// Passed through, so a test can see a deferred restore that would land after it ends.
vi.mock("../lib/workspaceFocus", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/workspaceFocus")>();
  return { ...actual, requestWorkspaceFocus: vi.fn(actual.requestWorkspaceFocus) };
});

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

function PullRequests({ panel = true }: { panel?: boolean }) {
  return (
    <>
      <div data-workspace-area="pull-requests">
        <button type="button">Open pull request</button>
      </div>
      {panel ? (
        <div data-workspace-area="panel">
          <button type="button">Show file tree</button>
        </div>
      ) : null}
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

/**
 * Opens the palette and closes it as a command does: one that navigates hands focus over once
 * the palette has finished closing, and `meanwhile` runs before that.
 */
async function runPaletteCommand(
  screen: Screen,
  { navigates, meanwhile }: { navigates: boolean; meanwhile?: () => unknown },
) {
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
  await meanwhile?.();
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

/** The app's styles give the modal the exit animation that Base UI waits for. */
function appStyles(): Disposable {
  const style = document.createElement("style");
  style.textContent = appCss;
  document.head.append(style);
  return { [Symbol.dispose]: () => style.remove() };
}

/**
 * Closes with Esc or a press on the backdrop, then presses a key in the same task, so the key
 * lands inside the exit animation however slow the run is.
 */
function closeThenPress(key: string, target: Element, closedBy: "Esc" | "backdrop" = "Esc") {
  const dialog = page.getByRole("dialog", { name: "Usage" }).element();
  if (closedBy === "Esc") {
    dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  } else {
    const backdrop = dialog.parentElement!;
    backdrop.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    backdrop.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  }
  target.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
}

/** Moves focus as a workspace shortcut does: it takes the key at the window. */
function shortcutFocuses(key: string, target: HTMLElement): Disposable {
  const moveOn = (event: KeyboardEvent) => {
    if (event.key !== key) return;
    event.stopPropagation();
    target.focus();
  };
  window.addEventListener("keydown", moveOn, true);
  return { [Symbol.dispose]: () => window.removeEventListener("keydown", moveOn, true) };
}

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

  // regression: pingdotgg/t3code#42075. Navigation: Back (mod+[) leaves the Settings route without closing the dialog.
  it("commits a focused field when the route goes back", async () => {
    const onCommit = vi.fn();
    function SettingsRoute() {
      const pathname = useLocation({ select: (location) => location.pathname });
      return pathname.startsWith("/settings") ? (
        <AppRouteModal label="Settings">
          <CommitOnBlurField onCommit={onCommit} />
        </AppRouteModal>
      ) : null;
    }
    await using screen = await render(<SettingsRoute />, { router: "/" });
    // The app redirects /settings, and the router skips onBeforeNavigate after a redirect.
    screen.router!.routeTree.update({
      beforeLoad: ({ location }: { location: { pathname: string } }) => {
        if (location.pathname === "/settings") throw redirect({ to: "/settings/general" });
      },
    });
    await screen.router!.navigate({ to: "/settings" });

    await page.getByRole("textbox", { name: "Instructions" }).fill("Keep titles short");
    screen.router!.history.back();

    await expect.element(page.getByRole("dialog", { name: "Settings" })).not.toBeInTheDocument();
    expect(onCommit).toHaveBeenCalledWith("Keep titles short");
  });

  it.each([
    { path: "/settings/general", from: { machine: "a" }, to: { machine: "b" } },
    // The provider panel is keyed by its instance too.
    {
      path: "/settings/providers",
      from: { machine: "a", instanceId: "claude" },
      to: { machine: "a", instanceId: "codex" },
    },
  ])("commits a focused field when Back changes $path from $from", async ({ path, from, to }) => {
    const onCommit = vi.fn();
    // Settings content is keyed by its search, so the same page remounts under another one.
    function SettingsRoute() {
      const searchStr = useLocation({ select: (location) => location.searchStr });
      return (
        <AppRouteModal label="Settings">
          <CommitOnBlurField key={searchStr} onCommit={onCommit} />
        </AppRouteModal>
      );
    }
    await using screen = await render(<SettingsRoute />, {
      router: `${path}?${new URLSearchParams(from)}`,
    });
    screen.router!.history.push(`${path}?${new URLSearchParams(to)}`);
    await vi.waitFor(() => expect(screen.router!.state.resolvedLocation?.search).toEqual(to));

    const field = page.getByRole("textbox", { name: "Instructions" });
    await field.fill("Keep titles short");
    screen.router!.history.back();

    await expect.element(field).toHaveValue("");
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
    { key: "Tab", shortcut: false, when: "while it was closing" },
    { key: "F6", shortcut: true, when: "while it was closing" },
    { key: "F6", shortcut: true, when: "once it closed" },
  ])("keeps focus that $key moved on $when", async ({ key, shortcut, when }) => {
    using _styles = appStyles();
    const view = (background: boolean, modal: ReactNode = null) => (
      <>
        <button type="button">Elsewhere</button>
        <Thread background={background} />
        {modal}
      </>
    );
    vi.mocked(requestWorkspaceFocus).mockClear();
    await using screen = await render(view(false), { router: "/" });
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    const elsewhere = screen.getByRole("button", { name: "Elsewhere" }).element() as HTMLElement;
    using _shortcut = shortcutFocuses("F6", elsewhere);
    (screen.getByRole("textbox", { name: "Main message" }).element() as HTMLElement).focus();

    await screen.rerender(view(true, usageModal));
    await expect.element(page.getByRole("dialog", { name: "Usage" })).toHaveFocus();
    if (when === "once it closed") {
      await closeWithEsc();
      // The route has not closed yet, so the thread is still inert.
      window.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
    } else {
      closeThenPress(key, elsewhere);
      if (!shortcut) elsewhere.focus();
      await expect.element(page.getByRole("dialog", { name: "Usage" })).not.toBeInTheDocument();
    }
    await screen.rerender(view(false));

    expect(requestWorkspaceFocus).not.toHaveBeenCalled();
    await expect.element(screen.getByRole("button", { name: "Elsewhere" })).toHaveFocus();
  });

  it.each(["while it closed", "once it closed"] as const)(
    "still returns focus after a key %s that moved nothing",
    async (when) => {
      using _styles = appStyles();
      const view = (background: boolean, modal: ReactNode = null) => (
        <>
          <Thread background={background} />
          {modal}
        </>
      );
      await using screen = await render(view(false), { router: "/" });
      using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
      (screen.getByRole("textbox", { name: "Main message" }).element() as HTMLElement).focus();

      await screen.rerender(view(true, usageModal));
      await expect.element(page.getByRole("dialog", { name: "Usage" })).toHaveFocus();
      // With the thread inert and the sidebar collapsed, F6 has nowhere to go.
      if (when === "while it closed") closeThenPress("F6", document.body);
      else await closeWithEsc();
      await expect.element(page.getByRole("dialog", { name: "Usage" })).not.toBeInTheDocument();
      // The route has not closed yet, so the thread is still inert.
      if (when === "once it closed")
        document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "F6", bubbles: true }));
      await screen.rerender(view(false));

      await expect.element(screen.getByRole("textbox", { name: "Main message" })).toHaveFocus();
    },
  );

  it("returns to what Settings opened from after Usage replaced it", async () => {
    const opener = <button type="button">Open settings</button>;
    await using screen = await render(opener, { router: "/" });
    (screen.getByRole("button", { name: "Open settings" }).element() as HTMLElement).focus();

    await screen.rerender(
      <>
        {opener}
        <AppRouteModal key="settings" label="Settings">
          <p>Settings</p>
        </AppRouteModal>
      </>,
    );
    await expect.element(page.getByRole("dialog", { name: "Settings" })).toHaveFocus();
    // Its own route renders each modal, so Usage mounts as Settings unmounts.
    await screen.rerender(
      <>
        {opener}
        <AppRouteModal key="usage" label="Usage">
          <p>Usage</p>
        </AppRouteModal>
      </>,
    );
    await closeWithEsc();

    await expect.element(screen.getByRole("button", { name: "Open settings" })).toHaveFocus();
  });

  it("returns to what the user focused after Settings closed when Usage replaced it", async () => {
    // A jump made while Settings closed has left its route, so its close is skipped and it
    // stays mounted until that page loads.
    await using screen = await render(<Sidebar />, { router: "/projects/demo" });
    (screen.getByRole("button", { name: "New thread" }).element() as HTMLElement).focus();
    await screen.rerender(
      <Sidebar>
        <AppRouteModal key="settings" label="Settings">
          <p>Settings</p>
        </AppRouteModal>
      </Sidebar>,
    );
    await expect.element(page.getByRole("dialog", { name: "Settings" })).toHaveFocus();
    await userEvent.keyboard("{Escape}");
    await expect.element(screen.getByRole("button", { name: "New thread" })).toHaveFocus();
    (screen.getByRole("button", { name: "Sidebar control" }).element() as HTMLElement).focus();

    await screen.rerender(
      <Sidebar>
        <AppRouteModal key="usage" label="Usage">
          <p>Usage</p>
        </AppRouteModal>
      </Sidebar>,
    );
    await closeWithEsc();

    await expect.element(screen.getByRole("button", { name: "Sidebar control" })).toHaveFocus();
  });

  it("returns to what Settings opened from when Usage replaced it from a Settings menu", async () => {
    const opener = <button type="button">Open settings</button>;
    await using screen = await render(opener, { router: "/" });
    (screen.getByRole("button", { name: "Open settings" }).element() as HTMLElement).focus();

    await screen.rerender(
      <>
        {opener}
        <AppRouteModal key="settings" label="Settings">
          {/* A select's options are portaled outside the modal. */}
          {createPortal(<button type="button">Wide</button>, document.body)}
        </AppRouteModal>
      </>,
    );
    await expect.element(page.getByRole("dialog", { name: "Settings" })).toHaveFocus();
    (
      page.getByRole("button", { name: "Wide", includeHidden: true }).element() as HTMLElement
    ).focus();
    await expect
      .element(page.getByRole("button", { name: "Wide", includeHidden: true }))
      .toHaveFocus();
    await screen.rerender(
      <>
        {opener}
        <AppRouteModal key="usage" label="Usage">
          <p>Usage</p>
        </AppRouteModal>
      </>,
    );
    await closeWithEsc();

    await expect.element(screen.getByRole("button", { name: "Open settings" })).toHaveFocus();
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

  it("keeps a sidebar opener's focus when it closes back onto the Pull Requests page", async () => {
    await using screen = await render(
      <Sidebar>
        <PullRequests />
      </Sidebar>,
      { router: "/pull-requests" },
    );
    using _location = { [Symbol.dispose]: startMainAppLocationTracking(screen.router!) };
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    (screen.getByRole("button", { name: "Sidebar control" }).element() as HTMLElement).focus();

    await screen.router!.navigate({ href: "/usage" });
    await screen.rerender(<Sidebar>{usageModal}</Sidebar>);
    const requests = getWorkspaceFocusRequestVersion();
    await closeWithEsc();
    await vi.waitFor(() =>
      expect(screen.router!.state.resolvedLocation?.pathname).toBe("/pull-requests"),
    );

    await expect.element(screen.getByRole("button", { name: "Sidebar control" })).toHaveFocus();
    // A request for the list would take focus once the page renders.
    expect(getWorkspaceFocusRequestVersion()).toBe(requests);
  });

  it.each(["Esc", "backdrop"] as const)(
    "leaves Pull Requests focus where F6 moved it while it was closing by %s",
    async (closedBy) => {
      using _styles = appStyles();
      await using screen = await render(
        <Sidebar>
          <PullRequests />
        </Sidebar>,
        { router: "/pull-requests" },
      );
      using _location = { [Symbol.dispose]: startMainAppLocationTracking(screen.router!) };
      using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
      const control = screen
        .getByRole("button", { name: "Sidebar control" })
        .element() as HTMLElement;
      using _shortcut = shortcutFocuses("F6", control);
      (screen.getByRole("button", { name: "Show file tree" }).element() as HTMLElement).focus();

      await screen.router!.navigate({ href: "/usage" });
      await screen.rerender(<Sidebar>{usageModal}</Sidebar>);
      await expect.element(page.getByRole("dialog", { name: "Usage" })).toHaveFocus();
      const requests = getWorkspaceFocusRequestVersion();
      closeThenPress("F6", document.body, closedBy);
      await vi.waitFor(() =>
        expect(screen.router!.state.resolvedLocation?.pathname).toBe("/pull-requests"),
      );
      await screen.rerender(
        <Sidebar>
          <PullRequests />
        </Sidebar>,
      );

      await expect.element(screen.getByRole("button", { name: "Sidebar control" })).toHaveFocus();
      // A request for the list would take focus once the page renders.
      expect(getWorkspaceFocusRequestVersion()).toBe(requests);
    },
  );

  it.each([
    { opener: "outside any area is gone", name: "Maximize", panelReturns: true },
    { opener: "in a panel that did not come back", name: "Show file tree", panelReturns: false },
  ])("focuses the Pull Requests list when its opener $opener", async ({ name, panelReturns }) => {
    const pullRequests = (panel: boolean) => (
      <>
        <button type="button">Maximize</button>
        <PullRequests panel={panel} />
      </>
    );
    await using screen = await render(pullRequests(true), { router: "/pull-requests" });
    using _location = { [Symbol.dispose]: startMainAppLocationTracking(screen.router!) };
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };
    (screen.getByRole("button", { name }).element() as HTMLElement).focus();

    await screen.router!.navigate({ href: "/usage" });
    await screen.rerender(usageModal);
    await closeWithEsc();
    await vi.waitFor(() =>
      expect(screen.router!.state.resolvedLocation?.pathname).toBe("/pull-requests"),
    );
    await screen.rerender(pullRequests(panelReturns));

    await expect.element(screen.getByRole("button", { name: "Open pull request" })).toHaveFocus();
  });

  it("focuses the Pull Requests list when Usage leads there without closing", async () => {
    await using screen = await render(usageModal, { router: "/usage" });
    using _location = { [Symbol.dispose]: startMainAppLocationTracking(screen.router!) };
    using _focus = { [Symbol.dispose]: startWorkspaceFocus(screen.router!) };

    await screen.router!.navigate({ href: "/pull-requests" });
    await screen.rerender(<PullRequests />);

    await expect.element(screen.getByRole("button", { name: "Open pull request" })).toHaveFocus();
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
    { after: "a palette that returned focus itself", navigates: false },
    {
      after: "a palette command whose page resolved before the palette finished closing",
      navigates: true,
      meanwhile: (screen: Screen) => screen.router!.navigate({ href: "/threads/other" }),
    },
    {
      after: "a key pressed before the palette finished closing",
      navigates: true,
      meanwhile: () => document.dispatchEvent(new KeyboardEvent("keydown", { key: "a" })),
    },
    {
      after: "a workspace shortcut pressed before the palette finished closing",
      navigates: true,
      // Shortcuts such as F6 take the key at the window, so the document never sees it.
      meanwhile: () => window.dispatchEvent(new KeyboardEvent("keydown", { key: "F6" })),
    },
    {
      after: "a palette command that moved workspace focus",
      navigates: true,
      meanwhile: () => requestWorkspaceFocus({ area: "composer" }),
    },
  ])("returns focus to its own opener after $after", async ({ navigates, meanwhile }) => {
    using _palette = endPaletteHandover();
    await using screen = await render(<Sidebar />, { router: "/" });
    using _location = { [Symbol.dispose]: startMainAppLocationTracking(screen.router!) };
    (screen.getByRole("button", { name: "Sidebar control" }).element() as HTMLElement).focus();

    await runPaletteCommand(screen, { navigates, meanwhile: () => meanwhile?.(screen) });
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

  it("stays open on the Settings section Back reached while it closed", async () => {
    using _styles = appStyles();
    await using screen = await render(
      <AppRouteModal label="Settings">
        <p>Settings</p>
      </AppRouteModal>,
      { router: "/settings/general" },
    );
    const router = screen.router!;
    await router.navigate({ href: "/settings/appearance" });
    const settings = page.getByRole("dialog", { name: "Settings" });
    await expect.element(settings).toHaveFocus();

    settings
      .element()
      .dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    router.history.back();
    await vi.waitFor(() =>
      expect(router.state.resolvedLocation?.pathname).toBe("/settings/general"),
    );
    await expect.element(settings).toHaveAttribute("data-closed");

    await expect.element(settings).toHaveAttribute("data-open");
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

  it("stays open when a held Esc repeats after closing a nested menu", async () => {
    await using _screen = await render(
      <AppRouteModal label="Usage">
        <Menu>
          <MenuTrigger>All providers</MenuTrigger>
          <MenuPopup>
            <MenuItem>Claude</MenuItem>
          </MenuPopup>
        </Menu>
      </AppRouteModal>,
      { router: "/usage" },
    );
    const usage = page.getByRole("dialog", { name: "Usage" });
    const trigger = page.getByRole("button", { name: "All providers" });
    await expect.element(usage).toHaveFocus();

    (trigger.element() as HTMLElement).focus();
    await userEvent.keyboard("{Enter}");
    await expect.element(page.getByRole("menu")).toBeVisible();

    await userEvent.keyboard("{Escape}");
    await expect.element(page.getByRole("menu")).not.toBeInTheDocument();
    await expect.element(trigger).toHaveFocus();
    await expect.element(usage).toHaveAttribute("data-open");

    await act(() =>
      trigger.element().dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Escape",
          repeat: true,
          bubbles: true,
          cancelable: true,
        }),
      ),
    );
    await expect.element(usage).toHaveAttribute("data-open");

    await userEvent.keyboard("{Escape}");
    await expect.element(usage).not.toBeInTheDocument();
  });
});
