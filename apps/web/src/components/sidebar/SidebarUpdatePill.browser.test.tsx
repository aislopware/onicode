import type { DesktopUpdateState } from "@onicode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import "~/index.css";
import { act, fakeTimers, render, stubGlobal } from "../../../test/render";
import { AppAtomRegistryProvider, appAtomRegistry } from "../../rpc/atomRegistry";
import { desktopUpdateRestartingVersionAtom } from "../../state/desktopUpdate";
import { ToastProvider } from "../ui/toast";
import { SidebarUpdatePill } from "./SidebarUpdatePill";
import type { UpdateEverythingRow } from "./updateEverything.logic";

const mocks = vi.hoisted(() => {
  const listeners = new Set<() => void>();
  return {
    update: null as DesktopUpdateState | null,
    hasUpdates: false,
    rows: [] as ReadonlyArray<UpdateEverythingRow>,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    notify: () => {
      for (const listener of listeners) listener();
    },
  };
});
vi.mock("../../state/desktopUpdate", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../state/desktopUpdate")>()),
  useDesktopUpdateState: (await import("react")).useSyncExternalStore.bind(
    null,
    mocks.subscribe,
    () => mocks.update,
  ),
}));
vi.mock("./useUpdateEverything", () => ({
  useUpdateEverything: () => ({
    hasUpdates: mocks.hasUpdates,
    summary: "Update everything",
    version: null,
    providerCount: 0,
    rows: mocks.rows,
    restarts: [],
  }),
}));

function availableUpdate(releaseNotes: DesktopUpdateState["releaseNotes"]): DesktopUpdateState {
  return {
    enabled: true,
    status: "available",
    currentVersion: "0.0.50",
    hostArch: "arm64",
    appArch: "arm64",
    runningUnderArm64Translation: false,
    availableVersion: "0.0.51",
    downloadedVersion: null,
    releaseNotes,
    omittedReleaseCount: 0,
    downloadPercent: null,
    checkedAt: null,
    message: null,
    errorContext: null,
    canRetry: false,
  };
}

function downloadedUpdate(overrides: Partial<DesktopUpdateState> = {}): DesktopUpdateState {
  return {
    ...availableUpdate([]),
    status: "downloaded",
    downloadedVersion: "0.0.51",
    downloadPercent: 100,
    ...overrides,
  };
}

const PROMPT_SNOOZE_KEY = "onicode:desktop-update-prompt:snooze";

const RELEASE_NOTES: DesktopUpdateState["releaseNotes"] = [
  { version: "0.0.51", items: ["Faster sidebar."], totalItems: 1 },
];

function setup(
  update: DesktopUpdateState | null,
  {
    hasUpdates = false,
    rows = [],
  }: { hasUpdates?: boolean; rows?: ReadonlyArray<UpdateEverythingRow> } = {},
) {
  mocks.update = update;
  mocks.hasUpdates = hasUpdates;
  mocks.rows = rows;
  appAtomRegistry.set(desktopUpdateRestartingVersionAtom, null);
}

function publish(update: DesktopUpdateState | null) {
  return act(() => {
    mocks.update = update;
    mocks.notify();
  });
}

const releaseNotes = () => document.querySelector('[aria-label="Update release notes"]');

describe("SidebarUpdatePill release notes", () => {
  it("closes when the release notes stop applying", async () => {
    setup(availableUpdate(RELEASE_NOTES));
    await using _screen = await render(<SidebarUpdatePill />);
    await userEvent.tab();
    await vi.waitFor(() => expect(releaseNotes()).not.toBeNull());

    await publish(availableUpdate([]));
    await vi.waitFor(() => expect(releaseNotes()).toBeNull());
  });

  it("opens when release notes arrive while the trigger has keyboard focus", async () => {
    setup(availableUpdate([]));
    await using _screen = await render(<SidebarUpdatePill />);
    await userEvent.tab();
    expect(releaseNotes()).toBeNull();

    await publish(
      availableUpdate([
        {
          version: "0.0.51",
          items: ["**One-time tasks.** Pick a date."],
          totalItems: 1,
          stats: "3 changes from 2 contributors",
          footer: "Plus 2 more bug fixes.",
        },
      ]),
    );
    await expect.element(page.getByText("One-time tasks.", { exact: true })).toBeVisible();
    await expect.element(page.getByText("Plus 2 more bug fixes.")).toBeVisible();
  });
});

