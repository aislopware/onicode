import { RegistryContext, useAtomValue } from "@effect/atom-react";
import { ANOTHER_ADDRESS, type LinkableMachine } from "@onicode/client-runtime/link-request";
import type { LinkThroughRoute } from "@onicode/client-runtime/state/peerLinks";
import { EnvironmentId, type PeerLinkSummary } from "@onicode/contracts";
import * as Cause from "effect/Cause";
import * as DateTime from "effect/DateTime";
import { AsyncResult, AtomRegistry } from "effect/reactivity";
import { describe, expect, it, vi } from "vite-plus/test";
import { page } from "vite-plus/test/browser";

import "~/index.css";
import { peerLinkEnvironment } from "~/state/peerLinks";
import { render } from "../../../test/render";
import { LinkedEnvironmentsSettings, LinkEnvironmentDialog } from "./LinkedEnvironmentsSettings";

const hostId = EnvironmentId.make("mac");
const viaId = EnvironmentId.make("nucbox");
const vmLink: PeerLinkSummary = {
  environmentId: EnvironmentId.make("vm"),
  label: "boat-task",
  urls: ["http://boat-task.example.ts.net:3773"],
  access: "full-access",
  linkedAt: DateTime.makeUnsafe("2026-11-01T00:00:00.000Z"),
  expiresAt: DateTime.makeUnsafe("2099-01-01T00:00:00.000Z"),
  lastReachedAt: null,
  lastError: null,
  status: "reachable",
};

// What the host's `peerLinks.list` answers. The CLI and agents link machines without this client.
const server = vi.hoisted(() => ({
  links: [] as Array<PeerLinkSummary>,
  started: 0,
  interrupted: 0,
  answer: Promise.resolve(),
  // What each atom command answers, by the label the dialog runs it under.
  commands: new Map<string, unknown>(),
  // The machines another environment links to, as the dialog's through-route hook reports them.
  routes: new Map<EnvironmentId, LinkThroughRoute>(),
  // The list mock below; with `queriesReadList`, every query hook reads it.
  list: null as unknown,
  queriesReadList: false,
  // The saved machines the dialog offers, in place of the ones the app has saved.
  machines: null as ReadonlyArray<LinkableMachine> | null,
}));

function setup() {
  server.links = [];
  server.started = 0;
  server.interrupted = 0;
  server.answer = Promise.resolve();
  server.commands = new Map();
  server.routes = new Map();
  server.queriesReadList = false;
  server.machines = null;
}

vi.mock(import("~/state/use-atom-command"), async (original) => ({
  ...(await original()),
  useAtomCommand: ((_command: unknown, options?: string | { readonly label?: string }) => {
    const label = typeof options === "string" ? options : (options?.label ?? "");
    return async () => server.commands.get(label);
  }) as never,
}));

// `useLinkableMachines` reads the source's list inside ~/state/peerLinks, past the list mock below.
vi.mock(import("~/state/query"), async (original) => {
  const actual = await original();
  return {
    ...actual,
    useEnvironmentQuery: ((atom: unknown, resolve?: never) =>
      actual.useEnvironmentQuery(
        (server.queriesReadList && atom !== null ? server.list : atom) as never,
        resolve,
      )) as never,
  };
});

vi.mock(import("~/state/environments"), async (original) => {
  const actual = await original();
  return {
    ...actual,
    // The environment the dialog links through; the factory runs before `viaId` exists.
    useEnvironment: ((environmentId: EnvironmentId | null) =>
      (environmentId as string | null) === "nucbox"
        ? { environmentId, label: "nucbox" }
        : actual.useEnvironment(environmentId)) as never,
  };
});

