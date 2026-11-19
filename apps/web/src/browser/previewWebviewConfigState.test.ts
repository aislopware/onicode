import { EnvironmentId } from "@onicode/contracts";
import { Atom, AtomRegistry } from "effect/reactivity";
import { expect, it, vi } from "vite-plus/test";

const mocks = vi.hoisted(() => ({ getPreviewConfig: vi.fn() }));

vi.mock(import("~/connection/catalog"), async (original) => {
  const actual = await original();
  return {
    ...actual,
    environmentCatalog: {
      ...actual.environmentCatalog,
      catalogValueAtom: Atom.make({
        isReady: false,
        entries: new Map(),
      }) as unknown as typeof actual.environmentCatalog.catalogValueAtom,
    },
  };
});

vi.mock(import("~/components/preview/previewBridge"), async (original) => ({
  ...(await original()),
  previewBridge: { getPreviewConfig: mocks.getPreviewConfig } as never,
}));

import { environmentCatalog } from "~/connection/catalog";
import { previewWebviewConfig } from "./previewWebviewConfigState";

it("asks for a partition only once the catalog knows whether to tunnel", async () => {
  mocks.getPreviewConfig.mockResolvedValue({
    partition: "p",
    webPreferences: "",
    preloadUrl: null,
  });
  const environmentId = EnvironmentId.make("environment-1");
  const registry = AtomRegistry.make();
  const unmount = registry.mount(previewWebviewConfig(environmentId));
  // Either guess would load one machine's localhost, then move the tab to another partition.
  await Promise.resolve();
  expect(mocks.getPreviewConfig).not.toHaveBeenCalled();

  registry.set(environmentCatalog.catalogValueAtom as never, {
    isReady: true,
    entries: new Map([[environmentId, { target: { _tag: "PrimaryConnectionTarget" } }]]),
  });
  await vi.waitFor(() =>
    expect(mocks.getPreviewConfig.mock.calls).toEqual([[environmentId, undefined, false]]),
  );
  unmount();
  registry.dispose();
});
