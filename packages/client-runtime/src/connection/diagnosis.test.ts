import { EnvironmentAuthInvalidError } from "@onicode/contracts";
import { describe, expect, it } from "@effect/vitest";

import { presentConnectionDiagnosis } from "./diagnosis.ts";
import { environmentMismatchError, mapRemoteEnvironmentError } from "./errors.ts";
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

  it("tells a host that refused this device's login to sign in again, not to start Onicode", () => {
    const failure = mapRemoteEnvironmentError(
      new EnvironmentAuthInvalidError({
        code: "auth_invalid",
        reason: "invalid_credential",
        traceId: "trace-1",
      }),
    );
    const steps = diagnose(backoff(failure))?.steps.join(" ");
    expect(steps).not.toContain("Make sure Onicode is running");
    expect(steps).toMatch(/pair|sign in/i);
  });

  it("says another machine answered when the address reaches a different Onicode", () => {
    const state = backoff(environmentMismatchError());
    const diagnosis = diagnose(state);
    expect(diagnosis?.headline).toBe("Another Onicode answers at the address of Mac mini");
    expect(results(state)).toEqual(["device:ok", "server:failed"]);
    expect(diagnosis?.steps.join(" ")).toMatch(/another network/);
    expect(diagnosis?.steps.join(" ")).toContain("remove Mac mini");
    // This desktop app's own server cannot be removed.
    const primary = presentConnectionDiagnosis({ hostLabel: "Mac mini", state, primary: true });
    expect(primary?.steps.join(" ")).not.toMatch(/remove/i);
  });

  it("does not blame a host that answered for not running", () => {
    const failure = new ConnectionBlockedError({
      reason: "configuration",
      detail: "The environment endpoint could not be found.",
      hop: "server",
    });
    expect(diagnose(backoff(failure))?.headline).toBe("Could not connect to Onicode on Mac mini");
    expect(diagnose(backoff(failure))?.steps.join(" ")).not.toContain("running");
  });

  it("does not report hops as working when the failure names none", () => {
    const failure = new ConnectionBlockedError({
      reason: "authentication",
      detail: "The environment credential is invalid.",
    });
    expect(results(backoff(failure))).not.toContainEqual(expect.stringMatching(/:ok$/));
  });
});
