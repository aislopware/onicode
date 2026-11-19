import {
  DEFAULT_CLIENT_SETTINGS,
  ProviderDriverKind,
  ProviderInstanceId,
  type ModelSelection,
} from "@onicode/contracts";
import { compileResolvedKeybindingsConfig } from "@onicode/shared/keybindings";
import { useState } from "react";
import { expect, it, vi } from "vite-plus/test";
import { page, userEvent } from "vite-plus/test/browser";

import "~/index.css";

import { render } from "../../../test/render";
import { __setClientSettingsForTests } from "../../hooks/useSettings";
import { deriveProviderInstanceEntries } from "../../providerInstances";
import { ProviderModelPicker } from "./ProviderModelPicker";

it("keeps each registry image when adding a model from another ACP agent", async () => {
  const icons = [
    "https://cdn.agentclientprotocol.com/registry/icons/picker-explicit.svg",
    "https://cdn.agentclientprotocol.com/registry/v1/latest/picker-by-id.svg",
  ];
  const cache = await caches.open("oni-acp-registry-icons-v1");
  await using _cachedIcons = {
    async [Symbol.asyncDispose]() {
      await Promise.all(icons.map((url) => cache.delete(url)));
    },
  };
  for (const [index, url] of icons.entries()) {
    await cache.put(
      url,
      new Response(
        `<svg xmlns="http://www.w3.org/2000/svg" width="${11 + index * 6}" height="10"><path d="M0 0h10v10H0z"/></svg>`,
        { headers: { "content-type": "image/svg+xml" } },
      ),
    );
  }
  const entries = deriveProviderInstanceEntries(
    ["First", "Second"].map((name) => ({
      instanceId: ProviderInstanceId.make(name.toLowerCase()),
      driver: ProviderDriverKind.make("acpRegistry"),
      displayName: `${name} agent`,
      enabled: true,
      installed: true,
      version: null,
      status: "ready" as const,
      auth: { status: "authenticated" as const },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models: [
        { slug: name.toLowerCase(), name: `${name} model`, isCustom: false, capabilities: {} },
      ],
      slashCommands: [],
      skills: [],
    })),
  ).map((entry, index) => ({
    ...entry,
    ...(index === 0 ? { acpRegistryIconUrl: icons[0] } : { acpRegistryAgentId: "picker-by-id" }),
  }));
  const first = { instanceId: entries[0]!.instanceId, model: "first" };
  const options = new Map(entries.map((entry) => [entry.instanceId, entry.models]));

  function Picker() {
    const [selection, setSelection] = useState<ReadonlyArray<ModelSelection>>([first]);
    return (
      <ProviderModelPicker
        environmentId={null}
        activeInstanceId={first.instanceId}
        model={first.model}
        {...(selection.length > 1 ? { selectedModels: selection } : {})}
        lockedProvider={null}
        instanceEntries={entries}
        modelOptionsByInstance={options}
        onToggleModel={(instanceId, model) => setSelection([...selection, { instanceId, model }])}
        onInstanceModelChange={(instanceId, model) => setSelection([{ instanceId, model }])}
      />
    );
  }

  await using screen = await render(<Picker />);
  const imageWidths = () =>
    Array.from(
      screen.container.querySelectorAll<HTMLImageElement>("[data-chat-provider-model-picker] img"),
      (image) => image.naturalWidth,
    );
  await vi.waitFor(() => expect(imageWidths()).toEqual([11]));
  await screen.getByRole("button", { name: "First model", exact: true }).click();
  await userEvent.keyboard("{Shift>}");
  try {
    await page.getByRole("option", { name: /Second model/ }).click();
  } finally {
    await userEvent.keyboard("{/Shift}");
  }
  await userEvent.keyboard("{Escape}");
  await vi.waitFor(() => expect(imageWidths()).toEqual([11, 17]));
});

