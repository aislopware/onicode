import { createDeviceEnvironmentAtoms } from "@onicode/client-runtime/state/device";
import { resolveDeviceHubAccess } from "@onicode/client-runtime/state/deviceHubAccess";
import type { EnvironmentId } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import { Atom } from "effect/reactivity";

import { connectionAtomRuntime } from "../connection/runtime";
import { useEnvironmentQuery } from "./query";
import { environmentSession, usePreparedConnection } from "./session";

export const deviceEnvironment = createDeviceEnvironmentAtoms(connectionAtomRuntime);

// Hub tickets are single use, so each viewer resolves its own access and never reuses a spent
// ticket. Keyed by the pair: a nested family is held only weakly, and a collected one would
// resolve a second ticket for a viewer that already has one.
const deviceHubAccessAtom = Atom.family(
  ([environmentId, _viewer]: readonly [EnvironmentId, string]) =>
    connectionAtomRuntime
      .atom((get) => {
        const prepared = Option.getOrNull(
          get(environmentSession.preparedConnectionValueAtom(environmentId)),
        );
        return prepared === null
          ? Effect.never
          : resolveDeviceHubAccess({ prepared, hubBasePath: "/api/device-hub" });
      })
      .pipe(Atom.withLabel(`mobile-device-hub-access:${environmentId}`)),
);

export function useDeviceHubAccess(environmentId: EnvironmentId, hostId: string, viewer: string) {
  const prepared = usePreparedConnection(environmentId);
  const query = useEnvironmentQuery(deviceHubAccessAtom([environmentId, viewer] as const));
  const access =
    query.data && query.error === null && Option.isSome(prepared)
      ? { ...query.data, query: { ...query.data.query, hostId } }
      : null;
  return { access, error: query.error, refresh: query.refresh };
}
