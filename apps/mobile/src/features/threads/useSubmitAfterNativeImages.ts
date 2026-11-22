import { useEffect, useEffectEvent, useRef, useState, useSyncExternalStore } from "react";
import type { RuntimeRequestId } from "@onicode/contracts";
import type { ActiveTurnComposerAction } from "@onicode/client-runtime/state/composer-dispatch";
import { useStableCallback } from "../../lib/useStableCallback";
import {
  nativeImagePasteCount,
  nativeImagePasteKey,
  nativeImagePastePending,
  onNativeImagePastesChanged,
  onNativeImagePastesSettled,
  startNativeImagePaste,
} from "../../state/composerDraftOperations";
import type { ComposerDraftSendSettings } from "../../state/use-composer-drafts";

/** Images still converting count as content, so Send can hold a message that is only them. */
export function useNativeImagesConverting(draftKey: string): boolean {
  return useSyncExternalStore(onNativeImagePastesChanged, () =>
    nativeImagePastePending(nativeImagePasteKey(draftKey)),
  );
}

type HeldSend = {
  readonly draftKey: string;
  readonly generation: number;
  /** The pastes it waits on; once they are aborted, nothing later releases it. */
  readonly pasteKey: string;
  readonly approvalIds: ReadonlyArray<RuntimeRequestId>;
  readonly settings: ComposerDraftSendSettings;
  readonly followUp?: ActiveTurnComposerAction;
  readonly images: number;
};

// Sends still held when their composer unmounted, by draft, so a composer back on it shows them.
const leftSends = new Map<string, { readonly held: HeldSend; readonly cancel: () => void }>();
const leftSendListeners = new Set<() => void>();
const subscribeLeftSends = (listener: () => void) => {
  leftSendListeners.add(listener);
  return () => {
    leftSendListeners.delete(listener);
  };
};
const setLeftSend = (draftKey: string, send?: { held: HeldSend; cancel: () => void }) => {
  if (send) leftSends.set(draftKey, send);
  else leftSends.delete(draftKey);
  for (const changed of leftSendListeners) changed();
};

