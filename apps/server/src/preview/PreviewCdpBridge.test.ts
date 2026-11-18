// @effect-diagnostics globalFetchInEffect:off - the bridge is a real loopback debug endpoint, read as a driver reads it.
import { expect, it } from "@effect/vitest";
import {
  EnvironmentId,
  PreviewAutomationNoAvailableHostError,
  ProviderInstanceId,
  ThreadId,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";

import type { PreviewAutomationInvokeInput } from "../mcp/PreviewAutomationBroker.ts";
import * as PreviewCdpBridge from "./PreviewCdpBridge.ts";
import { driverTargetId } from "./previewCdpPolicy.ts";
import { BrowserControlInterrupted } from "./SessionControl.ts";

const thread = {
  threadId: ThreadId.make("bridge-thread"),
  providerSessionId: "agent-a",
  providerInstanceId: ProviderInstanceId.make("claude"),
};
const scope = {
  environmentId: EnvironmentId.make("bridge-environment"),
  thread,
  client: undefined,
  requestNamespace: "bridge",
  capabilities: new Set(["preview", "preview-driver"] as const),
  issuedAt: 1,
};
const ownTab = { tabId: "tab_1", url: "https://example.com/", title: "Example", own: true };
const ownTarget = driverTargetId(thread.threadId, ownTab.tabId);

/** A CDP client over one socket: sends a command and returns its reply or the next event. */
const connect = (url: string) =>
  Effect.acquireRelease(
    Effect.promise(
      () =>
        new Promise<{
          readonly socket: WebSocket;
          readonly next: () => Promise<Record<string, unknown>>;
        }>((resolve, reject) => {
          const socket = new WebSocket(url);
          const received: Array<Record<string, unknown>> = [];
          let waiter: PromiseWithResolvers<Record<string, unknown>> | undefined;
          socket.addEventListener("message", ({ data }) => {
            const message = JSON.parse(String(data)) as Record<string, unknown>;
            if (waiter) {
              waiter.resolve(message);
              waiter = undefined;
            } else received.push(message);
          });
          socket.addEventListener("error", () => reject(new Error(`No socket at ${url}`)));
          socket.addEventListener("open", () =>
            resolve({
              socket,
              next: () => {
                const message = received.shift();
                if (message) return Promise.resolve(message);
                waiter = Promise.withResolvers();
                return waiter.promise;
              },
            }),
          );
        }),
    ),
    ({ socket }) => Effect.sync(() => socket.close()),
  ).pipe(
    Effect.map(({ socket, next }) => ({
      call: (id: number, method: string, params?: unknown) =>
        Effect.promise(() => {
          socket.send(JSON.stringify({ id, method, params }));
          return next();
        }),
      next: Effect.promise(next),
    })),
  );

const getJson = (url: string) =>
  Effect.promise(async () => {
    const response = await fetch(url);
    return { status: response.status, body: response.ok ? await response.json() : null };
  });

it.live("serves a thread's panel tabs as a debug endpoint only its driver can reach", () =>
  Effect.scoped(
    Effect.gen(function* () {
      const bridge = yield* PreviewCdpBridge.PreviewCdpBridge;
      const sent: Array<readonly [string, unknown]> = [];
      const invoked: Array<PreviewAutomationInvokeInput> = [];
      let emit: (method: string, params: unknown) => void = () => {};
      let humanControls = false;
      yield* bridge.bind({
        drivers: {
          tabs: ({ agentSessionId }) =>
            Effect.succeed(agentSessionId.includes("agent-a") ? [ownTab] : []),
          attach: ({ onEvent }) =>
            Effect.sync(() => {
              emit = onEvent;
              return {
                closed: new Promise<void>(() => {}),
                send: async (method, params) => {
                  if (humanControls) {
                    throw new BrowserControlInterrupted({
                      message: "A human controls this tab.",
                      reason: "humanControl",
                    });
                  }
                  sent.push([method, params]);
                  return { echoed: method };
                },
              };
            }),
        },
        broker: {
          connect: () => Effect.die("unused"),
          focusHost: () => Effect.die("unused"),
          respond: () => Effect.die("unused"),
          invoke: <A>(input: PreviewAutomationInvokeInput) => {
            invoked.push(input);
            return input.operation === "open"
              ? Effect.succeed({ tabId: "tab_2" } as A)
              : Effect.fail(
                  new PreviewAutomationNoAvailableHostError({
                    operation: input.operation,
                    environmentId: scope.environmentId,
                    threadId: thread.threadId,
                    providerSessionId: thread.providerSessionId,
                    providerInstanceId: thread.providerInstanceId,
                  }),
                );
          },
        },
      });

      const endpoint = yield* bridge.endpoint(scope);
      expect(yield* bridge.endpoint(scope)).toEqual(endpoint);
      const base = `http://${endpoint.host}:${endpoint.port}`;
      const socketBase = `ws://${endpoint.host}:${endpoint.port}${endpoint.path}`;
      expect((yield* getJson(`${base}/k/not-the-secret/json`)).status).toBe(404);
      expect((yield* getJson(`${base}/json`)).status).toBe(404);
      expect(yield* getJson(`${base}${endpoint.path}/json/version`)).toMatchObject({
        body: { webSocketDebuggerUrl: `${socketBase}/devtools/browser/onicode` },
      });
      expect((yield* getJson(`${base}${endpoint.path}/json`)).body).toEqual([
        {
          id: ownTarget,
          type: "page",
          title: "Example",
          url: "https://example.com/",
          webSocketDebuggerUrl: `${socketBase}/devtools/page/${ownTarget}`,
        },
      ]);

      const page = yield* connect(`${socketBase}/devtools/page/${ownTarget}`);
      expect(yield* page.call(1, "Runtime.evaluate", { expression: "1" })).toEqual({
        id: 1,
        result: { echoed: "Runtime.evaluate" },
      });
      emit("Page.loadEventFired", { timestamp: 2 });
      expect(yield* page.next).toEqual({ method: "Page.loadEventFired", params: { timestamp: 2 } });
      // The panel saves downloads itself, and the browser's other tabs are not this thread's.
      expect(yield* page.call(2, "Page.setDownloadBehavior", { behavior: "deny" })).toEqual({
        id: 2,
        result: {},
      });
      expect(yield* page.call(3, "Browser.close")).toMatchObject({
        id: 3,
        error: { message: "Browser.close is not available in the Browser panel." },
      });
      expect(yield* page.call(4, "Page.navigate", { url: "file:///etc/passwd" })).toMatchObject({
        id: 4,
        error: {
          message:
            "The Browser panel opens http and https pages only. To open a local file, serve its folder over http, as with `python3 -m http.server`, and open that address.",
        },
      });
      expect(yield* page.call(5, "Target.getTargets")).toMatchObject({
        id: 5,
        result: { targetInfos: [{ targetId: ownTarget, attached: true, type: "page" }] },
      });
      expect(sent).toEqual([["Runtime.evaluate", { expression: "1" }]]);

      // A driver's blank start tab is the tab the agent already has; a URL opens a panel tab.
      const browser = yield* connect(`${socketBase}/devtools/browser/onicode`);
      expect(yield* browser.call(1, "Target.createTarget", { url: "about:blank" })).toEqual({
        id: 1,
        result: { targetId: ownTarget },
      });
      expect(invoked).toEqual([]);
      expect(yield* browser.call(2, "Target.createTarget", { url: "example.org/docs" })).toEqual({
        id: 2,
        result: { targetId: driverTargetId(thread.threadId, "tab_2") },
      });
      expect(invoked).toMatchObject([
        {
          scope,
          operation: "open",
          input: { url: "https://example.org/docs", reuseExistingTab: false },
        },
      ]);
      expect(yield* browser.call(3, "Target.closeTarget", { targetId: ownTarget })).toMatchObject({
        id: 3,
        error: { message: expect.any(String) },
      });
      expect(invoked.at(-1)).toMatchObject({ operation: "close", tabId: ownTab.tabId });

      humanControls = true;
      expect(yield* page.call(6, "Input.dispatchKeyEvent", { type: "keyDown" })).toEqual({
        id: 6,
        error: { code: -32000, message: "A human controls this tab." },
      });

      // A new provider session is another agent: the old path stops answering.
      const next = yield* bridge.endpoint({
        ...scope,
        thread: { ...thread, providerSessionId: "agent-b" },
      });
      expect(next.path).not.toBe(endpoint.path);
      expect((yield* getJson(`${base}${endpoint.path}/json`)).status).toBe(404);
      expect((yield* getJson(`${base}${next.path}/json`)).body).toEqual([]);
      yield* bridge.release(thread.threadId);
      expect((yield* getJson(`${base}${next.path}/json`)).status).toBe(404);
    }),
  ).pipe(Effect.provide(PreviewCdpBridge.layer)),
);
