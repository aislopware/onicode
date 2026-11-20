import { useEffect, useEffectEvent, useRef, useState } from "react";
import type { RuntimeRequestId } from "@onicode/contracts";
import type { ActiveTurnComposerAction } from "@onicode/client-runtime/state/composer-dispatch";
import { useStableCallback } from "../../lib/useStableCallback";
import {
  nativeImagePasteKey,
  nativeImagePastePending,
  onNativeImagePastesSettled,
  startNativeImagePaste,
} from "../../state/composerDraftOperations";
import type { ComposerDraftSendSettings } from "../../state/use-composer-drafts";

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
    settingsAtPress: ComposerDraftSendSettings,
  ) => void;
}) {
  type HeldSend = {
    readonly draftKey: string;
    readonly generation: number;
    /** The pastes it waits on; once they are aborted, nothing later releases it. */
    readonly pasteKey: string;
    readonly approvalIds: ReadonlyArray<RuntimeRequestId>;
    readonly settings: ComposerDraftSendSettings;
    readonly followUp?: ActiveTurnComposerAction;
  };
  const heldSend = useRef<HeldSend | null>(null);
  const [releasedSend, setReleasedSend] = useState<HeldSend | null>(null);
  const consumedSend = useRef<HeldSend | null>(null);
  const sendReleased = useEffectEvent((held: HeldSend) => {
    if (held.draftKey !== props.draftKey || held.generation !== props.readResetGeneration()) return;
    if (held.pasteKey !== nativeImagePasteKey(held.draftKey)) return;
    // An image pasted while the send waited on another blocker joins the turn first.
    if (nativeImagePastePending(held.pasteKey)) {
      if (heldSend.current === null) heldSend.current = { ...held };
      return;
    }
    props.onSubmit(held.followUp, held.approvalIds, held.settings);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- submits from the render where the native images joined the draft
  useEffect(() => {
    if (!releasedSend || props.blocked || consumedSend.current === releasedSend) return;
    consumedSend.current = releasedSend;
    sendReleased(releasedSend);
  }, [releasedSend, props.blocked]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes to pastes settling, including a question's that this composer did not start
  useEffect(
    () =>
      onNativeImagePastesSettled((key, failed) => {
        const held = heldSend.current;
        if (held?.pasteKey !== key) return;
        heldSend.current = null;
        if (!failed) setReleasedSend(held);
      }),
    [],
  );
  // Stable so the composer's send handler, which calls these, stays in the compiler's cache.
  const deferSend = useStableCallback(
    (followUp: ActiveTurnComposerAction | undefined, settings: ComposerDraftSendSettings) => {
      const pasteKey = nativeImagePasteKey(props.draftKey);
      if (!nativeImagePastePending(pasteKey)) return false;
      heldSend.current = {
        draftKey: props.draftKey,
        generation: props.readResetGeneration(),
        pasteKey,
        approvalIds: props.approvalIds,
        settings,
        followUp,
      };
      return true;
    },
  );
  const pasteImages = useStableCallback((uris: ReadonlyArray<string>) => {
    const finish = startNativeImagePaste(props.draftKey);
    return props.onPasteImages(uris).then(finish, (error: unknown) => {
      finish(false);
      throw error;
    });
  });
  return { deferSend, pasteImages };
}
