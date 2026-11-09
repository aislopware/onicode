import { describe, expect, it } from "@effect/vitest";

import { presentConnectionDiagnosis } from "./diagnosis.ts";
import { mapRemoteEnvironmentError } from "./errors.ts";
import {
  AVAILABLE_CONNECTION_STATE,
  ConnectionBlockedError,
  type ConnectionAttemptError,
  type SupervisorConnectionState,
} from "./model.ts";
import { RemoteEnvironmentAuthFetchError } from "../rpc/http.ts";

const backoff = (lastFailure: ConnectionAttemptError): SupervisorConnectionState => ({
  ...AVAILABLE_CONNECTION_STATE,
  desired: true,
  network: "online",
  phase: "backoff",
  attempt: 3,
  lastFailure,
  retryAt: Date.UTC(2026, 9, 19, 12, 0, 30),
});

const diagnose = (state: SupervisorConnectionState) =>
  presentConnectionDiagnosis({ hostLabel: "Mac mini", state });

const results = (state: SupervisorConnectionState) =>
  diagnose(state)?.hops.map((hop) => `${hop.hop}:${hop.result}`);

const unreachable = () =>
  mapRemoteEnvironmentError(
    new RemoteEnvironmentAuthFetchError({ message: "Failed to fetch", cause: new TypeError() }),
  );

describe("presentConnectionDiagnosis", () => {
  it("names Onicode on the host when the request never arrives", () => {
    expect(diagnose(backoff(unreachable()))?.headline).toBe("Onicode on Mac mini did not answer");
    expect(results(backoff(unreachable()))).toEqual(["device:ok", "server:failed"]);
  });

  it("reports this device as offline before any other hop", () => {
    const state = { ...backoff(unreachable()), network: "offline" as const };
    expect(diagnose(state)?.headline).toBe("This device is offline");
    expect(results(state)?.[0]).toBe("device:failed");
  });

  it("returns nothing before any attempt fails", () => {
    expect(diagnose(AVAILABLE_CONNECTION_STATE)).toBeNull();
  });

  it("does not report hops as working when the failure names none", () => {
    const failure = new ConnectionBlockedError({
      reason: "authentication",
      detail: "The environment credential is invalid.",
    });
    expect(results(backoff(failure))).not.toContainEqual(expect.stringMatching(/:ok$/));
  });
});