vi.mock(import("~/state/peerLinks"), async (original) => {
  const actual = await original();
  const { Atom: AtomModule } = await import("effect/reactivity");
  const EffectModule = await import("effect/Effect");
  // The real list query's options: stale-while-revalidate, revalidating only when it is built.
  const list = AtomModule.make(
    EffectModule.gen(function* () {
      server.started += 1;
      yield* EffectModule.promise(() => server.answer);
      return { links: [...server.links] };
    }).pipe(
      EffectModule.onInterrupt(() =>
        EffectModule.sync(() => {
          server.interrupted += 1;
        }),
      ),
    ),
  ).pipe(AtomModule.swr({ staleTime: 10_000, revalidateOnMount: true }));
  server.list = list;
  const denied = AtomModule.make(false);
  return {
    ...actual,
    useLinkThroughRoutes: (() => server.routes) as never,
    useLinkableMachines: ((...args: Parameters<typeof actual.useLinkableMachines>) => {
      const listed = actual.useLinkableMachines(...args);
      return server.machines ?? listed;
    }) as never,
    peerLinkEnvironment: {
      ...actual.peerLinkEnvironment,
      list: (() => list) as never,
      link: { ...actual.peerLinkEnvironment.link, permissionAtom: () => denied },
      unlink: { ...actual.peerLinkEnvironment.unlink, permissionAtom: () => denied },
    },
  };
});

/** Stands in for the chat composer, which reads the same list while a thread is open. */
function ComposerReader() {
  const result = useAtomValue(peerLinkEnvironment.list({ environmentId: hostId, input: {} }));
  return AsyncResult.isSuccess(result) ? <p>Composer loaded</p> : null;
}

