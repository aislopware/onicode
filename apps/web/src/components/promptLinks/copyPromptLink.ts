import { thrownErrorMessage } from "@onicode/client-runtime/errors";
import type { EnvironmentProject } from "@onicode/client-runtime/state/shell";
import { isAttachmentOnlyBootstrapPrompt } from "@onicode/shared/composerContextReferences";
import {
  type ModelSelection,
  repositoryGroupingKeyOf,
  type ServerProvider,
} from "@onicode/contracts";
import {
  buildNewThreadLink,
  parsePromptLinkRepository,
  PROMPT_LINK_MAX_PROMPT_LENGTH,
  promptLinkDeepLink,
  type PromptLinkRepository,
} from "@onicode/shared/promptLinks";

import { type ComposerThreadTarget, useComposerDraftStore } from "~/composerDraftStore";
import { writeTextToClipboard } from "~/hooks/useCopyToClipboard";
import { draftModelSelectionApplies } from "~/lib/chatThreadActions";

import { stackedThreadToast, toastManager } from "../ui/toast";
import { msg } from "gt-react";
import { m } from "~/i18n/messages";

/** A link that opens the prompt as a draft in the desktop app, or null when it cannot fit in one. */
export function promptLinkFor(
  prompt: string,
  context?: {
    readonly repository: PromptLinkRepository | null;
    readonly provider: string | null;
    readonly model: string | null;
  },
): string | null {
  const trimmed = isAttachmentOnlyBootstrapPrompt(prompt) ? "" : prompt.trim();
  if (trimmed.length === 0 || trimmed.length > PROMPT_LINK_MAX_PROMPT_LENGTH) return null;
  const link = new URL(buildNewThreadLink({ prompt: trimmed, ...context }));
  return promptLinkDeepLink(`${link.pathname}${link.search}`);
}

export function promptLinkRepositoryOf(
  project: Pick<EnvironmentProject, "repositoryIdentity"> | undefined,
): PromptLinkRepository | null {
  const identity = project?.repositoryIdentity;
  if (identity == null) return null;
  const key = repositoryGroupingKeyOf(identity);
  return key.startsWith("github.com/")
    ? parsePromptLinkRepository(key.slice("github.com/".length))
    : null;
}

export function composerPromptLink(input: {
  readonly target: ComposerThreadTarget;
  readonly project: EnvironmentProject | undefined;
  readonly providers: ReadonlyArray<ServerProvider>;
  readonly threadModelSelection: ModelSelection | null;
  readonly threadStarted: boolean;
  readonly withContext: boolean;
}): string | null {
  const draft = useComposerDraftStore.getState().getComposerDraft(input.target);
  if (!draft) return null;
  if (!input.withContext) return promptLinkFor(draft.prompt);
  const selection =
    (draft.activeProvider && draftModelSelectionApplies(draft, input.threadStarted)
      ? draft.modelSelectionByProvider[draft.activeProvider]
      : undefined) ?? input.threadModelSelection;
  const provider = selection
    ? (input.providers.find((candidate) => candidate.instanceId === selection.instanceId)?.driver ??
      null)
    : null;
  return promptLinkFor(draft.prompt, {
    repository: promptLinkRepositoryOf(input.project),
    provider,
    model: provider === null ? null : (selection?.model ?? null),
  });
}

export async function copyPromptLink(link: string): Promise<void> {
  try {
    await writeTextToClipboard(link);
  } catch (error) {
    toastManager.add(
      stackedThreadToast({
        type: "error",
        title: m(msg("Could not copy the prompt link")),
        description: thrownErrorMessage(error, m) ?? m(msg("An error occurred.")),
      }),
    );
  }
}
