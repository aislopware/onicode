import { RegistryContext, useAtomValue } from "@effect/atom-react";
import { chooseLoadBalancedEnvironment } from "@onicode/client-runtime/load-balancing";
import { executeAtomQuery } from "@onicode/client-runtime/state/runtime";
import type { EnvironmentId } from "@onicode/contracts";
import { Atom } from "effect/reactivity";
import { useContext } from "react";

import { serverEnvironment } from "../state/server";

// Mirrors the freshness window in chooseLoadBalancedEnvironment.
const HOST_RESOURCES_FRESH_MS = 15_000;
// The server's sample cache lifetime.
const HOST_RESOURCES_RESAMPLE_MS = 5_000;

// Keyed by content: callers derive the list every render.
export const hostResourcesAtom = Atom.family((environmentIdsKey: string) =>
  Atom.make((get) => {
    const evaluatedAt = Date.now();
    const resources = (
      environmentIdsKey === "" ? [] : (environmentIdsKey.split("\n") as EnvironmentId[])
    ).map((environmentId) => {
      const result = get(serverEnvironment.hostResources({ environmentId, input: {} }));
      return {
        environmentId,
        resources: result._tag === "Success" ? result.value : null,
        receivedAt: result._tag === "Success" ? result.timestamp : 0,
        pending: result._tag === "Initial" || result.waiting,
        failed: result._tag === "Failure",
      };
    });
    // Re-read when the next fresh snapshot ages out, so a frozen evaluatedAt never selects it.
    const nextExpiry = Math.min(
      ...resources
        .map((resource) => resource.receivedAt + HOST_RESOURCES_FRESH_MS)
        .filter((expiresAt) => expiresAt >= evaluatedAt),
    );
    if (Number.isFinite(nextExpiry)) {
      const timer = setTimeout(() => get.refreshSelf(), nextExpiry - evaluatedAt + 1);
      get.addFinalizer(() => clearTimeout(timer));
    }
    // Queries refetch only on mount, so with every answering machine busy or stale nothing would
    // sample again. One that fails, such as an older server, is not asked again every time.
    // Weight only ranks machines that pass, and candidates all have a positive one.
    const answering = resources.filter((resource) => !resource.failed);
    if (
      answering.length > 0 &&
      resources.every((resource) => !resource.pending) &&
      chooseLoadBalancedEnvironment(
        resources.map((resource) => ({ ...resource, weight: 1 })),
        evaluatedAt,
      ) === null
    ) {
      // The registry, not `get`: the first refresh rebuilds this atom and disposes `get`.
      const registry = get.registry;
      const timer = setTimeout(() => {
        Atom.batch(() => {
          for (const { environmentId } of answering) {
            registry.refresh(serverEnvironment.hostResources({ environmentId, input: {} }));
          }
        });
      }, HOST_RESOURCES_RESAMPLE_MS);
      get.addFinalizer(() => clearTimeout(timer));
    }
    return { evaluatedAt, resources };
  }),
);

// Resampling rebuilds the snapshot every few seconds; the composer only cares when the outcome moves.
export const loadBalancedSelectionAtom = Atom.family((key: string) => {
  const [environmentIdsKey = "", weightsKey = ""] = key.split("\t");
  const ids = environmentIdsKey === "" ? [] : environmentIdsKey.split("\n");
  const weightList = weightsKey === "" ? [] : weightsKey.split(",").map(Number);
  const weights = Object.fromEntries(ids.map((id, index) => [id, weightList[index] ?? 50]));
  return Atom.make((get) =>
    selectLoadBalancedEnvironment(get(hostResourcesAtom(environmentIdsKey)), weights),
  ).pipe(
    Atom.withEquality<ReturnType<typeof selectLoadBalancedEnvironment>>(
      (a, b) =>
        a.pending === b.pending && a.environmentId === b.environmentId && a.failed === b.failed,
    ),
  );
});

/** Only mounted for unresolved automatic drafts, so idle clients do not poll hosts. */
export function useLoadBalancedEnvironment(
  environmentIds: readonly EnvironmentId[],
  weights: Readonly<Record<string, number>>,
) {
  const registry = useContext(RegistryContext);
  const refresh = (ids: readonly EnvironmentId[]) => {
    for (const environmentId of ids) {
      registry.refresh(serverEnvironment.hostResources({ environmentId, input: {} }));
    }
  };
  const refreshSelection = async (ids: readonly EnvironmentId[]) => {
    const candidates = await Promise.all(
      ids.map(async (environmentId) => {
        const result = await executeAtomQuery(
          registry,
          serverEnvironment.hostResources({ environmentId, input: {} }),
          { refresh: true, reportFailure: false, signal: AbortSignal.timeout(5_000) },
        );
        return {
          environmentId,
          resources: result._tag === "Success" ? result.value : null,
          receivedAt: Date.now(),
          weight: weights[environmentId] ?? 50,
        };
      }),
    );
    return chooseLoadBalancedEnvironment(candidates, Date.now()) as EnvironmentId | null;
  };
  const selection = useAtomValue(
    loadBalancedSelectionAtom(
      `${environmentIds.join("\n")}\t${environmentIds.map((id) => weights[id] ?? 50).join(",")}`,
    ),
  );
  return { refresh, refreshSelection, ...selection };
}

export function selectLoadBalancedEnvironment(
  { evaluatedAt, resources }: Atom.Type<ReturnType<typeof hostResourcesAtom>>,
  weights: Readonly<Record<string, number>>,
) {
  const checking = resources.some((resource) => resource.pending);
  const environmentId = chooseLoadBalancedEnvironment(
    resources.map((resource) => ({
      ...resource,
      weight: weights[resource.environmentId] ?? 50,
    })),
    evaluatedAt,
  ) as EnvironmentId | null;
  // Machines that answer are sampled again while busy, so only all failing ends the check.
  const failed =
    !checking &&
    environmentId === null &&
    resources.length > 0 &&
    resources.every((resource) => resource.failed);
  return {
    // Busy machines are sampled again until one frees up, so the check is still running.
    pending: checking || (environmentId === null && !failed && resources.length > 0),
    environmentId,
    failed,
  };
}
