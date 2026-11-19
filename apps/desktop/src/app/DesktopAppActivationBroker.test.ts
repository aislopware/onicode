import { ProjectId, ThreadId, type DesktopAppActivationRequest } from "@onicode/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { DesktopAppActivationBroker } from "./DesktopAppActivationBroker.ts";

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
    const send = vi.fn();
    const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate });

    const response = broker.request(request);
    expect(activate).toHaveBeenCalledOnce();
    expect(send).not.toHaveBeenCalled();

    broker.registerRenderer(send);
    expect(send).toHaveBeenCalledWith(request);
    broker.complete({
      version: 1,
      requestId: request.requestId,
      ok: true,
      projectId: ProjectId.make("project-1"),
      threadId: ThreadId.make("thread-1"),
    });

    await expect(response).resolves.toMatchObject({ ok: true, projectId: "project-1" });
    broker.close();
  });

  describe("claims", () => {
    beforeEach(() => {
      vi.useFakeTimers();
    });
    afterEach(() => {
      vi.useRealTimers();
    });

    it.each([
      { name: "is sent and unanswered", giveUp: () => undefined, claimed: true },
      {
        name: "timed out",
        giveUp: () => {
          vi.advanceTimersByTime(1_000);
        },
        claimed: false,
      },
      {
        name: "was canceled while the handler remounted",
        giveUp: (broker: DesktopAppActivationBroker) => {
          broker.pauseRenderer();
          broker.cancel(request.requestId);
        },
        claimed: false,
      },
    ])("lets the page claim a request only while it $name", ({ giveUp, claimed }) => {
      const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
      broker.registerRenderer(vi.fn());

      void broker.request(request);
      giveUp(broker);

      expect(broker.claim(request.requestId)).toBe(claimed);
      broker.close();
    });

    it("refuses a claim for a request the page was never sent", () => {
      const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
      broker.registerRenderer(vi.fn());
      const secondRequest = { ...request, requestId: "request-2" };

      void broker.request(request);
      void broker.request(secondRequest);

      expect(broker.claim(secondRequest.requestId)).toBe(false);
      broker.close();
    });

    it("still times out a claimed request the page never answers", async () => {
      const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
      broker.registerRenderer(vi.fn());

      const response = broker.request(request);
      expect(broker.claim(request.requestId)).toBe(true);
      vi.advanceTimersByTime(1_000);

      await expect(response).resolves.toMatchObject({ ok: false, code: "request-timeout" });
      broker.close();
    });
  });

  it("fails an in-flight request when the renderer goes away", async () => {
    const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
    broker.registerRenderer(vi.fn());

    const response = broker.request(request);
    broker.clearRenderer();

    await expect(response).resolves.toMatchObject({
      ok: false,
      code: "renderer-unavailable",
    });
    broker.close();
  });

  it("lets an in-flight request finish when the renderer only stops taking new ones", async () => {
    const send = vi.fn();
    const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
    broker.registerRenderer(send);
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
    expect(send).toHaveBeenCalledTimes(1);
    broker.registerRenderer(send);
    expect(send).toHaveBeenLastCalledWith(secondRequest);
    broker.close();
    await expect(secondResponse).resolves.toMatchObject({ ok: false });
  });

  it("never sends a canceled request that was queued behind another request", async () => {
    const send = vi.fn();
    const broker = new DesktopAppActivationBroker({ requestTimeoutMs: 1_000, activate: vi.fn() });
    broker.registerRenderer(send);
    const secondRequest = { ...request, requestId: "request-2" };

    const firstResponse = broker.request(request);
    const secondResponse = broker.request(secondRequest);
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenLastCalledWith(request);

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
    expect(send).toHaveBeenCalledTimes(1);
    // The page never saw it.
    broker.close();
  });
});
