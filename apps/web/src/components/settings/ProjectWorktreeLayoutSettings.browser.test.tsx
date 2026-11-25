import { RegistryContext } from "@effect/atom-react";
import { EnvironmentId, ProjectId, type WorktreeLayoutCommand } from "@onicode/contracts";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { describe, expect, it, vi } from "vite-plus/test";

import "~/index.css";
import type { SidebarProjectGroupMember } from "../../sidebarProjectGrouping";
import { render } from "../../../test/render";

const state = vi.hoisted(() => ({
  readableEnvs: new Set<EnvironmentId>(),
  approveScopeValue: false,
  approvePermissions: new Map<EnvironmentId, Atom.Atom<boolean>>(),
  layoutCommandAtoms: new Map<
    string,
    Atom.Atom<AsyncResult.AsyncResult<{ readonly commands: ReadonlyArray<WorktreeLayoutCommand> }>>
  >(),
}));

vi.mock(import("~/state/environments"), async (original) => ({
  ...(await original()),
  useEnvironments: (() => ({
    environments: [envA, envB].map((environmentId) => ({
      environmentId,
      label: environmentId,
      connection: { phase: "connected" },
    })),
  })) as never,
  usePrimaryEnvironmentId: () => envA,
  usePrimaryEnvironment: () => null,
}));

vi.mock(import("~/state/entities"), async (original) => ({
  ...(await original()),
  useServerConfigs: (() => new Map()) as never,
}));

vi.mock(import("~/state/managedPolicy"), () => ({
  useManagedPolicy: () => undefined,
}));

vi.mock(import("~/state/session"), async (original) => ({
  ...(await original()),
  useEnvironmentScope: (() => state.approveScopeValue) as never,
  useEnvironmentsWithScope: (() => state.readableEnvs) as never,
}));

vi.mock(import("~/state/filesystem"), async (original) => {
  const actual = await original();
  const emptyCommands = Atom.make(
    AsyncResult.success({ commands: [] as ReadonlyArray<WorktreeLayoutCommand> }),
  );
  const denied = Atom.make(false);
  return {
    ...actual,
    filesystemEnvironment: {
      ...actual.filesystemEnvironment,
      layoutCommands: (({
        environmentId,
        input,
      }: {
        readonly environmentId: EnvironmentId;
        readonly input: { readonly projectId: ProjectId };
      }) =>
        state.layoutCommandAtoms.get(`${environmentId}:${input.projectId}`) ??
        emptyCommands) as never,
      approveLayoutCommands: {
        ...actual.filesystemEnvironment.approveLayoutCommands,
        permissionAtom: (environmentId: EnvironmentId | null) =>
          (environmentId === null ? undefined : state.approvePermissions.get(environmentId)) ??
          denied,
      },
    },
  };
});

vi.mock(import("~/state/use-atom-command"), () => ({
  useAtomCommand: (() => vi.fn(async () => AsyncResult.success(undefined))) as never,
}));

import { ProjectWorktreeLayoutSettings } from "./ProjectWorktreeLayoutSettings";

const envA = EnvironmentId.make("env-a");
const envB = EnvironmentId.make("env-b");

function memberFor(environmentId: EnvironmentId, suffix: string): SidebarProjectGroupMember {
  return {
    id: ProjectId.make(`project-${suffix}`),
    environmentId,
    title: "Shared repo",
    workspaceRoot: `/work/${suffix}`,
    defaultModelSelection: null,
    physicalProjectKey: `${environmentId}:/work/${suffix}`,
    environmentLabel: environmentId,
    createdAt: "2026-01-01T00:00:00.000Z",
    updatedAt: "2026-01-01T00:00:00.000Z",
    scripts: [],
  };
}

const layoutCommandsAtom = (commands: ReadonlyArray<WorktreeLayoutCommand>) =>
  Atom.make(AsyncResult.success({ commands }));

function setup() {
  state.readableEnvs = new Set([envA, envB]);
  state.approveScopeValue = false;
  state.approvePermissions = new Map([
    [envA, Atom.make(true)],
    [envB, Atom.make(false)],
  ]);
  state.layoutCommandAtoms = new Map([
    [
      `${envA}:project-a`,
      layoutCommandsAtom([{ kind: "create", command: "scripts/create-a.sh", approved: false }]),
    ],
    [
      `${envB}:project-b`,
      layoutCommandsAtom([{ kind: "remove", command: "scripts/remove-b.sh", approved: false }]),
    ],
  ]);
}

describe("ProjectWorktreeLayoutSettings", () => {
  it("renders a single project-worktree-layout section across checkouts and uses approveLayoutCommands.permissionAtom", async () => {
    setup();
    const registry = AtomRegistry.make();
    using _registry = { [Symbol.dispose]: () => registry.dispose() };
    const members = [memberFor(envA, "a"), memberFor(envB, "b")];
    await using screen = await render(
      <RegistryContext.Provider value={registry}>
        <ProjectWorktreeLayoutSettings members={members} />
      </RegistryContext.Provider>,
      { router: false /* no routes involved */ },
    );

    expect(screen.container.querySelectorAll("#project-worktree-layout")).toHaveLength(1);
    await expect.element(screen.getByText("scripts/create-a.sh")).toBeVisible();
    await expect.element(screen.getByText("scripts/remove-b.sh")).toBeVisible();
    // envA is allowed by permissionAtom even though useEnvironmentScope returns false; envB is denied.
    await expect
      .element(screen.getByRole("button", { name: "Approve the create command" }))
      .toBeEnabled();
    await expect
      .element(screen.getByRole("button", { name: "Approve the remove command" }))
      .toBeDisabled();
  });

  it("renders nothing when no checkout declares layout commands", async () => {
    setup();
    state.layoutCommandAtoms = new Map();
    const registry = AtomRegistry.make();
    using _registry = { [Symbol.dispose]: () => registry.dispose() };
    await using screen = await render(
      <RegistryContext.Provider value={registry}>
        <ProjectWorktreeLayoutSettings members={[memberFor(envA, "a")]} />
      </RegistryContext.Provider>,
      { router: false /* no routes involved */ },
    );

    expect(screen.container.querySelector("#project-worktree-layout")).toBeNull();
  });
});