export function useSubmitAfterNativeImages(props: {
  readonly draftKey: string;
  readonly readResetGeneration: () => number;
  readonly blocked: boolean;
  /** Approvals shown now; a held send carries those shown when Send was pressed. */
  readonly approvalIds: ReadonlyArray<RuntimeRequestId>;
  readonly onPasteImages: (uris: ReadonlyArray<string>) => Promise<boolean>;
  /** Calls back when the user edits what a held send carries; returns the unsubscribe. */
  readonly watchDraft: (onEdit: () => void) => () => void;
  /** Lands input the editor took before the press, such as an autocorrection the tap accepted. */
  readonly flushInput: () => Promise<unknown>;
  readonly onSubmit: (
    followUp: ActiveTurnComposerAction | undefined,
    approvalIdsAtPress: ReadonlyArray<RuntimeRequestId>,
    settingsAtPress: ComposerDraftSendSettings,
  ) => void;
}) {
  const heldSend = useRef<HeldSend | null>(null);
  const [releasedSend, setReleasedSend] = useState<HeldSend | null>(null);
  const consumedSend = useRef<HeldSend | null>(null);
  // From the press until the send goes or is dropped, including while a blocker holds it.
  const [shownSend, setShownSend] = useState<HeldSend | null>(null);
  const watched = useRef<HeldSend | null>(null);
  const stopWatching = useRef<(() => void) | null>(null);
  const leftSend = useSyncExternalStore(subscribeLeftSends, () => leftSends.get(props.draftKey));
  const settle = () => {
    const settled = watched.current;
    watched.current = null;
    stopWatching.current?.();
    stopWatching.current = null;
    setShownSend(null);
    if (settled !== null && leftSends.get(settled.draftKey)?.held === settled) {
      setLeftSend(settled.draftKey);
    }
  };
  const cancel = useStableCallback(() => {
    heldSend.current = null;
    setReleasedSend(null);
    settle();
    leftSends.get(props.draftKey)?.cancel();
  });
  const sendReleased = useEffectEvent((held: HeldSend) => {
    if (
      held.draftKey !== props.draftKey ||
      held.generation !== props.readResetGeneration() ||
      held.pasteKey !== nativeImagePasteKey(held.draftKey)
    ) {
      settle();
      return;
    }
    // An image a closed question pasted while the send waited on another blocker joins it first.
    if (nativeImagePastePending(held.pasteKey)) {
      if (heldSend.current === null) heldSend.current = { ...held };
      return;
    }
    settle();
    props.onSubmit(held.followUp, held.approvalIds, held.settings);
  });
  const pastesSettled = useEffectEvent((key: string, failed: boolean) => {
    const held = heldSend.current;
    if (held?.pasteKey !== key) return;
    heldSend.current = null;
    if (failed) {
      settle();
      return;
    }
    setReleasedSend(held);
    // Only another blocker holds it now.
    setShownSend((shown) => shown && { ...shown, images: 0 });
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- submits from the render where the native images joined the draft
  useEffect(() => {
    if (!releasedSend || props.blocked || consumedSend.current === releasedSend) return;
    consumedSend.current = releasedSend;
    sendReleased(releasedSend);
  }, [releasedSend, props.blocked]);
  // Leaving the thread keeps a send waiting on its images, as on desktop; an edit still drops it.
  const leave = useEffectEvent(() => {
    const held = heldSend.current;
    const shown = watched.current;
    if (held === null || shown === null) {
      stopWatching.current?.();
      return;
    }
    setLeftSend(held.draftKey, { held: shown, cancel });
    const stop = onNativeImagePastesSettled((key, failed) => {
      if (key !== held.pasteKey) return;
      stop();
      // Null once Cancel or an edit dropped it.
      if (watched.current === null) return;
      settle();
      if (!failed) props.onSubmit(held.followUp, held.approvalIds, held.settings);
    });
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes to pastes settling, including a question's that this composer did not start
  useEffect(() => {
    const unsubscribe = onNativeImagePastesSettled(pastesSettled);
    return () => {
      unsubscribe();
      leave();
    };
  }, []);
  const dropLeftSend = useEffectEvent((draftKey: string) => {
    if (shownSend !== null && shownSend.draftKey !== draftKey) cancel();
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- a held send belongs to the draft it was pressed in; leaving that draft stops watching it
  useEffect(() => dropLeftSend(props.draftKey), [props.draftKey]);
  // Stable so the composer's send handler, which calls these, stays in the compiler's cache.
  const deferSend = useStableCallback(
    (followUp: ActiveTurnComposerAction | undefined, settings: ComposerDraftSendSettings) => {
      const pasteKey = nativeImagePasteKey(props.draftKey);
      if (!nativeImagePastePending(pasteKey)) return false;
      const held: HeldSend = {
        draftKey: props.draftKey,
        generation: props.readResetGeneration(),
        pasteKey,
        approvalIds: props.approvalIds,
        settings,
        followUp,
        images: nativeImagePasteCount(pasteKey),
      };
      heldSend.current = held;
      stopWatching.current?.();
      stopWatching.current = null;
      watched.current = held;
      setShownSend(held);
      // Input the press itself made land is not an edit, so watching starts after it.
      void props.flushInput().then(
        () => {
          if (watched.current === held) stopWatching.current = props.watchDraft(cancel);
        },
        () => {
          if (watched.current === held) cancel();
        },
      );
      return true;
    },
  );
  const convertImages = useStableCallback((uris: ReadonlyArray<string>) => {
    const finish = startNativeImagePaste(props.draftKey, uris.length);
    return props.onPasteImages(uris).then(finish, (error: unknown) => {
      finish(false);
      throw error;
    });
  });
  const pasteImages = useStableCallback((uris: ReadonlyArray<string>) => {
    // Another image changes what the held send would carry, as an edit does.
    if (watched.current !== null || leftSends.has(props.draftKey)) cancel();
    return convertImages(uris);
  });
  /** iOS reads a large pasteboard image off the main thread, so Send can come before its files. */
  const readPastedImages = useStableCallback((images: number) => {
    if (watched.current !== null || leftSends.has(props.draftKey)) cancel();
    const finishRead = startNativeImagePaste(props.draftKey, images);
    return (uris: ReadonlyArray<string>) => {
      // Conversion takes over the hold before the read lets go of it.
      if (uris.length > 0) void convertImages(uris);
      finishRead(uris.length > 0);
    };
  });
  const held =
    shownSend?.draftKey === props.draftKey
      ? { images: shownSend.images }
      : leftSend
        ? { images: leftSend.held.images }
        : null;
  return { deferSend, pasteImages, readPastedImages, held, cancel };
}
