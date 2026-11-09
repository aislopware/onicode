import type { AuthSessionState } from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
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
});
