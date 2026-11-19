import { EnvironmentId } from "@onicode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

const host = vi.hoisted(() => ({
  target: undefined as unknown,
  httpBaseUrl: "http://192.168.1.25:3773",
  previewTunnel: false,
  canOperateTerminal: false,
}));

vi.mock("~/previewStateStore", () => ({ isPreviewSupportedInRuntime: () => true }));
vi.mock("~/state/primaryEnvironment", async () => {
  const { Atom } = await import("effect/reactivity");
  return { primaryEnvironmentIdAtom: Atom.make("primary") };
});
vi.mock("~/state/entities", () => ({
  readEnvironmentSupportsServerBrowser: () => true,
  readEnvironmentSupportsPreviewTunnel: () => host.previewTunnel,
}));
vi.mock("~/state/session", () => ({
  readPreparedConnection: () => ({ httpBaseUrl: host.httpBaseUrl, target: host.target }),
  readEnvironmentScope: () => host.canOperateTerminal,
}));

const remote = { _tag: "BearerConnectionTarget", connectionId: "saved:remote" };
const wsl = { _tag: "BearerConnectionTarget", connectionId: "local:wsl:Ubuntu" };
const lan = "http://192.168.1.25:3773";
// Over SSH the environment's origin is a local forward, and signed file previews load from it.
const ssh = "http://127.0.0.1:52000";

describe("previewRuntimeFor on desktop", () => {
  it.each([
    {
      case: "a host with the tunnel",
      target: remote,
      origin: lan,
      tunnel: true,
      operate: true,
      url: "http://localhost:5173/",
      runtime: undefined,
    },
    {
      case: "a host from before the tunnel",
      target: remote,
      origin: lan,
      tunnel: false,
      operate: true,
      url: "http://localhost:5173/",
      runtime: "server",
    },
    {
      case: "a host from before the tunnel, schemeless",
      target: remote,
      origin: lan,
      tunnel: false,
      operate: true,
      url: "127.0.0.1:5173",
      runtime: "server",
    },
    {
      case: "a host from before the tunnel, http://0.0.0.0:8000/",
      target: remote,
      origin: lan,
      tunnel: false,
      operate: true,
      url: "http://0.0.0.0:8000/",
      runtime: "server",
    },
    {
      case: "a host from before the tunnel, http://[::]:8000/",
      target: remote,
      origin: lan,
      tunnel: false,
      operate: true,
      url: "http://[::]:8000/",
      runtime: "server",
    },
    {
      case: "a host from before the tunnel, http://app.localhost.:5173/",
      target: remote,
      origin: lan,
      tunnel: false,
      operate: true,
      url: "http://app.localhost.:5173/",
      runtime: "server",
    },
    {
      case: "a host from before the tunnel, blank tab",
      target: remote,
      origin: lan,
      tunnel: false,
      operate: true,
      url: undefined,
      runtime: "server",
    },
    {
      case: "a host from before the tunnel, public site",
      target: remote,
      origin: lan,
      tunnel: false,
      operate: true,
      url: "https://example.com/",
      runtime: undefined,
    },
    {
      case: "a host from before the tunnel, file over SSH",
      target: remote,
      origin: ssh,
      tunnel: false,
      operate: true,
      url: `${ssh}/api/assets/a.html`,
      runtime: undefined,
    },
    {
      case: "a host from before the tunnel, SSH origin spelled localhost",
      target: remote,
      origin: ssh,
      tunnel: false,
      operate: true,
      url: "http://localhost:52000/api/assets/a.html",
      runtime: undefined,
    },
    {
      // On the environment, another loopback address at the forward's port is another server.
      case: "a host from before the tunnel, the SSH port at another address",
      target: remote,
      origin: ssh,
      tunnel: false,
      operate: true,
      url: "http://[::1]:52000/",
      runtime: "server",
    },
    {
      case: "a host from before the tunnel, another port over SSH",
      target: remote,
      origin: ssh,
      tunnel: false,
      operate: true,
      url: "http://127.0.0.1:5173/",
      runtime: "server",
    },
    {
      case: "a session without terminal:operate",
      target: remote,
      origin: lan,
      tunnel: true,
      operate: false,
      url: "http://localhost:5173/",
      runtime: "server",
    },
    {
      case: "the desktop's own WSL backend",
      target: wsl,
      origin: lan,
      tunnel: false,
      operate: false,
      url: "http://localhost:5173/",
      runtime: undefined,
    },
  ])(
    "$case: tabs open with runtime $runtime",
    async ({ target, origin, tunnel, operate, url, runtime }) => {
      host.target = target;
      host.httpBaseUrl = origin;
      host.previewTunnel = tunnel;
      host.canOperateTerminal = operate;
      const { previewRuntimeFor } = await import("./previewRuntime");
      expect(previewRuntimeFor(EnvironmentId.make("remote"), url)).toBe(runtime);
    },
  );

  it.each([
    { url: "http://localhost:5173/", move: null },
    { url: "https://example.com/", move: "desktop" },
    // A blank tab may be headed for a site only this computer reaches.
    { url: undefined, move: "desktop" },
  ] as const)("offers to move a host's $url tab to desktop: $move", async ({ url, move }) => {
    host.target = remote;
    host.httpBaseUrl = lan;
    host.previewTunnel = false;
    host.canOperateTerminal = true;
    const { alternatePreviewRuntime } = await import("./previewRuntime");
    const navStatus =
      url === undefined
        ? ({ _tag: "Idle" } as const)
        : ({ _tag: "Success", url, title: "" } as const);
    expect(
      alternatePreviewRuntime(EnvironmentId.make("remote"), null, true, {
        runtime: "server",
        navStatus,
      }),
    ).toBe(move);
  });

  it("opens the primary environment's tabs on the server, which desktop renders natively", async () => {
    host.target = { _tag: "PrimaryConnectionTarget" };
    host.previewTunnel = true;
    host.canOperateTerminal = true;
    const { previewRuntimeFor } = await import("./previewRuntime");
    expect(previewRuntimeFor(EnvironmentId.make("primary"), "http://localhost:5173/")).toBe(
      "server",
    );
    expect(previewRuntimeFor(EnvironmentId.make("primary"), undefined)).toBe("server");
  });
});
