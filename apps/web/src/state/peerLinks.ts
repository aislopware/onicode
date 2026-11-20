import { useAtomValue } from "@effect/atom-react";
import {
  linkableMachines,
  linkedEnvironmentNames,
  linkNamesQuery,
  offersLinkedEnvironments,
} from "@onicode/client-runtime/link-request";
import { createPeerLinkEnvironmentAtoms } from "@onicode/client-runtime/state/peerLinks";
import { AuthAccessReadScope, type EnvironmentId } from "@onicode/contracts";
import { Atom } from "effect/reactivity";

import { connectionAtomRuntime } from "../connection/runtime";
import { useEnvironmentIdentities, useEnvironments } from "./environments";
import { environmentPresentations } from "./presentation";
import { useEnvironmentQuery } from "./query";
import { useEnvironmentsWithScope } from "./session";

export const peerLinkEnvironment = createPeerLinkEnvironmentAtoms(connectionAtomRuntime);

/** Whether `environmentId` offers links, so its threads may name another machine. */
export const offersLinkedEnvironmentsAtom = Atom.family((environmentId: EnvironmentId) =>
  Atom.make((get) =>
    offersLinkedEnvironments(get(environmentPresentations.serverConfigsAtom).get(environmentId)),
  ).pipe(Atom.withLabel(`peer-links:offered:${environmentId}`)),
);

const linkNamesQueryAtom = Atom.family((environmentId: EnvironmentId) =>
  Atom.make((get) =>
    linkNamesQuery(get(environmentPresentations.serverConfigsAtom).get(environmentId)),
  ).pipe(Atom.withLabel(`peer-links:names-query:${environmentId}`)),
);

const noLinkNamesQueryAtom = Atom.make(null).pipe(Atom.withLabel("peer-links:names-query:none"));

/**
 * The machines `environmentId` links to, by name, while it offers links, and none otherwise.
 * Its agents can read threads there, and so can a client allowed to read threads on it.
 */
export function useLinkedEnvironments(
  environmentId: EnvironmentId | null,
): ReadonlyMap<EnvironmentId, string> {
  const query = useAtomValue(
    environmentId === null ? noLinkNamesQueryAtom : linkNamesQueryAtom(environmentId),
  );
  const identities = useEnvironmentIdentities();
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
  return linkedEnvironmentNames({
    links:
      query === "names"
        ? (names.data?.links ?? [])
        : query === "list"
          ? (list.data?.links ?? [])
          : [],
    environments: identities,
  });
}

export function usePeerLinkHosts() {
  const { environments } = useEnvironments();
  const supported = environments.filter(
    (environment) =>
      environment.connection.phase === "connected" &&
      offersLinkedEnvironments(environment.serverConfig),
  );
  const readable = useEnvironmentsWithScope(supported, AuthAccessReadScope);
  return supported.filter((environment) => readable.has(environment.environmentId));
}

/**
 * The user's other saved machines, for picking one to link `environmentId` to.
 * Reads that environment's links only while `enabled`, since listing them asks
 * every linked machine whether it answers.
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