it("keeps a docked picker open after a pick and hands focus to that model's effort", async () => {
  const instanceId = ProviderInstanceId.make("codex");
  const entries = deriveProviderInstanceEntries([
    {
      instanceId,
      driver: ProviderDriverKind.make("codex"),
      enabled: true,
      installed: true,
      version: null,
      status: "ready",
      auth: { status: "authenticated" },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models: [],
      slashCommands: [],
      skills: [],
    },
  ]);
  const options = new Map([
    [
      instanceId,
      [
        { slug: "original", name: "Original model" },
        { slug: "second", name: "Second model" },
        { slug: "plain", name: "Plain model" },
      ],
    ],
  ]);
  // The last level is the selected one; "plain" has no effort to pick.
  const levels: Record<string, ReadonlyArray<string>> = {
    original: ["Low", "High"],
    second: ["Medium"],
    plain: [],
  };

  function Picker() {
    const [model, setModel] = useState("original");
    return (
      <ProviderModelPicker
        environmentId={null}
        activeInstanceId={instanceId}
        model={model}
        lockedProvider={null}
        instanceEntries={entries}
        modelOptionsByInstance={options}
        triggerAriaLabel="Choose model"
        onInstanceModelChange={(_, selected) => setModel(selected)}
        effortColumn={(levels[model] ?? []).map((level, index, all) => (
          <button
            key={level}
            type="button"
            role="radio"
            aria-checked={index === all.length - 1}
            data-effort-row
            data-effort-level
          >
            {level}
          </button>
        ))}
      />
    );
  }

  await using screen = await render(<Picker />);
  await screen.getByRole("button", { name: "Choose model" }).click();
  const search = page.getByPlaceholder("Search models");
  await expect.element(search).toHaveFocus();
  await userEvent.keyboard("{ArrowRight}");
  await expect.element(page.getByRole("radio", { name: "High" })).toHaveFocus();
  await userEvent.keyboard("{ArrowUp}");
  await expect.element(search).toHaveFocus();

  await page.getByRole("option", { name: /Second model/ }).click();
  await expect.element(page.getByRole("radio", { name: "Medium" })).toHaveFocus();

  await page.getByRole("option", { name: /Plain model/ }).click();
  await page.getByRole("option", { name: /Second model/ }).click();
  await expect.element(page.getByRole("radio", { name: "Medium" })).toHaveFocus();
  await userEvent.keyboard("{Escape}");
  await expect.element(search).not.toBeInTheDocument();
});

it("lists a model reported twice by one provider once, with contiguous jump shortcuts", async () => {
  const instanceId = ProviderInstanceId.make("claude");
  const entries = deriveProviderInstanceEntries([
    {
      instanceId,
      driver: ProviderDriverKind.make("claudeAgent"),
      enabled: true,
      installed: true,
      version: null,
      status: "ready",
      auth: { status: "authenticated" },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models: [],
      slashCommands: [],
      skills: [],
    },
  ]);
  const options = new Map([
    [
      instanceId,
      ["a", "b", "c", "c", "d", "d"].map((slug) => ({
        slug,
        name: `Model ${slug.toUpperCase()}`,
      })),
    ],
  ]);
  const keybindings = compileResolvedKeybindingsConfig(
    ([1, 2, 3, 4, 5, 6] as const).map((index) => ({
      key: `mod+${index}`,
      command: `modelPicker.jump.${index}` as const,
      when: "modelPickerOpen",
    })),
  );

  await using screen = await render(
    <ProviderModelPicker
      environmentId={null}
      activeInstanceId={instanceId}
      model="a"
      lockedProvider={null}
      instanceEntries={entries}
      modelOptionsByInstance={options}
      keybindings={keybindings}
      triggerAriaLabel="Choose model"
      onInstanceModelChange={() => {}}
    />,
  );
  await screen.getByRole("button", { name: "Choose model" }).click();
  await expect.element(page.getByRole("option", { name: /Model A/ })).toBeVisible();
  await vi.waitFor(() =>
    expect(
      page
        .getByRole("option")
        .elements()
        .map((row) => [
          row.textContent?.match(/Model [A-Z]/)?.[0],
          row.querySelector("[data-model-jump-label]")?.textContent?.slice(-1),
        ]),
    ).toEqual([
      ["Model A", "1"],
      ["Model B", "2"],
      ["Model C", "3"],
      ["Model D", "4"],
    ]),
  );
});