describe("LinkedEnvironmentsSettings", () => {
  it("shows a link made elsewhere while the composer keeps the list loaded", async () => {
    setup();
    const registry = AtomRegistry.make();
    using _registry = { [Symbol.dispose]: () => registry.dispose() };
    await using composer = await render(
      <RegistryContext.Provider value={registry}>
        <ComposerReader />
      </RegistryContext.Provider>,
    );
    await expect.element(composer.getByText("Composer loaded")).toBeVisible();

    server.links = [vmLink];
    await using screen = await render(
      <RegistryContext.Provider value={registry}>
        <LinkedEnvironmentsSettings
          environmentId={hostId}
          environmentLabel={null}
          isSearchTarget={false}
          onLinkSeveral={() => {}}
        />
      </RegistryContext.Provider>,
    );

    await expect.element(screen.getByRole("heading", { name: "boat-task" })).toBeVisible();
  });

  it("leaves a list that is still loading to answer", async () => {
    setup();
    // useLiveRefresh remembers each view's last read across mounts, so this view gets its own host.
    const loadingHostId = EnvironmentId.make("mac-loading");
    let answer = () => {};
    server.answer = new Promise((resolve) => {
      answer = resolve;
    });
    server.links = [vmLink];
    const registry = AtomRegistry.make();
    using _registry = { [Symbol.dispose]: () => registry.dispose() };
    await using screen = await render(
      <RegistryContext.Provider value={registry}>
        <LinkedEnvironmentsSettings
          environmentId={loadingHostId}
          environmentLabel={null}
          isSearchTarget={false}
          onLinkSeveral={() => {}}
        />
      </RegistryContext.Provider>,
    );
    await expect.element(screen.getByRole("status")).toBeVisible();

    answer();
    await expect.element(screen.getByRole("heading", { name: "boat-task" })).toBeVisible();
    expect(server).toMatchObject({ started: 1, interrupted: 0 });
  });

  it("asks for an address and a code once linking through another environment fails", async () => {
    setup();
    server.commands.set(
      "pair link through a linked environment",
      AsyncResult.success({
        type: "credential",
        environmentId: vmLink.environmentId,
        label: "boat-task",
        urls: ["http://192.168.1.20:3773"],
        credential: "ONE-TIME",
        expiresAt: vmLink.expiresAt,
      }),
    );
    server.commands.set(
      "link environment",
      AsyncResult.failure(Cause.fail(new Error("Nothing answered at http://192.168.1.20:3773"))),
    );
    await using _screen = await render(
      <LinkEnvironmentDialog
        environmentId={hostId}
        open
        onOpenChange={() => {}}
        prefill={{
          choice: ANOTHER_ADDRESS,
          address: vmLink.urls[0]!,
          through: {
            viaEnvironmentId: viaId,
            environmentId: vmLink.environmentId,
            label: "boat-task",
          },
        }}
      />,
    );
    const dialog = page.getByRole("dialog");
    await expect.element(dialog.getByText(/No code to paste/)).toBeVisible();

    await dialog.getByRole("button", { name: "Link", exact: true }).click();

    await expect
      .element(dialog.getByRole("textbox", { name: "Address" }))
      .toHaveValue(vmLink.urls[0]!);
    await expect.element(dialog.getByRole("textbox", { name: "Pairing code" })).toBeVisible();
  });

  it("asks for a saved machine's address once linking through another fails", async () => {
    setup();
    const storedUrl = "http://boat-task.local:3773";
    server.machines = [
      {
        environmentId: vmLink.environmentId,
        label: "boat-task",
        machine: "server",
        connected: false,
        linked: false,
        urls: [storedUrl],
      },
    ];
    server.routes.set(vmLink.environmentId, {
      viaEnvironmentId: viaId,
      environmentId: vmLink.environmentId,
      label: "boat-task",
      urls: vmLink.urls,
    });
    server.commands.set(
      "pair link through a linked environment",
      AsyncResult.success({
        type: "credential",
        environmentId: vmLink.environmentId,
        label: "boat-task",
        urls: ["http://192.168.1.20:3773"],
        credential: "ONE-TIME",
        expiresAt: vmLink.expiresAt,
      }),
    );
    server.commands.set(
      "link environment",
      AsyncResult.failure(Cause.fail(new Error("Nothing answered at http://192.168.1.20:3773"))),
    );
    const registry = AtomRegistry.make();
    using _registry = { [Symbol.dispose]: () => registry.dispose() };
    await using _screen = await render(
      <RegistryContext.Provider value={registry}>
        <LinkEnvironmentDialog
          environmentId={hostId}
          open
          onOpenChange={() => {}}
          prefill={{ choice: vmLink.environmentId }}
        />
      </RegistryContext.Provider>,
    );
    const dialog = page.getByRole("dialog");
    await expect.element(dialog.getByText(/No code to paste/)).toBeVisible();

    await dialog.getByRole("button", { name: "Link", exact: true }).click();

    await expect.element(dialog.getByRole("textbox", { name: "Address" })).toHaveValue(storedUrl);
    await expect.element(dialog.getByRole("textbox", { name: /^Pairing code/ })).toBeVisible();
  });

  it("keeps a machine found through another environment once the source's links load", async () => {
    setup();
    server.queriesReadList = true;
    server.routes.set(vmLink.environmentId, {
      viaEnvironmentId: viaId,
      environmentId: vmLink.environmentId,
      label: "boat-task",
      urls: vmLink.urls,
    });
    let answer = () => {};
    server.answer = new Promise((resolve) => {
      answer = resolve;
    });
    server.links = [vmLink];
    const registry = AtomRegistry.make();
    using _registry = { [Symbol.dispose]: () => registry.dispose() };
    await using _screen = await render(
      <RegistryContext.Provider value={registry}>
        <LinkEnvironmentDialog
          environmentId={hostId}
          open
          onOpenChange={() => {}}
          prefill={{ choice: vmLink.environmentId }}
        />
      </RegistryContext.Provider>,
    );
    const dialog = page.getByRole("dialog");
    const submit = dialog.getByRole("button", { name: "Link", exact: true });
    await expect.element(dialog.getByText(/No code to paste/)).toBeVisible();
    await expect.element(submit).toBeDisabled();

    answer();

    await expect.element(dialog.getByText(/Already linked/)).toBeVisible();
    await expect.element(dialog.getByText(/No code to paste/)).toBeVisible();
    await expect.element(submit).toBeEnabled();
  });
});
