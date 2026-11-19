import { useAtomValue } from "@effect/atom-react";
import { visibleLinkedThreads } from "@onicode/client-runtime/state/threads";
import type { EnvironmentId, ThreadId } from "@onicode/contracts";
import { useNavigationState, useRoute } from "@react-navigation/native";
import type { AsyncResult, Atom } from "effect/reactivity";
import { useState } from "react";

import { useEnvironmentQuery } from "./query";
import { environmentThreadShells } from "./threads";

/**
 * The threads a pull request or an issue is linked to, as the screen showing it reads them: only
 * while its route is on top, so a screen pushed over it stops the rereads, and keeping the last
 * answer for `key` through a refresh or a dropped connection. `error` is set only while no answer
 * is known.
 */
export function useLinkedThreads<Row extends { readonly id: ThreadId }, E>(
  environmentId: EnvironmentId,
  atom: Atom.Atom<AsyncResult.AsyncResult<{ readonly threads: ReadonlyArray<Row> }, E>> | null,
  key: string,
) {
  const route = useRoute();
  // The iOS synthetic workspace parent's focus differs from the visible detail's focus.
  const focused = useNavigationState((state) => state.routes[state.index]?.key === route.key);
  const relations = useEnvironmentQuery(focused ? atom : null);
  const [last, setLast] = useState({ key, data: relations.data });
  if (relations.data && (last.key !== key || relations.data !== last.data))
    setLast({ key, data: relations.data });
  const answer = relations.data ?? (last.key === key ? last.data : null);
  const hiddenKeys = useAtomValue(environmentThreadShells.hiddenThreadKeysAtom);
  const hidingAnywhere = useAtomValue(environmentThreadShells.anyEnvironmentHidingAtom);
  const loadedThreadRefs = useAtomValue(
    environmentThreadShells.environmentThreadRefsAtom(environmentId),
  );
  return {
    threads: visibleLinkedThreads(
      answer?.threads ?? [],
      environmentId,
      loadedThreadRefs,
      hiddenKeys,
      hidingAnywhere,
    ),
    known: answer !== null,
    error: answer === null ? relations.error : null,
    isPending: relations.isPending,
    refresh: relations.refresh,
  };
}
