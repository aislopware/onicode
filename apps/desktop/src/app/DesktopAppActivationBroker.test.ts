import { ProjectId, ThreadId, type DesktopAppActivationRequest } from "@onicode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import { DesktopAppActivationBroker } from "./DesktopAppActivationBroker.ts";

function renderer() {
  return { send: vi.fn(), abandon: vi.fn() };
}

const request: DesktopAppActivationRequest = {
  version: 1,
  requestId: "request-1",
  type: "open-workspace",
  workspaceRoot: "/workspace/project",
  platform: "linux",
};

describe("DesktopAppActivationBroker", () => {
  it("focuses immediately and waits for renderer readiness", async () => {
    const activate = vi.fn();
    const port = renderer();
    const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate });

    const response = broker.request(request);
    expect(activate).toHaveBeenCalledOnce();
    expect(port.send).not.toHaveBeenCalled();

    broker.registerRenderer(port);
    expect(port.send).toHaveBeenCalledWith(request);
    broker.complete({
      version: 1,
      requestId: request.requestId,
      ok: true,
      projectId: ProjectId.make("project-1"),
      threadId: ThreadId.make("thread-1"),
    });

    await expect(response).resolves.toMatchObject({ ok: true, projectId: "project-1" });
    expect(port.abandon).not.toHaveBeenCalled();
    broker.close();
  });

  it.each([
    { name: "times out", requestTimeoutMs: 0, giveUp: () => undefined },
    {
      name: "is canceled while the handler remounts",
      requestTimeoutMs: 1_000,
      giveUp: (broker: DesktopAppActivationBroker) => {
        broker.pauseRenderer();
        broker.cancel(request.requestId);
      },
    },
  ])(
    "tells the page it stopped waiting when a sent request $name",
    async ({ requestTimeoutMs, giveUp }) => {
      const port = renderer();
      const broker = new DesktopAppActivationBroker({ requestTimeoutMs, activate: vi.fn() });
      broker.registerRenderer(port);

      const response = broker.request(request);
      giveUp(broker);

      await expect(response).resolves.toMatchObject({ ok: false });
      expect(port.abandon).toHaveBeenCalledWith(request.requestId);
      broker.close();
    },
  );

  it("fails an in-flight request when the renderer goes away", async () => {
    const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
    broker.registerRenderer(renderer());

    const response = broker.request(request);
    broker.clearRenderer();

    await expect(response).resolves.toMatchObject({
      ok: false,
      code: "renderer-unavailable",
    });
    broker.close();
  });

  it("lets an in-flight request finish when the renderer only stops taking new ones", async () => {
    const port = renderer();
    const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
    broker.registerRenderer(port);
    const secondRequest = { ...request, requestId: "request-2" };

    const response = broker.request(request);
    // The page stays loaded; its handler unmounted or is waiting for the connection.
    broker.pauseRenderer();
    const secondResponse = broker.request(secondRequest);
    broker.complete({
      version: 1,
      requestId: request.requestId,
      ok: true,
      projectId: ProjectId.make("project-1"),
      threadId: ThreadId.make("thread-1"),
    });

    await expect(response).resolves.toMatchObject({ ok: true });
    expect(port.send).toHaveBeenCalledTimes(1);
    broker.registerRenderer(port);
    expect(port.send).toHaveBeenLastCalledWith(secondRequest);
    broker.close();
    await expect(secondResponse).resolves.toMatchObject({ ok: false });
  });

  it("never sends a canceled request that was queued behind another request", async () => {
    const port = renderer();
    const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
    broker.registerRenderer(port);
    const secondRequest = { ...request, requestId: "request-2" };

    const firstResponse = broker.request(request);
    const secondResponse = broker.request(secondRequest);
    expect(port.send).toHaveBeenCalledTimes(1);
    expect(port.send).toHaveBeenLastCalledWith(request);

    broker.cancel(secondRequest.requestId);
    broker.complete({
      version: 1,
      requestId: request.requestId,
      ok: true,
      projectId: ProjectId.make("project-1"),
      threadId: ThreadId.make("thread-1"),
    });

    await expect(firstResponse).resolves.toMatchObject({ ok: true });
    await expect(secondResponse).resolves.toMatchObject({ ok: false });
    expect(port.send).toHaveBeenCalledTimes(1);
    // The page never saw it.
    expect(port.abandon).not.toHaveBeenCalled();
    broker.close();
  });
});
