import { useAtomMount } from "@effect/atom-react";
import { scopedThreadKey } from "@onicode/client-runtime/environment";
import { createThreadVisitTracker } from "@onicode/client-runtime/state/thread-visits";
import { type ScopedThreadRef } from "@onicode/contracts";

import { environmentCatalog } from "../../connection/catalog";
import { userPresentAtom } from "../../lib/userPresence";
import { useUiStateStore } from "../../uiStateStore";
import { environmentThreadShells, threadEnvironment } from "../../state/threads";

const threadVisitTracker = createThreadVisitTracker({
  threadShellAtom: environmentThreadShells.threadShellAtom,
  sessionGenerationAtom: environmentCatalog.sessionGenerationAtom,
  visit: threadEnvironment.visit,
  markLocalVisit: (ref, visitedAt) =>
    useUiStateStore.getState().markThreadVisited(scopedThreadKey(ref), visitedAt),
  // A thread shown to an empty room has not been seen: a result that lands then stays unread,
  // here and on a phone.
  viewingAtom: userPresentAtom,
});

// The visit watermark is the one part of the chat view that follows the
// shell's updatedAt, which ticks while a run streams. The tracker subscribes to
// it outside React, so ChatView keeps reading the quiet shell.
export function ThreadVisitWatermark(props: { readonly threadRef: ScopedThreadRef }) {
  useAtomMount(threadVisitTracker(props.threadRef));
  return null;
}
