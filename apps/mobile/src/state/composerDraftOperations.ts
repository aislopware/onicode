const operationsByDraftKey = new Map<string, Set<AbortController>>();
const abortGenerations = new Map<string, number>();

/**
 * Async work that writes into one draft (a context import, a picker) starts
 * here. Whoever discards that draft or moves the composer off it aborts the
 * work, so a late result never lands in a draft the user has left.
 */
export function startComposerDraftOperation(draftKey: string): {
  readonly signal: AbortSignal;
  readonly finish: () => void;
} {
  const controller = new AbortController();
  const operations = operationsByDraftKey.get(draftKey) ?? new Set();
  operations.add(controller);
  operationsByDraftKey.set(draftKey, operations);
  return {
    signal: controller.signal,
    finish: () => {
      operations.delete(controller);
      if (operations.size === 0 && operationsByDraftKey.get(draftKey) === operations) {
        operationsByDraftKey.delete(draftKey);
      }
    },
  };
}

/** Advances on each abort, so work keyed by it can tell aborted runs from ones still landing. */
export function composerDraftAbortGeneration(draftKey: string): number {
  return abortGenerations.get(draftKey) ?? 0;
}

export function abortComposerDraftOperations(draftKey: string): void {
  const operations = operationsByDraftKey.get(draftKey);
  if (!operations) return;
  const pasteKey = nativeImagePasteKey(draftKey);
  operationsByDraftKey.delete(draftKey);
  abortGenerations.set(draftKey, composerDraftAbortGeneration(draftKey) + 1);
  for (const controller of operations) controller.abort();
  // Its pastes can no longer land, so a send held for them drops now, not once they finish.
  if (nativeImagePastes.delete(pasteKey)) settleNativeImagePaste(pasteKey, true);
}

export function abortComposerDraftOperationsWhere(matches: (draftKey: string) => boolean): void {
  // Deleting the current entry while iterating a Map is safe.
  for (const draftKey of operationsByDraftKey.keys()) {
    if (matches(draftKey)) abortComposerDraftOperations(draftKey);
  }
}

/** Pastes into a draft count until it is discarded or the composer leaves it for a queued edit. */
export function nativeImagePasteKey(draftKey: string): string {
  return `${composerDraftAbortGeneration(draftKey)}:${draftKey}`;
}

// A paste aborted with its draft (discarded, or left for a queued edit) must not hold or drop a
// send from the draft shown now; one that can still land in it must.
// `holds` counts the pastes and picks still landing; `images` only the images they named.
const nativeImagePastes = new Map<string, { holds: number; images: number; failed: boolean }>();
const nativeImagePasteListeners = new Set<(pasteKey: string, failed: boolean) => void>();
const nativeImagePasteChangeListeners = new Set<() => void>();
const notifyNativeImagePastesChanged = () => {
  for (const changed of nativeImagePasteChangeListeners) changed();
};
const settleNativeImagePaste = (pasteKey: string, failed: boolean) => {
  notifyNativeImagePastesChanged();
  for (const settled of nativeImagePasteListeners) settled(pasteKey, failed);
};

/**
 * Images converting into a draft, a closed question's included, or a pick whose files are still
 * landing (`images` 0, as its count is unknown), that a send waits for.
 */
export function startNativeImagePaste(draftKey: string, images = 1): (success: boolean) => void {
  const key = nativeImagePasteKey(draftKey);
  const existing = nativeImagePastes.get(key);
  const paste = existing ?? { holds: 0, images: 0, failed: false };
  nativeImagePastes.set(key, paste);
  paste.holds += 1;
  paste.images += images;
  if (existing === undefined) notifyNativeImagePastesChanged();
  return (success) => {
    if (!success) paste.failed = true;
    paste.holds -= 1;
    paste.images -= images;
    if (paste.holds > 0 || nativeImagePastes.get(key) !== paste) return;
    nativeImagePastes.delete(key);
    settleNativeImagePaste(key, paste.failed);
  };
}

export function nativeImagePastePending(pasteKey: string): boolean {
  return nativeImagePastes.has(pasteKey);
}

export function nativeImagePasteCount(pasteKey: string): number {
  return nativeImagePastes.get(pasteKey)?.images ?? 0;
}

export function onNativeImagePastesSettled(
  listener: (pasteKey: string, failed: boolean) => void,
): () => void {
  nativeImagePasteListeners.add(listener);
  return () => {
    nativeImagePasteListeners.delete(listener);
  };
}

/** Calls back when a draft starts or stops having images converting into it. */
export function onNativeImagePastesChanged(listener: () => void): () => void {
  nativeImagePasteChangeListeners.add(listener);
  return () => {
    nativeImagePasteChangeListeners.delete(listener);
  };
}
