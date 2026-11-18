import { useEffect, useEffectEvent, useRef, useState } from "react";
import type { ActiveTurnComposerAction } from "@onicode/client-runtime/state/composer-dispatch";
import { useStableCallback } from "../../lib/useStableCallback";

export function useSubmitAfterNativeImages(props: {
  readonly draftKey: string;
  readonly readResetGeneration: () => number;
  readonly blocked: boolean;
  readonly onPasteImages: (uris: ReadonlyArray<string>) => Promise<boolean>;
  readonly onSubmit: (followUp?: ActiveTurnComposerAction) => void;
}) {
  type HeldSend = {
    readonly draftKey: string;
    readonly generation: number;
    readonly followUp?: ActiveTurnComposerAction;
  };
  const pendingCount = useRef(0);
  const failed = useRef(false);
  const heldSend = useRef<HeldSend | null>(null);
  const [releasedSend, setReleasedSend] = useState<HeldSend | null>(null);
  const consumedSend = useRef<HeldSend | null>(null);
  const sendReleased = useEffectEvent((held: HeldSend) => {
    if (held.draftKey !== props.draftKey || held.generation !== props.readResetGeneration()) return;
    // An image pasted while the send waited on another blocker joins the turn first.
    if (pendingCount.current > 0) {
      if (heldSend.current === null) heldSend.current = { ...held };
      return;
    }
    props.onSubmit(held.followUp);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- submits from the render where the native images joined the draft
  useEffect(() => {
    if (!releasedSend || props.blocked || consumedSend.current === releasedSend) return;
    consumedSend.current = releasedSend;
    sendReleased(releasedSend);
  }, [releasedSend, props.blocked]);
  // Stable so the composer's send handler, which calls these, stays in the compiler's cache.
  const deferSend = useStableCallback((followUp?: ActiveTurnComposerAction) => {
    if (pendingCount.current === 0) return false;
    heldSend.current = {
      draftKey: props.draftKey,
      generation: props.readResetGeneration(),
      followUp,
    };
    return true;
  });
  const pasteImages = useStableCallback((uris: ReadonlyArray<string>) => {
    if (pendingCount.current === 0) failed.current = false;
    pendingCount.current += 1;
    const finish = (success: boolean) => {
      if (!success) failed.current = true;
      pendingCount.current -= 1;
      if (pendingCount.current === 0) {
        const held = heldSend.current;
        heldSend.current = null;
        if (held && !failed.current) setReleasedSend(held);
      }
    };
    return props.onPasteImages(uris).then(finish, (error: unknown) => {
      finish(false);
      throw error;
    });
  });
  return { deferSend, pasteImages };
}