it("folds a provider's models under each backend, opening the thread's backend", async () => {
  const instanceId = ProviderInstanceId.make("fx");
  const entries = deriveProviderInstanceEntries([
    {
      instanceId,
      driver: ProviderDriverKind.make("fx"),
      enabled: true,
      installed: true,
      version: null,
      status: "ready",
      auth: { status: "authenticated" },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models: [],
      subProviders: [
        { name: "Grok", unavailableReason: "Signed out. Run `fx login grok` on this machine." },
        { name: "Vercel AI Gateway" },
        { name: "Codex" },
      ],
      slashCommands: [],
      skills: [],
    },
  ]);
  const options = new Map([
    [
      instanceId,
      [
        { slug: "gateway:opus", name: "Opus", subProvider: "Vercel AI Gateway" },
        { slug: "codex:gpt", name: "GPT", subProvider: "Codex" },
      ],
    ],
  ]);

  await using screen = await render(
    <ProviderModelPicker
      environmentId={null}
      activeInstanceId={instanceId}
      model="codex:gpt"
      lockedProvider={null}
      instanceEntries={entries}
      modelOptionsByInstance={options}
      keybindings={compileResolvedKeybindingsConfig([
        { key: "ctrl+]", command: "modelPicker.nextProvider", when: "modelPickerOpen" },
      ])}
      triggerAriaLabel="Choose model"
      onInstanceModelChange={() => {}}
    />,
  );
  await screen.getByRole("button", { name: "Choose model" }).click();
  await expect.element(page.getByRole("option", { name: /GPT/ })).toBeVisible();
  const optionNames = () =>
    page
      .getByRole("option")
      .elements()
      .map((row) => row.textContent?.match(/^(Vercel AI Gateway|Codex|GPT|Opus)/)?.[1]);
  expect(optionNames()).toEqual(["Vercel AI Gateway", "Codex", "GPT"]);
  await expect
    .element(page.getByText("Signed out. Run `fx login grok` on this machine."))
    .toBeVisible();

  // A signed-out backend leads the section, and the provider shortcut still lands on fx.
  await userEvent.keyboard("{Control>}]{/Control}");
  await expect
    .element(page.getByRole("option", { name: /Vercel AI Gateway/ }))
    .toHaveAttribute("data-highlighted");

  await page.getByRole("option", { name: /Vercel AI Gateway/ }).click();
  await vi.waitFor(() =>
    expect(optionNames()).toEqual(["Vercel AI Gateway", "Opus", "Codex", "GPT"]),
  );
});

it("opens on the active provider when every favorite belongs to a disabled provider", async () => {
  const entries = deriveProviderInstanceEntries(
    ["Codex", "Claude"].map((name) => ({
      instanceId: ProviderInstanceId.make(name.toLowerCase()),
      driver: ProviderDriverKind.make(name.toLowerCase()),
      enabled: name === "Codex",
      installed: true,
      version: null,
      status: "ready" as const,
      auth: { status: "authenticated" as const },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models: [
        { slug: name.toLowerCase(), name: `${name} model`, isCustom: false, capabilities: {} },
      ],
      slashCommands: [],
      skills: [],
    })),
  );
  const [codex, claude] = entries;
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    favorites: [{ provider: claude!.instanceId, model: "claude" }],
  });
  using _settings = {
    [Symbol.dispose]: () => __setClientSettingsForTests(DEFAULT_CLIENT_SETTINGS),
  };

  await using screen = await render(
    <ProviderModelPicker
      environmentId={null}
      activeInstanceId={codex!.instanceId}
      model="codex"
      lockedProvider={null}
      instanceEntries={entries}
      modelOptionsByInstance={new Map(entries.map((entry) => [entry.instanceId, entry.models]))}
      triggerAriaLabel="Choose model"
      onInstanceModelChange={() => {}}
    />,
  );
  await screen.getByRole("button", { name: "Choose model" }).click();
  await expect.element(page.getByRole("option", { name: /Codex model/ })).toBeVisible();
});

