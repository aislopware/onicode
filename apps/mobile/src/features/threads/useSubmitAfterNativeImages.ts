import { useEffect, useEffectEvent, useRef, useState } from "react";
import type { RuntimeRequestId } from "@onicode/contracts";
import type { ActiveTurnComposerAction } from "@onicode/client-runtime/state/composer-dispatch";
import { useStableCallback } from "../../lib/useStableCallback";
import { composerDraftAbortGeneration } from "../../state/composerDraftOperations";

/** Pastes into a draft count until it is discarded or the composer leaves it for a queued edit. */
export function nativeImagePasteKey(draftKey: string): string {
  return `${composerDraftAbortGeneration(draftKey)}:${draftKey}`;
}

export function useSubmitAfterNativeImages(props: {
  readonly draftKey: string;
  readonly readResetGeneration: () => number;
  readonly blocked: boolean;
  /** Approvals shown now; a held send carries those shown when Send was pressed. */
  readonly approvalIds: ReadonlyArray<RuntimeRequestId>;
  readonly onPasteImages: (uris: ReadonlyArray<string>) => Promise<boolean>;
  readonly onSubmit: (
    followUp: ActiveTurnComposerAction | undefined,
    approvalIdsAtPress: ReadonlyArray<RuntimeRequestId>,
  ) => void;
}) {
  type HeldSend = {
    readonly draftKey: string;
    readonly generation: number;
    /** The pastes it waits on; once they are aborted, nothing later releases it. */
    readonly pasteKey: string;
    readonly approvalIds: ReadonlyArray<RuntimeRequestId>;
    readonly followUp?: ActiveTurnComposerAction;
  };
  // A paste aborted with its draft (discarded, or left for a queued edit) must not hold or drop a
  // send from the draft shown now; one that can still land in it must.
  const pastes = useRef(new Map<string, { pending: number; failed: boolean }>());
  const heldSend = useRef<HeldSend | null>(null);
  const [releasedSend, setReleasedSend] = useState<HeldSend | null>(null);
  const consumedSend = useRef<HeldSend | null>(null);
  const sendReleased = useEffectEvent((held: HeldSend) => {
    if (held.draftKey !== props.draftKey || held.generation !== props.readResetGeneration()) return;
    if (held.pasteKey !== nativeImagePasteKey(held.draftKey)) return;
    // An image pasted while the send waited on another blocker joins the turn first.
    if (pastes.current.has(held.pasteKey)) {
      if (heldSend.current === null) heldSend.current = { ...held };
      return;
    }
    props.onSubmit(held.followUp, held.approvalIds);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- submits from the render where the native images joined the draft
  useEffect(() => {
    if (!releasedSend || props.blocked || consumedSend.current === releasedSend) return;
    consumedSend.current = releasedSend;
    sendReleased(releasedSend);
  }, [releasedSend, props.blocked]);
  // Stable so the composer's send handler, which calls these, stays in the compiler's cache.
  const deferSend = useStableCallback((followUp?: ActiveTurnComposerAction) => {
    const pasteKey = nativeImagePasteKey(props.draftKey);
    if (!pastes.current.has(pasteKey)) return false;
    heldSend.current = {
      draftKey: props.draftKey,
      generation: props.readResetGeneration(),
      pasteKey,
      approvalIds: props.approvalIds,
      followUp,
    };
    return true;
  });
  const pasteImages = useStableCallback((uris: ReadonlyArray<string>) => {
    const draftKey = props.draftKey;
    const key = nativeImagePasteKey(draftKey);
    const paste = pastes.current.get(key) ?? { pending: 0, failed: false };
    pastes.current.set(key, paste);
    paste.pending += 1;
    const finish = (success: boolean) => {
      if (!success) paste.failed = true;
      paste.pending -= 1;
      if (paste.pending > 0) return;
      pastes.current.delete(key);
      const held = heldSend.current;
      if (held?.pasteKey !== key) return;
      heldSend.current = null;
      if (!paste.failed) setReleasedSend(held);
    };
    return props.onPasteImages(uris).then(finish, (error: unknown) => {
      finish(false);
      throw error;
    });
  });
  return { deferSend, pasteImages };
}
