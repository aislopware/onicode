import { PROVIDER_SEND_TURN_MAX_ATTACHMENTS } from "@onicode/contracts";

import {
  type ComposerThreadTarget,
  composerTargetKey,
  resolveComposerDraftKey,
  useComposerDraftStore,
} from "../../composerDraftStore";
import { countMovedQuestionImports } from "../../questionAttachments";
import { pendingThreadImports } from "./pendingThreadImports";
import { countPreparingComposerImages } from "./preparingComposerImages";

/** Slots in a draft held for attachments still on their way into it. */
export function countReservedComposerSlots(targetKey: string): number {
  return (
    countPreparingComposerImages(targetKey) +
    (pendingThreadImports.get(targetKey) ?? 0) +
    countMovedQuestionImports(targetKey)
  );
}

/** A draft's attachments plus its reserved slots; an unsent thread keeps both under its draft id. */
export function countHeldComposerSlots(target: ComposerThreadTarget): number {
  const state = useComposerDraftStore.getState();
  const key = resolveComposerDraftKey(state, target) ?? composerTargetKey(target);
  const draft = state.draftsByThreadKey[key];
  return (draft?.images.length ?? 0) + (draft?.files.length ?? 0) + countReservedComposerSlots(key);
}

export async function importComposerThreadAttachment(input: {
  readonly targetKey: string;
  readonly pendingImports: Map<string, number>;
  readonly countReservedAttachments: () => number;
  readonly load: () => Promise<File>;
  readonly isActive: () => boolean;
  readonly attach: (file: File) => Promise<boolean>;
  readonly onLimitReached: () => void;
}): Promise<void> {
  if (input.countReservedAttachments() >= PROVIDER_SEND_TURN_MAX_ATTACHMENTS) {
    input.onLimitReached();
    return;
  }
  input.pendingImports.set(input.targetKey, (input.pendingImports.get(input.targetKey) ?? 0) + 1);
  let reserved = true;
  const release = () => {
    if (!reserved) return;
    reserved = false;
    const remaining = (input.pendingImports.get(input.targetKey) ?? 0) - 1;
    if (remaining > 0) input.pendingImports.set(input.targetKey, remaining);
    else input.pendingImports.delete(input.targetKey);
  };
  try {
    const file = await input.load();
    if (!input.isActive()) return;
    release();
    await input.attach(file);
  } finally {
    release();
  }
}
