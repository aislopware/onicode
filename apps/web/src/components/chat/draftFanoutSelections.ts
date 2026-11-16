import type { EnvironmentId, ModelSelection, ThreadId } from "@onicode/contracts";
import { scopedThreadKey, scopeThreadRef } from "@onicode/client-runtime/environment";
import { Atom } from "effect/reactivity";

import { appAtomRegistry } from "../../rpc/atomRegistry";

// Kept per route so the multiple-model selections survive ChatView remounting while the user
// navigates.
export const draftFanoutSelectionsAtom = Atom.family((_routeKey: string) =>
  Atom.make<ReadonlyArray<ModelSelection> | null>(null).pipe(Atom.keepAlive),
);

/** A draft moving machine takes its multiple-model selections along. */
export function carryFanoutSelections(
  draft: { readonly environmentId: EnvironmentId; readonly threadId: ThreadId },
  environmentId: EnvironmentId,
) {
  if (draft.environmentId === environmentId) return;
  const selectionsOn = (id: EnvironmentId) =>
    draftFanoutSelectionsAtom(scopedThreadKey(scopeThreadRef(id, draft.threadId)));
  appAtomRegistry.set(
    selectionsOn(environmentId),
    appAtomRegistry.get(selectionsOn(draft.environmentId)),
  );
}