it("opens on favorites and recent picks, with every other model behind All models", async () => {
  const entries = deriveProviderInstanceEntries(
    [
      { name: "Claude", models: ["Opus", "Haiku", "Sonnet"] },
      { name: "Codex", models: ["Sol"] },
    ].map(({ name, models }) => ({
      instanceId: ProviderInstanceId.make(name.toLowerCase()),
      driver: ProviderDriverKind.make(name === "Claude" ? "claudeAgent" : name.toLowerCase()),
      displayName: name,
      enabled: true,
      installed: true,
      version: null,
      status: "ready" as const,
      auth: { status: "authenticated" as const },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models: models.map((model) => ({
        slug: model.toLowerCase(),
        name: model,
        isCustom: false,
        capabilities: {},
      })),
      slashCommands: [],
      skills: [],
    })),
  );
  const [claude] = entries;
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    favorites: [{ provider: claude!.instanceId, model: "haiku" }],
  });
  using _settings = {
    [Symbol.dispose]: () => __setClientSettingsForTests(DEFAULT_CLIENT_SETTINGS),
  };

  const keybindings = compileResolvedKeybindingsConfig([
    ...([1, 2, 3, 4, 5, 6] as const).map((index) => ({
      key: `mod+${index}`,
      command: `modelPicker.jump.${index}` as const,
      when: "modelPickerOpen",
    })),
    { key: "ctrl+[", command: "modelPicker.previousProvider", when: "modelPickerOpen" },
  ]);

  const picker = () => (
    <ProviderModelPicker
      environmentId={null}
      activeInstanceId={claude!.instanceId}
      model="opus"
      lockedProvider={null}
      instanceEntries={entries}
      modelOptionsByInstance={new Map(entries.map((entry) => [entry.instanceId, entry.models]))}
      keybindings={keybindings}
      triggerAriaLabel="Choose model"
      onInstanceModelChange={() => {}}
    />
  );
  await using screen = await render(picker());
  await screen.getByRole("button", { name: "Choose model" }).click();
  const rows = () =>
    page
      .getByRole("option")
      .elements()
      .map((row) => [
        row.textContent?.match(/Haiku|Opus|Sonnet|Sol|All models/)?.[0],
        row.querySelector("[data-model-jump-label]")?.textContent?.slice(-1),
      ]);
  const names = () => rows().map(([name]) => name);
  // The favorite, then the selected model as the newest pick, which Recent marks as selected.
  await vi.waitFor(() => expect(names()).toEqual(["Haiku", "Opus", "All models"]));
  await expect
    .element(page.getByRole("option", { name: /Opus/ }))
    .toHaveAttribute("aria-selected", "true");

  // Typing searches every model, not only the ones in view.
  await userEvent.keyboard("sol");
  await vi.waitFor(() => expect(names()).toContain("Sol"));
  await userEvent.clear(page.getByPlaceholder("Search models"));
  await vi.waitFor(() => expect(names()).toEqual(["Haiku", "Opus", "All models"]));

  // A star changed while the list is open leaves it folded the way it opened.
  __setClientSettingsForTests({ ...DEFAULT_CLIENT_SETTINGS, favorites: [] });
  await screen.rerender(picker());
  await vi.waitFor(() => expect(names()).toEqual(["Opus", "All models"]));

  // Opus is listed under Recent and under its provider, and takes one jump shortcut.
  await page.getByRole("option", { name: /All models/ }).click();
  await vi.waitFor(() =>
    expect(rows()).toEqual([
      ["Opus", "1"],
      ["Opus", undefined],
      ["Haiku", "2"],
      ["Sonnet", "3"],
      ["Sol", "4"],
    ]),
  );

  // Provider navigation moves between providers; Recent only repeats their models.
  await userEvent.keyboard("{Control>}[[{/Control}");
  await expect
    .element(page.getByRole("option", { name: /Sol/ }))
    .toHaveAttribute("data-highlighted");
  await userEvent.keyboard("{Control>}[[{/Control}");
  await expect
    .element(page.getByRole("option", { name: /Opus/ }).nth(1))
    .toHaveAttribute("data-highlighted");
  await userEvent.keyboard("{Control>}[[{/Control}");
  await expect
    .element(page.getByRole("option", { name: /Sol/ }))
    .toHaveAttribute("data-highlighted");
});

it("keeps the touch keyboard down when the picker opens", async () => {
  const entries = deriveProviderInstanceEntries(
    ["Codex", "Claude"].map((name) => ({
      instanceId: ProviderInstanceId.make(name.toLowerCase()),
      driver: ProviderDriverKind.make(name.toLowerCase()),
      enabled: true,
      installed: true,
      version: null,
      status: "ready" as const,
      auth: { status: "authenticated" as const },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models: [
        { slug: name.toLowerCase(), name: `${name} model`, isCustom: false, capabilities: {} },
      ],
      slashCommands: [],
      skills: [],
    })),
  );
  const [codex] = entries;
  const matchMedia = window.matchMedia.bind(window);
  using _touch = vi.spyOn(window, "matchMedia").mockImplementation((query) =>
    query === "(pointer: coarse)"
      ? ({
          matches: true,
          media: query,
          onchange: null,
          addEventListener: () => {},
          removeEventListener: () => {},
          addListener: () => {},
          removeListener: () => {},
          dispatchEvent: () => false,
        } as MediaQueryList)
      : matchMedia(query),
  );

  await using screen = await render(
    <ProviderModelPicker
      environmentId={null}
      activeInstanceId={codex!.instanceId}
      model="codex"
      lockedProvider={null}
      instanceEntries={entries}
      modelOptionsByInstance={new Map(entries.map((entry) => [entry.instanceId, entry.models]))}
      triggerAriaLabel="Choose model"
      onInstanceModelChange={() => {}}
    />,
  );
  await screen.getByRole("button", { name: "Choose model" }).click();
  const search = page.getByPlaceholder("Search models");
  const popup = search.element().closest('[data-slot="popover-popup"]');
  await vi.waitFor(() => expect(document.activeElement).toBe(popup));
  await expect.element(page.getByRole("option", { name: /Claude model/ })).toBeVisible();
  await expect.element(search).not.toHaveFocus();
});

