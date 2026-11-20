import {
  linkableMachines,
  linkedEnvironmentNames,
  linkNamesQuery,
} from "@onicode/client-runtime/link-request";
import { createPeerLinkEnvironmentAtoms } from "@onicode/client-runtime/state/peerLinks";
import type { EnvironmentId } from "@onicode/contracts";
import { useMemo } from "react";

import { connectionAtomRuntime } from "../connection/runtime";
import { useEnvironments } from "./environments";
import { useEnvironmentQuery } from "./query";

export const peerLinkEnvironment = createPeerLinkEnvironmentAtoms(connectionAtomRuntime);

/**
 * The machines `environmentId` links to, by name, while it offers links, and none otherwise.
 * Its agents can read threads there, and so can a client allowed to read threads on it.
 */
export function useLinkedEnvironments(
  environmentId: EnvironmentId | null,
): ReadonlyMap<EnvironmentId, string> {
  const { environments } = useEnvironments();
  const query = linkNamesQuery(
    environments.find((environment) => environment.environmentId === environmentId)?.serverConfig,
  );
  const names = useEnvironmentQuery(
    query === "names" && environmentId !== null
      ? peerLinkEnvironment.names({ environmentId, input: {} })
      : null,
  );
  const list = useEnvironmentQuery(
    query === "list" && environmentId !== null
      ? peerLinkEnvironment.list({ environmentId, input: {} })
      : null,
  );
  return useMemo(
    () =>
      linkedEnvironmentNames({
        links:
          query === "names"
            ? (names.data?.links ?? [])
            : query === "list"
              ? (list.data?.links ?? [])
              : [],
        environments,
      }),
    [environments, names.data, list.data, query],
  );
}

/**
 * The user's other machines, for picking one to link `environmentId` to. Reads that
 * environment's links only while `enabled`, since listing them asks every linked machine
 * whether it answers.
 */
export function useLinkableMachines(environmentId: EnvironmentId, enabled: boolean) {
  const { environments } = useEnvironments();
  const links = useEnvironmentQuery(
    enabled ? peerLinkEnvironment.list({ environmentId, input: {} }) : null,
  );
  return linkableMachines({
    environments,
    threadEnvironmentId: environmentId,
    links: links.data?.links ?? [],
  });
}
