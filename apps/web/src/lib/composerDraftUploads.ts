import type {
  EnvironmentId,
  ScopedProjectRef,
  ScopedThreadRef,
  ThreadId,
} from "@onicode/contracts";
import { scopedThreadKey } from "@onicode/client-runtime/environment";

import { type DraftId, useComposerDraftStore } from "../composerDraftStore";
import { releaseDraftAttachments } from "./attachmentUploadQueue";
import { closeDraftThreadTerminals } from "./closeDraftThreadTerminals";

export function releaseComposerDraftUploads(target: ScopedThreadRef | DraftId): void {
  const draft = useComposerDraftStore.getState().getComposerDraft(target);
  if (draft) {
    releaseDraftAttachments([...draft.images, ...draft.files]);
  }
}

type DraftThread = { readonly environmentId: EnvironmentId; readonly threadId: ThreadId };

/**
 * Frees a removed project's draft uploads and returns its unsent draft threads, for
 * `closeUnreachableDraftThreadTerminals` once the caller has moved or cleared their sessions.
 */
export function releaseProjectDrafts(
  projectRef: ScopedProjectRef,
  projectThreadRefs: ReadonlyArray<ScopedThreadRef> = [],
): ReadonlyArray<DraftThread> {
  const store = useComposerDraftStore.getState();
  const draftThreads: DraftThread[] = [];
  for (const [draftKey, session] of Object.entries(store.draftThreadsByThreadKey)) {
    if (
      session.environmentId === projectRef.environmentId &&
      session.projectId === projectRef.projectId
    ) {
      const draft = store.draftsByThreadKey[draftKey];
      releaseDraftAttachments(draft ? [...draft.images, ...draft.files] : []);
      if (session.promotedTo == null) {
        draftThreads.push({ environmentId: session.environmentId, threadId: session.threadId });
      }
    }
  }
  for (const threadRef of projectThreadRefs) {
    const draft = store.draftsByThreadKey[scopedThreadKey(threadRef)];
    if (draft) {
      releaseDraftAttachments([...draft.images, ...draft.files]);
    }
  }
  return draftThreads;
}

/** A draft thread's terminals are reachable only through a draft session for that thread. */
export function closeUnreachableDraftThreadTerminals(
  draftThreads: ReadonlyArray<DraftThread>,
): void {
  const sessions = Object.values(useComposerDraftStore.getState().draftThreadsByThreadKey);
  for (const { environmentId, threadId } of draftThreads) {
    if (
      !sessions.some(
        (session) => session.environmentId === environmentId && session.threadId === threadId,
      )
    ) {
      closeDraftThreadTerminals(environmentId, threadId);
    }
  }
}
