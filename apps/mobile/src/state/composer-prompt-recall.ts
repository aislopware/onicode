import { useAtomValue } from "@effect/atom-react";
import {
  buildComposerPromptHistoryEntries,
  stepComposerPromptHistory,
  type ComposerPromptHistoryPosition,
} from "@onicode/client-runtime/composer-prompt-history";
import { Atom } from "effect/reactivity";

import { appAtomRegistry } from "./atom-registry";
import {
  getComposerDraftSnapshot,
  rememberComposerDraftSelection,
  setComposerDraftContext,
  setComposerDraftText,
} from "./use-composer-drafts";

// Read at each key press, not from a render: a held arrow repeats faster than React commits.
// It only counts while the draft still shows the recalled prompt, so edits and sends need not clear it.
const composerPromptRecallAtom = Atom.family((_draftKey: string) =>
  Atom.make<ComposerPromptHistoryPosition | null>(null).pipe(
    Atom.keepAlive,
    Atom.withLabel("mobile:composer-prompt-recall"),
  ),
);

/** True while the draft shows a recalled prompt unedited, so Up and Down keep stepping from it. */
export function useComposerPromptRecallActive(draftKey: string, text: string): boolean {
  return useAtomValue(composerPromptRecallAtom(draftKey))?.recalled === text;
}

/** Replaces the draft with the thread's earlier or later prompt and its thread and machine mentions. */
export function recallComposerPrompt(input: {
  readonly draftKey: string;
  readonly direction: "backward" | "forward";
  /** The thread's messages, oldest first. */
  readonly messages: Parameters<typeof buildComposerPromptHistoryEntries>[0];
}): void {
  const draft = getComposerDraftSnapshot(input.draftKey);
  // A recalled prompt would replace the text under attachments the user is still composing with.
  if (draft.attachments.length > 0) return;
  const positionAtom = composerPromptRecallAtom(input.draftKey);
  const step = stepComposerPromptHistory({
    direction: input.direction,
    entries: buildComposerPromptHistoryEntries(input.messages),
    position: appAtomRegistry.get(positionAtom),
    currentPrompt: draft.text,
  });
  if (step === null) return;
  appAtomRegistry.set(positionAtom, step.position);
  setComposerDraftText(input.draftKey, step.prompt);
  const records = [...step.mentions.threadContexts, ...step.mentions.environmentContexts];
  if (records.length > 0) setComposerDraftContext(input.draftKey, { version: 1, records });
  const end = step.prompt.length;
  rememberComposerDraftSelection(input.draftKey, step.prompt, { start: end, end });
}
