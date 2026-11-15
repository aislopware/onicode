import type { AuthSessionState } from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import { HttpClientError, HttpClientRequest } from "effect/http";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { installEnvironmentHttpTest } from "../test/environmentHttpTest";
import { __setPrimaryHttpRunnerForTests } from "./lib/runtime";

const DESKTOP_AUTH = {
  policy: "desktop-managed-local",
  bootstrapMethods: ["desktop-bootstrap"],
} as const;

const unauthenticatedSession: AuthSessionState = { authenticated: false, auth: DESKTOP_AUTH };

const authenticatedSession: AuthSessionState = {
  authenticated: true,
  auth: DESKTOP_AUTH,
  expiresAt: DateTime.makeUnsafe("2026-04-05T00:00:00.000Z"),
};

function sequence<A>(...values: ReadonlyArray<A>) {
  let index = 0;
  return () => values[Math.min(index++, values.length - 1)]!;
}

let disposeHttpTest: (() => Promise<void>) | undefined;

async function installSessionApi(session: () => AuthSessionState) {
  const testApi = await installEnvironmentHttpTest({
    session: () => Effect.sync(session),
  });
  disposeHttpTest = testApi.dispose;
  return testApi;
}

describe("resolveInitialServerAuthGateState", () => {
  beforeEach(() => {
    vi.stubGlobal("window", { location: new URL("http://localhost/") });
  });

  afterEach(async () => {
    await disposeHttpTest?.();
    disposeHttpTest = undefined;
    const { __resetServerAuthBootstrapForTests } = await import("./environments/primary");
    __resetServerAuthBootstrapForTests();
    __setPrimaryHttpRunnerForTests();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("shares one session check between concurrent callers and caches success", async () => {
    const testApi = await installSessionApi(() => authenticatedSession);
    const { resolveInitialServerAuthGateState } = await import("./environments/primary");

    await expect(
      Promise.all([resolveInitialServerAuthGateState(), resolveInitialServerAuthGateState()]),
    ).resolves.toEqual([{ status: "authenticated" }, { status: "authenticated" }]);
    await expect(resolveInitialServerAuthGateState()).resolves.toEqual({
      status: "authenticated",
    });

    expect(testApi.calls.session).toBe(1);
  });

  it("reports a rejected desktop credential and checks again on the next load", async () => {
    const testApi = await installSessionApi(sequence(unauthenticatedSession, authenticatedSession));
    const { resolveInitialServerAuthGateState } = await import("./environments/primary");

    await expect(resolveInitialServerAuthGateState()).resolves.toEqual({
      status: "requires-auth",
      errorMessage: "The server did not accept the credential the desktop app gave it.",
    });
    await expect(resolveInitialServerAuthGateState()).resolves.toEqual({
      status: "authenticated",
    });

    expect(testApi.calls.session).toBe(2);
  });

  it("retries the session check while the backend is still unreachable", async () => {
    const testApi = await installSessionApi(() => authenticatedSession);
    // What FetchHttpClient yields when nothing listens on the backend port.
    const connectionRefused = new HttpClientError.HttpClientError({
      reason: new HttpClientError.TransportError({
        request: HttpClientRequest.get("http://localhost:3773/api/auth/session"),
        cause: new TypeError("Failed to fetch"),
      }),
    });
    let unreachableAttempts = 1;
    __setPrimaryHttpRunnerForTests((effect) => {
      if (unreachableAttempts > 0) {
        unreachableAttempts -= 1;
        return Promise.reject(connectionRefused);
      }
      return testApi.run(effect);
    });
    const { resolveInitialServerAuthGateState } = await import("./environments/primary");

    vi.useFakeTimers({ toFake: ["setTimeout", "Date"] });
    await Promise.all([
      expect(resolveInitialServerAuthGateState()).resolves.toEqual({ status: "authenticated" }),
      vi.advanceTimersByTimeAsync(1_000),
    ]);
    expect(testApi.calls.session).toBe(1);
  });
});
