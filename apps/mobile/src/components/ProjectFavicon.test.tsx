import { RegistryContext } from "@effect/atom-react";
import { Atom, AtomRegistry } from "effect/reactivity";
import type { ReactNode } from "react";
import { act, Profiler } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  Passthrough: ({ children }: { children?: unknown }) => children,
  faviconUrlAtom: null as Atom.Writable<string | null, string | null> | null,
  refreshProjectFaviconUrl: vi.fn(),
  imageProps: null as {
    readonly source?: { readonly uri?: string };
    readonly onLoad?: () => void;
    readonly onError?: () => void;
  } | null,
}));

vi.mock("react-native", () => ({ View: harness.Passthrough }));
vi.mock("expo-image", () => ({
  Image: (props: {
    readonly source?: { readonly uri?: string };
    readonly onLoad?: () => void;
    readonly onError?: () => void;
  }) => {
    harness.imageProps = props;
    return <img alt="" src={props.source?.uri} />;
  },
}));
vi.mock("./ui/AppSymbol", () => ({ SymbolView: () => null }));
vi.mock("./ui/AppText", () => ({
  AppText: ({ children }: { children?: ReactNode }) => <span>{children}</span>,
}));
vi.mock("../features/settings/appearance/AppearancePreferencesProvider", () => ({
  useAppearancePreferences: () => ({ themeAppearance: "light" }),
}));
vi.mock("../state/assets", async () => {
  const { Atom } = await import("effect/reactivity");
  const defaultUrl = Atom.make<string | null>("https://example.test/favicon.png");
  const missing = Atom.make<string | null>(null);
  return {
    projectFaviconUrlAtom: (target: { readonly cwd: string }) =>
      target.cwd === "/repo-without-icon" ? missing : (harness.faviconUrlAtom ?? defaultUrl),
    refreshProjectFaviconUrl: (target: unknown) => harness.refreshProjectFaviconUrl(target),
  };
});

import { EnvironmentId } from "@onicode/contracts";

import { ProjectFavicon } from "./ProjectFavicon";

it("mounts the favicon image in the first commit", async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const container = document.createElement("div");
  const root = createRoot(container);
  const commits: string[] = [];
  try {
    await act(async () =>
      root.render(
        <Profiler id="favicon" onRender={(_id, phase) => commits.push(phase)}>
          <ProjectFavicon
            environmentId={EnvironmentId.make("env-1")}
            projectTitle="Project"
            workspaceRoot="/repo"
          />
        </Profiler>,
      ),
    );
    expect(container.querySelector("img")).not.toBeNull();
    expect(commits).toEqual(["mount"]);
  } finally {
    await act(async () => root.unmount());
    harness.imageProps = null;
  }
});

it("refreshes an expired remote favicon URL once on load error and recovers after reload", async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const registry = AtomRegistry.make();
  const environmentId = EnvironmentId.make("env-1");
  const expiredUrl = "https://example.test/api/assets/expired/v1-favicon.svg";
  const freshUrl = "https://example.test/api/assets/fresh/v1-favicon.svg";
  const faviconUrl = Atom.make<string | null>(expiredUrl);
  const refresh = vi.fn(() => {
    registry.set(faviconUrl, freshUrl);
  });
  harness.faviconUrlAtom = faviconUrl;
  harness.refreshProjectFaviconUrl = refresh;

  const container = document.createElement("div");
  const root = createRoot(container);
  try {
    await act(async () =>
      root.render(
        <RegistryContext.Provider value={registry}>
          <ProjectFavicon
            environmentId={environmentId}
            projectTitle="Project"
            workspaceRoot="/repo"
            faviconPath="logo.svg"
          />
        </RegistryContext.Provider>,
      ),
    );
    expect(container.querySelector("img")?.getAttribute("src")).toBe(expiredUrl);

    await act(async () => harness.imageProps?.onError?.());
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(refresh).toHaveBeenCalledWith({
      environmentId,
      cwd: "/repo",
      faviconPath: "logo.svg",
    });
    expect(container.querySelector("img")?.getAttribute("src")).toBe(freshUrl);

    // A second error before a successful load must not loop refreshes.
    await act(async () => harness.imageProps?.onError?.());
    expect(refresh).toHaveBeenCalledTimes(1);

    // Once the fresh URL loads, a later expiration can trigger another recovery refresh.
    await act(async () => harness.imageProps?.onLoad?.());
    await act(async () => harness.imageProps?.onError?.());
    expect(refresh).toHaveBeenCalledTimes(2);

    // Inline data URLs never trigger a remote refresh.
    await act(async () => {
      registry.set(faviconUrl, "data:image/svg+xml;base64,PHN2Zy8+");
    });
    await act(async () => harness.imageProps?.onError?.());
    expect(refresh).toHaveBeenCalledTimes(2);
  } finally {
    await act(async () => root.unmount());
    registry.dispose();
    harness.faviconUrlAtom = null;
    harness.refreshProjectFaviconUrl = vi.fn();
    harness.imageProps = null;
  }
});

it("shows the next project's icon when a group's own project has none", async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const container = document.createElement("div");
  const root = createRoot(container);
  const environmentId = EnvironmentId.make("env-1");
  try {
    await act(async () =>
      root.render(
        <ProjectFavicon
          environmentId={environmentId}
          projectTitle="Project"
          workspaceRoot="/repo-without-icon"
          fallbackProjects={[
            {
              environmentId,
              workspaceRoot: "/repo/apps/web",
              faviconPath: null,
              projectIcon: null,
            },
          ]}
        />,
      ),
    );
    expect(container.querySelector("img")?.getAttribute("src")).toBe(
      "https://example.test/favicon.png",
    );
  } finally {
    await act(async () => root.unmount());
  }
});