it("keeps the selected model's options, favorites and every provider in the one popup", async () => {
  const entries = deriveProviderInstanceEntries(
    [
      { name: "Claude", status: "ready" as const, models: ["Opus", "Haiku"] },
      { name: "Codex", status: "ready" as const, models: ["Sol"] },
      { name: "Grok", status: "error" as const, models: [] },
    ].map(({ name, status, models }) => ({
      instanceId: ProviderInstanceId.make(name.toLowerCase()),
      driver: ProviderDriverKind.make(name === "Claude" ? "claudeAgent" : name.toLowerCase()),
      displayName: name,
      enabled: true,
      installed: true,
      version: null,
      status,
      auth: { status: "authenticated" as const },
      checkedAt: "2026-01-01T00:00:00.000Z",
      models: models.map((model) => ({
        slug: model.toLowerCase(),
        name: model,
        isCustom: false,
        capabilities: {},
      })),
      slashCommands: [],
      skills: [],
    })),
  );
  const [claude] = entries;
  const keybindings = compileResolvedKeybindingsConfig([
    { key: "ctrl+]", command: "modelPicker.nextProvider", when: "modelPickerOpen" },
  ]);
  __setClientSettingsForTests({
    ...DEFAULT_CLIENT_SETTINGS,
    favorites: [{ provider: claude!.instanceId, model: "haiku" }],
  });
  using _settings = {
    [Symbol.dispose]: () => __setClientSettingsForTests(DEFAULT_CLIENT_SETTINGS),
  };

  function Picker() {
    const [selection, setSelection] = useState({ instanceId: claude!.instanceId, model: "haiku" });
    return (
      <>
        <output aria-label="Selected model">{selection.model}</output>
        <ProviderModelPicker
          environmentId={null}
          activeInstanceId={selection.instanceId}
          model={selection.model}
          lockedProvider={null}
          instanceEntries={entries}
          modelOptionsByInstance={new Map(entries.map((entry) => [entry.instanceId, entry.models]))}
          keybindings={keybindings}
          triggerAriaLabel="Choose model"
          onInstanceModelChange={(instanceId, model) => setSelection({ instanceId, model })}
          getModelDisabledReason={(_, model) => (model === "sol" ? "Sol is busy" : null)}
          effortColumn={<span>Levels of {selection.model}</span>}
        />
      </>
    );
  }

  await using screen = await render(<Picker />);
  await screen.getByRole("button", { name: "Choose model" }).click();
  const popup = page
    .getByPlaceholder("Search models")
    .element()
    .closest("[data-slot=popover-popup]")!;
  // The options sit under the list, inside the popup, and no second surface opens beside it.
  await vi.waitFor(() => expect(popup.textContent).toContain("Levels of haiku"));
  expect(document.querySelectorAll("[data-slot=popover-popup], [role=menu]")).toHaveLength(1);

  // Haiku is listed under Favorites and again under its provider; Favorites holds the selection.
  const haiku = page.getByRole("option", { name: /Haiku/ });
  await expect.element(haiku.nth(0)).toHaveAttribute("aria-selected", "true");
  await expect.element(haiku.nth(1)).toHaveAttribute("aria-selected", "false");

  // The provider shortcut moves the highlight with the view, so Enter picks from that provider.
  await expect.element(haiku.nth(0)).toHaveAttribute("data-highlighted");
  await userEvent.keyboard("{Control>}]{/Control}");
  await expect
    .element(page.getByRole("option", { name: /Opus/ }))
    .toHaveAttribute("data-highlighted");
  await userEvent.keyboard("{Enter}");
  const selected = screen.getByRole("status", { name: "Selected model" });
  await expect.element(selected).toHaveTextContent("opus");

  // The star on a row is its own control: pressing it does not pick the model.
  await haiku.nth(1).getByRole("button", { name: "Remove from favorites" }).click();
  await expect.element(selected).toHaveTextContent("opus");

  // A provider with nothing to list says why, and a row that cannot be picked says why on hover.
  await expect.element(page.getByText("Grok provider is unavailable.")).toBeVisible();
  await page.getByRole("option", { name: /Sol/ }).hover();
  await expect.element(page.getByText("Sol is busy")).toBeVisible();
});
