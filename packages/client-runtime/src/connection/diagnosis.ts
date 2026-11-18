import * as DateTime from "effect/DateTime";

import { mFallback, msg } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";

import type { ConnectionAttemptError, ConnectionHop, SupervisorConnectionState } from "./model.ts";

export type ConnectionHopResult = "ok" | "failed" | "unchecked";

export interface ConnectionDiagnosisHop {
  readonly hop: ConnectionHop;
  readonly label: string;
  readonly result: ConnectionHopResult;
}

export interface ConnectionDiagnosis {
  /** Names the part that failed, e.g. "Onicode on Mac mini did not answer". */
  readonly headline: string;
  readonly hops: ReadonlyArray<ConnectionDiagnosisHop>;
  readonly steps: ReadonlyArray<string>;
  readonly facts: ReadonlyArray<readonly [label: string, value: string]>;
}

export interface ConnectionDiagnosisInput {
  readonly hostLabel: string;
  readonly state: SupervisorConnectionState;
  readonly clientVersion?: string | undefined;
  readonly serverVersion?: string | null | undefined;
}

interface Located {
  readonly hop: ConnectionHop;
  readonly cause: "offline" | "outdated" | null;
  /** The failure named no hop, so the hops before this one were never probed. */
  readonly guessed?: true;
}

function locate(input: ConnectionDiagnosisInput): Located | null {
  const failure = input.state.lastFailure;
  if (input.state.network === "offline" || input.state.phase === "offline") {
    return { hop: "device", cause: "offline" };
  }
  if (failure?._tag === "ConnectionBlockedError" && failure.serverUpdateRequired) {
    return { hop: "server", cause: "outdated" };
  }
  if (failure?.hop !== undefined) {
    return { hop: failure.hop, cause: null };
  }
  if (failure !== null) {
    return { hop: "server", cause: null, guessed: true };
  }
  return null;
}

function headline(
  host: string,
  located: Located,
  failure: ConnectionAttemptError | null,
  resolve: MFunctionType,
): string {
  switch (located.cause) {
    case "offline":
      return resolve(msg("This device is offline"));
    case "outdated":
      return resolve(msg("{host} runs an older Onicode", { host }));
    case null:
      break;
  }
  switch (located.hop) {
    case "device":
      return resolve(msg("This device could not prepare the connection"));
    case "server":
      return failure?.reason === "authentication" || failure?.reason === "permission"
        ? resolve(msg("Onicode on {host} did not accept this device", { host }))
        : resolve(msg("Onicode on {host} did not answer", { host }));
  }
}

function steps(
  host: string,
  located: Located,
  failure: ConnectionAttemptError | null,
  resolve: MFunctionType,
): string[] {
  switch (located.cause) {
    case "offline":
      return [resolve(msg("Connect this device to the network. Onicode reconnects on its own."))];
    case "outdated":
      return [resolve(msg("Update Onicode on {host}.", { host }))];
    case null:
      break;
  }
  switch (located.hop) {
    case "device":
      return [
        resolve(msg("Restart Onicode on this device.")),
        resolve(
          msg("If it keeps failing, remove {host} from this device and add it again.", { host }),
        ),
      ];
    case "server":
      return failure?.reason === "authentication"
        ? [
            resolve(
              msg(
                "This device's sign-in to {host} expired or was revoked. Pair it again with a new pairing link from {host}.",
                { host },
              ),
            ),
          ]
        : [
            resolve(msg("Make sure Onicode is running on {host}.", { host })),
            resolve(msg("Retry. If it keeps failing, copy the diagnostics.")),
          ];
  }
}

const HOP_ORDER: ReadonlyArray<ConnectionHop> = ["device", "server"];

function hopLabel(hop: ConnectionHop, host: string, resolve: MFunctionType): string {
  switch (hop) {
    case "device":
      return resolve(msg("This device"));
    case "server":
      return resolve(msg("Onicode on {host}", { host }));
  }
}

/**
 * Turns a connection state into what failed, in order, and what to try. Returns
 * null when nothing has failed. Web and mobile render this same model.
 */
export function presentConnectionDiagnosis(
  input: ConnectionDiagnosisInput,
  resolve: MFunctionType = mFallback,
): ConnectionDiagnosis | null {
  const located = locate(input);
  if (located === null) return null;
  const host = input.hostLabel;
  const failure = input.state.lastFailure;
  const failedIndex = HOP_ORDER.indexOf(located.hop);
  const hops = HOP_ORDER.map((hop, index): ConnectionDiagnosisHop => ({
    hop,
    label: hopLabel(hop, host, resolve),
    result:
      index === failedIndex
        ? "failed"
        : index < failedIndex && located.guessed !== true
          ? "ok"
          : "unchecked",
  }));

  const facts: Array<readonly [string, string]> = [];
  const add = (label: string, value: string | number | null | undefined) => {
    if (value !== null && value !== undefined && value !== "") facts.push([label, String(value)]);
  };
  add(
    resolve(msg("Cause", { $context: "diagnostics field label" })),
    located.cause ?? failure?.reason,
  );
  add(
    resolve(msg("Attempts", { $context: "diagnostics field label" })),
    input.state.attempt > 0 ? input.state.attempt : null,
  );
  add(
    resolve(msg("Next retry")),
    input.state.retryAt === null
      ? null
      : DateTime.formatIso(DateTime.makeUnsafe(input.state.retryAt)),
  );
  add(resolve(msg("App version")), input.clientVersion);
  add(resolve(msg("Host version")), input.serverVersion);
  add(resolve(msg("Error", { $context: "diagnostics field label" })), failure?.detail);
  add(resolve(msg("Trace ID")), failure?.traceId);

  return {
    headline: headline(host, located, failure, resolve),
    hops,
    steps: steps(host, located, failure, resolve),
    facts,
  };
}

const HOP_MARK = { ok: "ok", failed: "FAILED", unchecked: "not checked" } as const;

/** One plain-text block for bug reports. */
export function connectionDiagnosisText(hostLabel: string, diagnosis: ConnectionDiagnosis): string {
  return [
    `Onicode connection to ${hostLabel}: ${diagnosis.headline}`,
    "",
    ...diagnosis.hops.map((hop) => `${hop.label}: ${HOP_MARK[hop.result]}`),
    "",
    ...diagnosis.facts.map(([label, value]) => `${label}: ${value}`),
  ].join("\n");
}
