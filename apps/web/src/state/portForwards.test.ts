import { EnvironmentId } from "@onicode/contracts";
import * as Option from "effect/Option";
import { Atom, AtomRegistry } from "effect/reactivity";
import * as AsyncResult from "effect/reactivity/AsyncResult";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("./session", () => ({
  environmentSession: {
    transportSessionAtom: () => Atom.make(AsyncResult.success(Option.some({}))),
  },
}));

const { forwardLocalitiesAtom, forwardPort, forwardedUrl, isSameMachineForwardError } =
  await import("./portForwards");

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("forward locality", () => {
  it("probes again after a failed probe instead of staying unknown", async () => {
    vi.useFakeTimers();
    const sharesLoopback = vi
      .fn<() => Promise<boolean>>()
      .mockRejectedValueOnce(new Error("tunnel dial refused"))
      .mockResolvedValue(false);
    vi.stubGlobal("window", { desktopBridge: { preview: { sharesLoopback } } });
    const registry = AtomRegistry.make();
    const environmentId = EnvironmentId.make("env-remote");
    const atom = forwardLocalitiesAtom(JSON.stringify([environmentId]));
    const unmount = registry.mount(atom);

    await vi.advanceTimersByTimeAsync(0);
    expect(sharesLoopback).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1_000);

    expect(sharesLoopback).toHaveBeenCalledTimes(2);
    expect(registry.get(atom)).toEqual(["remote"]);
    unmount();
  });
});

describe("forwardedUrl", () => {
  it.each([
    ["http://127.0.0.1:3000/docs?x=1#top", "http://localhost:4100/docs?x=1#top"],
    ["http://admin.localhost:3000/", "http://admin.localhost:4100/"],
  ])("moves %s to the forward's local port", (url, expected) => {
    const forward = { environmentId: EnvironmentId.make("env"), remotePort: 3000, localPort: 4100 };
    expect(forwardedUrl(forward, url)).toBe(expected);
  });
});

describe("forwardPort", () => {
  it.each([
    ["LoopbackTunnelSameMachineError: same machine", true],
    ["LoopbackTunnelUnavailableError: offline", false],
  ])("tells a same-machine refusal apart (%s)", async (detail, sameMachine) => {
    const forwardPortBridge = vi
      .fn()
      .mockRejectedValue(new Error(`Error invoking remote method 'preview:forward': ${detail}`));
    vi.stubGlobal("window", { desktopBridge: { preview: { forwardPort: forwardPortBridge } } });
    const error = await forwardPort(EnvironmentId.make("env"), 3000).catch((cause) => cause);
    expect(isSameMachineForwardError(error)).toBe(sameMachine);
  });
});
