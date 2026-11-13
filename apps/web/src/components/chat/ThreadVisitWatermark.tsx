import { useAtomMount } from "@effect/atom-react";
import { scopedThreadKey } from "@onicode/client-runtime/environment";
import { createThreadVisitTracker } from "@onicode/client-runtime/state/thread-visits";
import { parseThreadKey, threadKey } from "@onicode/client-runtime/state/entities";
import { type ScopedThreadRef } from "@onicode/contracts";
import { Atom } from "effect/reactivity";

import { userPresentAtom } from "../../lib/userPresence";
import { useUiStateStore } from "../../uiStateStore";
import { environmentThreadShells, threadEnvironment } from "../../state/threads";

// A thread shown to an empty room has not been seen: a result that lands then stays unread, here
// and on a phone. The tracker stays mounted, so an open it has yet to send is not lost.
const attendedThreadShell = Atom.family((key: string) => {
  const ref = parseThreadKey(key);
  return Atom.make((get) =>
    get(userPresentAtom) ? get(environmentThreadShells.threadShellAtom(ref)) : null,
  ).pipe(Atom.withLabel(`attended-thread-shell:${key}`));
});

const threadVisitTracker = createThreadVisitTracker({
  threadShellAtom: (ref) => attendedThreadShell(threadKey(ref)),
  visit: threadEnvironment.visit,
  markLocalVisit: (ref, visitedAt) =>
    useUiStateStore.getState().markThreadVisited(scopedThreadKey(ref), visitedAt),
});

// The visit watermark is the one part of the chat view that follows the
// shell's updatedAt, which ticks while a run streams. The tracker subscribes to
// it outside React, so ChatView keeps reading the quiet shell.
export function ThreadVisitWatermark(props: { readonly threadRef: ScopedThreadRef }) {
  useAtomMount(threadVisitTracker(props.threadRef));
  return null;
}
