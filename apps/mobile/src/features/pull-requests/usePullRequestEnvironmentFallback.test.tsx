import type { EnvironmentConnectionPhase } from "@onicode/client-runtime/connection";
import type { EnvironmentProject } from "@onicode/client-runtime/state/models";
import { EnvironmentId } from "@onicode/contracts";
import { act, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

import type { WorkspaceEnvironment } from "../../state/workspaceModel";
import type { PullRequestEnvironmentTarget } from "./pullRequestEnvironmentFallback";

const mocks = vi.hoisted(() => ({
  environments: [] as ReadonlyArray<WorkspaceEnvironment>,
  isReady: true,
  projects: [] as ReadonlyArray<EnvironmentProject>,
  navigation: { setParams: vi.fn<(params: Partial<PullRequestEnvironmentTarget>) => void>() },
  readHosts: [] as Array<string | undefined>,
}));

vi.mock("@react-navigation/native", async () => {
  const { useEffect } = await import("react");
  return {
    useNavigation: () => mocks.navigation,
    // The screen under test is focused, so its focus effect runs like an effect.
    useFocusEffect: (effect: () => void) => useEffect(effect, [effect]),
  };
});
vi.mock("../../state/entities", () => ({ useProjects: () => mocks.projects }));
vi.mock("../../state/workspace", () => ({
  useWorkspaceEnvironments: () => mocks.environments,
  useConnectionsReady: () => mocks.isReady,
}));

import { usePullRequestEnvironmentFallback } from "./usePullRequestEnvironmentFallback";

function environment(
  environmentId: string,
  connectionState: EnvironmentConnectionPhase,
  isEnabled = true,
): WorkspaceEnvironment {
  return {
    environmentId: EnvironmentId.make(environmentId),
    environmentLabel: environmentId,
    displayUrl: "",
    isEnabled,
    connectionState,
    connectionError: null,
    connectionErrorTraceId: null,
  };
}

function setup(initial: { readonly host?: string } = { host: "github.com" }) {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  mocks.isReady = true;
  mocks.environments = [environment("requested", "connecting"), environment("copy", "connected")];
  mocks.projects = [
    { environmentId: "requested", id: "original" },
    { environmentId: "copy", id: "copy-project" },
  ].map(
    (project) =>
      ({
        ...project,
        repositoryIdentity: {
          canonicalKey: "github.com/owner/repo",
          provider: "github",
          displayName: "owner/repo",
        },
      }) as unknown as EnvironmentProject,
  );
  const container = document.createElement("div");
  const root = createRoot(container);
  mocks.navigation.setParams = vi.fn();
  mocks.readHosts = [];
  function Probe() {
    const [params, updateParams] = useState<PullRequestEnvironmentTarget>({
      environmentId: "requested",
      projectId: "original",
      repository: "owner/repo",
      ...initial,
    });
    useEffect(() => {
      mocks.navigation.setParams.mockImplementation((next) =>
        updateParams((previous) => ({ ...previous, ...next })),
      );
    }, []);
    mocks.readHosts.push(usePullRequestEnvironmentFallback(params).host);
    return <span>{`${params.environmentId}/${params.projectId}`}</span>;
  }
  return {
    render() {
      act(() => root.render(<Probe />));
    },
    get target() {
      return container.textContent;
    },
    [Symbol.dispose]() {
      act(() => root.unmount());
    },
  };
}

it("keeps the requested target while the catalog initializes or its enabled connection is pending", () => {
  using screen = setup();
  mocks.isReady = false;
  mocks.environments = [environment("copy", "connected")];
  screen.render();
  expect(screen.target).toBe("requested/original");
  expect(mocks.navigation.setParams).not.toHaveBeenCalled();

  mocks.isReady = true;
  for (const phase of ["available", "connecting", "reconnecting", "connected"] as const) {
    mocks.environments = [environment("requested", phase), environment("copy", "connected")];
    screen.render();
    expect(screen.target).toBe("requested/original");
    expect(mocks.navigation.setParams).not.toHaveBeenCalled();
  }
});

it("falls back to a connected repository copy when the requested environment cannot connect", () => {
  for (const state of ["missing", "disabled", "offline", "error", "unsupported"] as const) {
    using screen = setup();
    mocks.environments = [
      ...(state === "missing"
        ? []
        : [
            environment(
              "requested",
              state === "disabled" ? "available" : state,
              state !== "disabled",
            ),
          ]),
      environment("copy", "connected"),
    ];
    screen.render();
    expect(screen.target).toBe("copy/copy-project");
    expect(mocks.navigation.setParams).toHaveBeenCalledExactlyOnceWith({
      environmentId: "copy",
      projectId: "copy-project",
    });
  }
});

it("does not replace a connecting target with a connected repository copy before its project hydrates", () => {
  using screen = setup();
  const projects = mocks.projects;
  mocks.projects = projects.filter((project) => project.environmentId !== "requested");
  screen.render();
  expect(screen.target).toBe("requested/original");
  expect(mocks.navigation.setParams).not.toHaveBeenCalled();

  mocks.projects = projects;
  mocks.environments = [environment("requested", "connected"), environment("copy", "connected")];
  screen.render();
  expect(screen.target).toBe("requested/original");
  expect(mocks.navigation.setParams).not.toHaveBeenCalled();
});

it("reads a host-less link on the project's host from the first render", () => {
  using screen = setup({});
  mocks.environments = [environment("requested", "connected")];
  screen.render();
  expect(mocks.readHosts[0]).toBe("github.com");
  expect(mocks.navigation.setParams).toHaveBeenCalledExactlyOnceWith({ host: "github.com" });
});
