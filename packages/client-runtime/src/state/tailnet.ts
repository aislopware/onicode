import { WS_METHODS } from "@onicode/contracts";
import { Atom } from "effect/reactivity";

import type { EnvironmentRegistry } from "../connection/registry.ts";
import { createEnvironmentRpcQueryAtomFamily } from "./runtime.ts";

export function createTailnetEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
) {
  return {
    devices: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:tailnet:devices",
      tag: WS_METHODS.tailnetListDevices,
      // Listing probes every online device, so a list stays fresh briefly.
      staleTimeMs: 10_000,
    }),
  };
}
