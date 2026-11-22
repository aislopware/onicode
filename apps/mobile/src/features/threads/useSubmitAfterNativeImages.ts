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
  /** Its images joined the draft. */
  ready: boolean;
  /** Ends the edit and paste watches that drop it while no composer holds it. */
  readonly stop: () => void;
};

// Sends whose composer unmounted, by draft. One goes through the send path its composer left once
// its images land, unless a blocker or an approval it answers needs the thread screen; then the
// next composer on the draft adopts and sends it. An edit, a failed image or a discard drops it.
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
const park = (
  held: HeldSend,
  ready: boolean,
  watchDraft: (onEdit: () => void) => () => void,
  sendAway: (() => void) | undefined,
) => {
  const previous = parkedSends.get(held.draftKey);
  if (previous) unpark(held.draftKey, previous);
  const stopWatchingDraft = watchDraft(() => unpark(held.draftKey, parked));
  const stopWatchingPastes = onNativeImagePastesSettled((key, failed) => {
    if (key !== held.pasteKey) return;
    if (failed || sendAway) unpark(held.draftKey, parked);
    else parked.ready = true;
    if (!failed) sendAway?.();
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
  /** Deletes the files of an iOS read that no longer lands in a draft. */
  readonly discardImages: (uris: ReadonlyArray<string>) => void;
  /** Calls back when the user edits what a held send carries; returns the unsubscribe. */
  readonly watchDraft: (onEdit: () => void) => () => void;
  /** Lands input the editor took before the press, such as an autocorrection the tap accepted. */
  readonly flushInput: () => Promise<unknown>;
  readonly onSubmit: (
    followUp: ActiveTurnComposerAction | undefined,
    approvalIdsAtPress: ReadonlyArray<RuntimeRequestId>,
    settingsAtPress: ComposerDraftSendSettings,
    /** Its composer left; settles once the send went or was turned down. */
    away?: boolean,
  ) => void | Promise<unknown>;
}) {
  const heldSend = useRef<HeldSend | null>(null);
  const [releasedSend, setReleasedSend] = useState<HeldSend | null>(null);
  // Set with the state, so an unmount in the same batch still sees a release that has not rendered.
  const released = useRef<HeldSend | null>(null);
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
    released.current = null;
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
    released.current = held;
    setReleasedSend(held);
    // Only another blocker holds it now.
    setShownSend((shown) => shown && { ...shown, images: 0 });
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- submits from the render where the native images joined the draft
  useEffect(() => {
    if (!releasedSend || props.blocked || consumedSend.current === releasedSend) return;
    consumedSend.current = releasedSend;
    if (released.current === releasedSend) released.current = null;
    sendReleased(releasedSend);
  }, [releasedSend, props.blocked]);
  // Leaving the thread keeps a held send, to go once its images land or from a returning composer.
  const leave = useEffectEvent(() => {
    const shown = watched.current;
    // Watching starts once the press's own input landed; before that an edit could go unseen.
    const watching = stopWatching.current !== null;
    settle();
    if (shown === null || !watching) return;
    if (heldSend.current === null && released.current === null) return;
    const ready = heldSend.current === null;
    // The parked send is the only copy, so a remount's paste listener cannot release it again.
    heldSend.current = null;
    released.current = null;
    setReleasedSend(null);
    const { onSubmit: submit, watchDraft } = props;
    // Only the thread screen sees a blocker clear, and a redirect declines its approvals there.
    const sendAway =
      props.blocked || shown.approvalIds.length > 0
        ? undefined
        : () => {
            // A send that went clears the draft; one turned down leaves it for a returning composer.
            let draftChanged = false;
            const stop = watchDraft(() => {
              draftChanged = true;
            });
            void Promise.resolve(submit(shown.followUp, shown.approvalIds, shown.settings, true))
              .catch(() => undefined)
              .then(() => {
                stop();
                // A newer press parked meanwhile is the one to keep.
                if (!draftChanged && !parkedSends.has(shown.draftKey)) {
                  park(shown, true, watchDraft, undefined);
                }
              });
          };
    // A closed question's image still converting joins a released send first.
    if (ready && sendAway && !nativeImagePastePending(shown.pasteKey)) sendAway();
    else park(shown, ready, props.watchDraft, sendAway);
  });
  // A held send belongs to the draft it was pressed in; showing another draft drops it.
  const showDraft = useEffectEvent((draftKey: string, parked: ParkedSend | undefined) => {
    if (watched.current !== null && watched.current.draftKey !== draftKey) cancel();
    // Another composer on the draft may have taken it in this commit.
    if (!parked || parkedSends.get(draftKey) !== parked) return;
    unpark(draftKey, parked);
    // Only one send can be held for a draft; this composer's own press wins.
    if (watched.current !== null) return;
    // A discard while it was parked dropped it, so the draft is the one Send was pressed in.
    const held: HeldSend = { ...parked.held, generation: props.readResetGeneration() };
    watched.current = held;
    stopWatching.current = props.watchDraft(cancel);
    setShownSend(parked.ready ? { ...held, images: 0 } : held);
    if (!parked.ready) {
      heldSend.current = held;
      return;
    }
    released.current = held;
    setReleasedSend(held);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes to pastes settling, including a question's that this composer did not start
  useEffect(() => {
    const unsubscribe = onNativeImagePastesSettled(pastesSettled);
    return () => {
      unsubscribe();
      leave();
    };
  }, []);
  // Takes over a send that another composer on this draft left, on mount or as it leaves.
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync, react/set-state-in-effect -- adopts from the module's store of sends whose composer unmounted
  useEffect(() => showDraft(props.draftKey, parkedSend), [props.draftKey, parkedSend]);
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
      else if (uris.length > 0) props.discardImages(uris);
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
