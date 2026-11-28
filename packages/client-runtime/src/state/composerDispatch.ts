import { parseGoalCommand } from "@onicode/contracts";
import {
  isClearComposerCommand,
  isSlashCommand,
  parseSideChatCommand,
} from "@onicode/shared/composerTrigger";
import { isUsageLimitsCommand } from "@onicode/shared/usageLimits";

import { providerSessionViewForPrompt, type ProviderSessionView } from "../providerSessionViews.ts";
import { parseCodexFeedbackCommand } from "./threadFeedback.ts";

export type ComposerDispatchMode = "auto" | "queue" | "steer" | "restart";
export type ActiveTurnComposerAction = Exclude<ComposerDispatchMode, "auto">;

/**
 * A command waits for the turn instead of joining it. Current servers queue one sent as a steer
 * and older ones steer it, so clients send it as a queue; only /goal needs the steer, to stop the
 * turn first, unless the provider says it does not run /goal itself.
 */
export function commandNeverSteers(
  message: {
    readonly text: string;
    readonly attachments: ReadonlyArray<unknown>;
  },
  /** The provider runs /goal itself; undefined from a server that does not say. */
  nativeGoals?: boolean,
): boolean {
  return isSlashCommand(message) && (nativeGoals === false || parseGoalCommand(message) === null);
}

export function resolveComposerDispatchMode(input: {
  readonly running: boolean;
  readonly alternateModifier: boolean;
  readonly activeTurnDefault?: ActiveTurnComposerAction;
  /** False while the active turn cannot take steering input, such as an explicit /compact. */
  readonly canSteer?: boolean;
  /**
   * Saving a queued message: its steer is the queued row's Send now, which checks for itself
   * whether the turn takes it or the message only moves to the front.
   */
  readonly editingQueuedRun?: boolean;
  /** Sent after its thread screen left: the turn it saw may have ended or been replaced. */
  readonly sentAway?: boolean;
}): ComposerDispatchMode {
  if (!input.running) return "auto";
  if (input.sentAway === true) return "queue";
  if (input.canSteer === false && input.editingQueuedRun !== true) return "queue";
  const defaultAction = input.activeTurnDefault ?? "steer";
  if (input.alternateModifier) return defaultAction === "queue" ? "steer" : "queue";
  return defaultAction;
}

// Automatic compaction takes steers only where one leaves it running: the provider takes it
// natively, or the server holds it until the compaction ends.
export function compactionBlocksSteering(input: {
  readonly compactCommandIsActive: boolean;
  readonly compactionRunning: boolean;
  readonly steersDuringCompaction: boolean;
  readonly run: { readonly userMessageId: string } | undefined;
  readonly messages:
    | ReadonlyArray<{
        readonly id: string;
        readonly text: string;
        readonly attachments?: ReadonlyArray<unknown>;
      }>
    | undefined;
}): boolean {
  if (input.compactCommandIsActive) return true;
  if (!input.compactionRunning) return false;
  if (!input.steersDuringCompaction) return true;
  if (input.run === undefined) return false;
  const message = input.messages?.find((message) => message.id === input.run?.userMessageId);
  return (
    (message?.attachments?.length ?? 0) === 0 &&
    /^\/compact(?:\s|$)/i.test(message?.text.trim() ?? "")
  );
}

export function alternateComposerDispatchAction(
  activeTurnDefault?: ActiveTurnComposerAction,
): ActiveTurnComposerAction {
  return resolveComposerDispatchMode({
    running: true,
    alternateModifier: true,
    ...(activeTurnDefault === undefined ? {} : { activeTurnDefault }),
  }) as ActiveTurnComposerAction;
}

/** The composer runs these drafts itself, so they never queue for or steer a running turn. */
export function composerRunsDraftLocally(input: {
  readonly text: string;
  /** The text a send reads `/feedback` and `/clear` from, when it strips more than `text`. */
  readonly commandText?: string;
  readonly holdsOnlyText: boolean;
  readonly sideChat: boolean;
  readonly usageLimits: boolean;
  readonly codexFeedback: boolean;
  readonly freshSession: boolean;
  /** Bare `/context`, `/status`… open a provider session view here. */
  readonly sessionViews: ReadonlyArray<ProviderSessionView>;
}): boolean {
  if (input.sideChat && parseSideChatCommand(input.text) !== null) return true;
  if (!input.holdsOnlyText) return false;
  const commandText = input.commandText ?? input.text;
  return (
    (input.usageLimits && isUsageLimitsCommand(input.text)) ||
    (input.codexFeedback && parseCodexFeedbackCommand(commandText) !== null) ||
    (input.freshSession && isClearComposerCommand(commandText)) ||
    providerSessionViewForPrompt(input.text, input.sessionViews) !== null
  );
}
