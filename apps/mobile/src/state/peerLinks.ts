import { useAtomValue } from "@effect/atom-react";
import {
  linkableMachines,
  linkedEnvironmentNames,
  linkNamesQuery,
} from "@onicode/client-runtime/link-request";
import {
  createPeerLinkEnvironmentAtoms,
  linkThroughRoutes,
  offersLinkThrough,
} from "@onicode/client-runtime/state/peerLinks";
import { AuthAccessWriteScope, type EnvironmentId } from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import { Atom } from "effect/reactivity";
import { useMemo } from "react";

import { connectionAtomRuntime } from "../connection/runtime";
import { useNowMinuteMs } from "../lib/clock";
import { useEnvironments } from "./environments";
import { useEnvironmentQuery } from "./query";
import { useEnvironmentsWithScope } from "./session";

export const peerLinkEnvironment = createPeerLinkEnvironmentAtoms(connectionAtomRuntime);

const idsOf = (key: string) => (key === "" ? [] : (key.split("\n") as EnvironmentId[]));

/** Each environment's link list, in the order of the newline-joined ids. */
const peerLinkListsAtom = Atom.family((key: string) =>
  Atom.make((get) =>
    idsOf(key).map((environmentId) => get(peerLinkEnvironment.list({ environmentId, input: {} }))),
  ).pipe(Atom.withLabel(`peer-links:lists:${key}`)),
);

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
 * The user's other saved machines, then the unsaved ones in `through`, for picking one to link
 * `environmentId` to. Reads that environment's links only while `enabled`, since listing them
 * asks every linked machine whether it answers.
 */
export function useLinkableMachines(
  environmentId: EnvironmentId,
  enabled: boolean,
  through?: ReadonlyArray<{ readonly environmentId: EnvironmentId; readonly label: string }>,
) {
  const { environments } = useEnvironments();
  const links = useEnvironmentQuery(
    enabled ? peerLinkEnvironment.list({ environmentId, input: {} }) : null,
  );
  return linkableMachines({
    environments,
    threadEnvironmentId: environmentId,
    links: links.data?.links ?? [],
    ...(through === undefined ? {} : { through }),
  });
}

/**
 * The machines `sourceId` can link to through another environment's full-access link, by
 * environment id. Reads those environments' links only while `enabled`: listing them asks every
 * linked machine whether it answers.
 */
export function useLinkThroughRoutes(sourceId: EnvironmentId, enabled: boolean) {
  const { environments } = useEnvironments();
  const offering = environments.filter(
    (environment) =>
      enabled &&
      environment.environmentId !== sourceId &&
      environment.connection.phase === "connected" &&
      offersLinkThrough(environment.serverConfig),
  );
  const offeringKey = offering.map((environment) => environment.environmentId).join("\n");
  const scoped = idsOf(offeringKey).map((environmentId) => ({ environmentId }));
  const writable = useEnvironmentsWithScope(scoped, AuthAccessWriteScope);
  const hosts = offering.filter((environment) => writable.has(environment.environmentId));
  const lists = useAtomValue(peerLinkListsAtom(hosts.map((host) => host.environmentId).join("\n")));
  const nowMinuteMs = useNowMinuteMs();
  return linkThroughRoutes({
    sourceEnvironmentId: sourceId,
    hosts: hosts.map((host, index) => {
      const list = lists[index];
      return {
        environmentId: host.environmentId,
        config: host.serverConfig,
        links: list?._tag === "Success" ? list.value.links : [],
      };
    }),
    now: DateTime.makeUnsafe(nowMinuteMs),
  });
}
