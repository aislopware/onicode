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
  startComposerDraftOperation,
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

type ParkedSend = {
  readonly held: HeldSend;
  /** Its images joined the draft; it waits only for a composer to send it. */
  ready: boolean;
  /** Ends the edit and paste watches that drop it while no composer holds it. */
  readonly stop: () => void;
};

// Sends whose composer unmounted, by draft. The thread screen owns the send path, so the next
// composer on the draft adopts and sends it; an edit, a failed image or a discard drops it.
const parkedSends = new Map<string, ParkedSend>();
const parkedSendListeners = new Set<() => void>();
const subscribeParkedSends = (listener: () => void) => {
  parkedSendListeners.add(listener);
  return () => {
    parkedSendListeners.delete(listener);
  };
};
const unpark = (draftKey: string, parked: ParkedSend) => {
  if (parkedSends.get(draftKey) !== parked) return;
  parkedSends.delete(draftKey);
  parked.stop();
  for (const changed of parkedSendListeners) changed();
};
const park = (held: HeldSend, ready: boolean, watchDraft: (onEdit: () => void) => () => void) => {
  const previous = parkedSends.get(held.draftKey);
  if (previous) unpark(held.draftKey, previous);
  const stopWatchingDraft = watchDraft(() => unpark(held.draftKey, parked));
  const stopWatchingPastes = onNativeImagePastesSettled((key, failed) => {
    if (key !== held.pasteKey) return;
    if (failed) unpark(held.draftKey, parked);
    else parked.ready = true;
  });
  const parked: ParkedSend = {
    held,
    ready,
    stop: () => {
      stopWatchingDraft();
      stopWatchingPastes();
    },
  };
  parkedSends.set(held.draftKey, parked);
  for (const changed of parkedSendListeners) changed();
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
  const parkedSend = useSyncExternalStore(subscribeParkedSends, () =>
    parkedSends.get(props.draftKey),
  );
  const settle = () => {
    watched.current = null;
    stopWatching.current?.();
    stopWatching.current = null;
    setShownSend(null);
  };
  const cancel = useStableCallback(() => {
    heldSend.current = null;
    setReleasedSend(null);
    settle();
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
  // Leaving the thread keeps a held send for the composer that comes back to its draft.
  const leave = useEffectEvent(() => {
    const shown = watched.current;
    // Watching starts once the press's own input landed; before that an edit could go unseen.
    const watching = stopWatching.current !== null;
    settle();
    if (shown === null || !watching) return;
    const unsent = releasedSend !== null && consumedSend.current !== releasedSend;
    if (heldSend.current === null && !unsent) return;
    park(shown, heldSend.current === null, props.watchDraft);
    // The parked send is the only copy, so a remount's paste listener cannot release it again.
    heldSend.current = null;
    setReleasedSend(null);
  });
  const adopt = useEffectEvent((draftKey: string) => {
    const parked = parkedSends.get(draftKey);
    if (!parked) return;
    unpark(draftKey, parked);
    // A discard while it was parked dropped it, so the draft is the one Send was pressed in.
    const held: HeldSend = { ...parked.held, generation: props.readResetGeneration() };
    watched.current = held;
    stopWatching.current = props.watchDraft(cancel);
    setShownSend(parked.ready ? { ...held, images: 0 } : held);
    if (parked.ready) setReleasedSend(held);
    else heldSend.current = held;
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes to pastes settling, including a question's that this composer did not start
  useEffect(() => {
    const unsubscribe = onNativeImagePastesSettled(pastesSettled);
    return () => {
      unsubscribe();
      leave();
    };
  }, []);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync, react/set-state-in-effect -- takes over a send a composer on this draft left behind when it unmounted
  useEffect(() => adopt(props.draftKey), [props.draftKey]);
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
    if (watched.current !== null) cancel();
    return convertImages(uris);
  });
  const shownDraftKey = useStableCallback(() => props.draftKey);
  /** iOS reads a large pasteboard image off the main thread, so Send can come before its files. */
  const readPastedImages = useStableCallback((images: number) => {
    if (watched.current !== null) cancel();
    const draftKey = props.draftKey;
    // Discarding the draft or leaving it for a queued edit aborts the read with the draft.
    const operation = startComposerDraftOperation(draftKey);
    const finishRead = startNativeImagePaste(draftKey, images);
    return (uris: ReadonlyArray<string>) => {
      operation.finish();
      const lands = uris.length > 0 && !operation.signal.aborted && shownDraftKey() === draftKey;
      // Conversion takes over the hold before the read lets go of it.
      if (lands) void convertImages(uris);
      finishRead(lands);
    };
  });
  const held =
    shownSend?.draftKey === props.draftKey
      ? { images: shownSend.images }
      : parkedSend
        ? { images: parkedSend.ready ? 0 : parkedSend.held.images }
        : null;
  return { deferSend, pasteImages, readPastedImages, held, cancel };
}