describe("SidebarUpdatePill update everything", () => {
  it("does not refocus the update button after a pointer cancel", async () => {
    setup(null, { hasUpdates: true });
    await using screen = await render(<SidebarUpdatePill />);
    const button = screen.getByRole("button", { name: "Update everything" });
    await userEvent.click(button);
    const cancel = page.getByRole("button", { name: "Cancel" });
    await expect.element(cancel).toBeVisible();

    await userEvent.click(cancel);
    await vi.waitFor(() => expect(document.querySelector('[role="alertdialog"]')).toBeNull());
    expect(document.activeElement).not.toBe(button.element());
  });

  it("returns focus to the update button after Escape", async () => {
    setup(null, { hasUpdates: true });
    await using screen = await render(<SidebarUpdatePill />);
    const button = screen.getByRole("button", { name: "Update everything" });
    await userEvent.click(button);
    await expect.element(page.getByRole("button", { name: "Cancel" })).toBeVisible();

    await userEvent.keyboard("{Escape}");
    await vi.waitFor(() => expect(document.activeElement).toBe(button.element()));
  });

  it("exposes the machines as a table with each value in its column", async () => {
    const label = "DESKTOP-WORKSTATION-UPSTAIRS";
    setup(null, {
      hasUpdates: true,
      rows: [{ environmentId: null, label, app: "this-app", skipped: null, providers: ["Claude"] }],
    });
    await using screen = await render(<SidebarUpdatePill />);
    await userEvent.click(screen.getByRole("button", { name: "Update everything" }));

    const table = page.getByRole("table");
    await expect.element(table.getByRole("columnheader", { name: "Machine" })).toBeVisible();
    await expect.element(table.getByRole("columnheader", { name: "Onicode" })).toBeVisible();
    await expect.element(table.getByRole("columnheader", { name: "Providers" })).toBeVisible();
    const row = table.getByRole("row", { name: new RegExp(label) });
    await expect.element(row.getByRole("cell", { name: label, exact: true })).toBeVisible();
    await expect.element(row.getByRole("cell", { name: "Claude", exact: true })).toBeVisible();
  });
});

describe("SidebarUpdatePill restart prompt", () => {
  it("snoozes when the user dismisses it, not when the update stops being ready", async () => {
    localStorage.removeItem(PROMPT_SNOOZE_KEY);
    setup(downloadedUpdate());
    await using _screen = await render(
      <ToastProvider>
        <SidebarUpdatePill />
      </ToastProvider>,
    );
    const prompt = page.getByText("Onicode 0.0.51 is ready");
    await expect.element(prompt).toBeVisible();

    await publish(availableUpdate([]));
    await expect.element(prompt).not.toBeInTheDocument();
    expect(localStorage.getItem(PROMPT_SNOOZE_KEY)).toBeNull();

    await publish(downloadedUpdate());
    await userEvent.click(page.getByRole("button", { name: "Later" }));
    await expect.element(prompt).not.toBeInTheDocument();
    expect(JSON.parse(localStorage.getItem(PROMPT_SNOOZE_KEY) ?? "null")).toMatchObject({
      version: "0.0.51",
    });
  });

  it("stays closed while the app quits into the update it started", async () => {
    localStorage.removeItem(PROMPT_SNOOZE_KEY);
    const installUpdate = vi.fn(async () => ({
      accepted: true,
      completed: false,
      state: downloadedUpdate(),
    }));
    const updateListeners = new Set<(state: DesktopUpdateState) => void>();
    using _bridge = stubGlobal("desktopBridge", {
      installUpdate,
      onUpdateState: (listener: (state: DesktopUpdateState) => void) => {
        updateListeners.add(listener);
        return () => updateListeners.delete(listener);
      },
    });
    setup(downloadedUpdate());
    await using screen = await render(
      <AppAtomRegistryProvider>
        <ToastProvider>
          <SidebarUpdatePill />
        </ToastProvider>
      </AppAtomRegistryProvider>,
    );
    const pill = screen.getByRole("button", {
      name: "Update 0.0.51 downloaded. Click to restart and install.",
    });
    await userEvent.click(page.getByRole("button", { name: "Restart to update" }));
    expect(installUpdate).toHaveBeenCalledOnce();
    // An accepted install stays pending until the app quits or macOS reports a refusal.
    await expect.element(pill).toHaveAttribute("aria-disabled");
    await expect.element(page.getByText("Onicode 0.0.51 is ready")).not.toBeInTheDocument();

    const refused = {
      ...downloadedUpdate(),
      message: "OSStatus error -60006.",
      errorContext: "install" as const,
    };
    for (const listener of updateListeners) listener(refused);
    await expect.element(page.getByText("OSStatus error -60006.")).toBeInTheDocument();
    await expect.element(pill).not.toHaveAttribute("aria-disabled");
    // Refused, so the restart prompt may offer it again.
    await expect.element(page.getByText("Onicode 0.0.51 is ready")).toBeInTheDocument();
    expect(updateListeners.size).toBe(0);
    localStorage.removeItem("onicode:update-everything-result:v1");
  });

  it("keeps a required update on screen, then restarts into it without a click", async () => {
    localStorage.removeItem(PROMPT_SNOOZE_KEY);
    const installUpdate = vi.fn(async () => ({
      accepted: true,
      completed: true,
      failed: false,
      state: mocks.update,
    }));
    using _bridge = stubGlobal("desktopBridge", { installUpdate, onUpdateState: () => () => {} });
    setup({
      ...downloadedUpdate({ required: true }),
      status: "downloading",
      downloadedVersion: null,
    });
    await using _screen = await render(
      <ToastProvider>
        <SidebarUpdatePill />
      </ToastProvider>,
    );
    const dialog = page.getByRole("alertdialog", { name: "Update required" });
    await expect.element(dialog.getByRole("button", { name: "Restart to update" })).toBeDisabled();
    await userEvent.keyboard("{Escape}");
    await expect.element(dialog).toBeVisible();

    using _timers = fakeTimers();
    await publish(downloadedUpdate({ required: true }));
    expect(dialog.getByRole("button", { name: "Restart now" }).query()).not.toBeNull();
    expect(page.getByText("Onicode 0.0.51 is ready").query()).toBeNull();
    expect(installUpdate).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(60_000);
    expect(installUpdate).toHaveBeenCalledOnce();
    localStorage.removeItem("onicode:update-everything-result:v1");
  });
});
