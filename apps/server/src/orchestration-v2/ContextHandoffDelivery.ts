import type {
  OrchestrationV2ContextHandoff,
  OrchestrationV2ProviderThread,
  OrchestrationV2ThreadProjection,
} from "@onicode/contracts";
import * as ProviderAdapter from "@onicode/provider-core/server/ProviderAdapter";
import { ContextHandoffBudgetError } from "@onicode/provider-core/server/failure";
import { unsentManualContinuationSource } from "@onicode/shared/orchestrationV2ManualContinuation";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import {
  historyCost,
  renderHistory,
  selectHistory,
} from "@onicode/provider-core/server/handoffBudget";

type CommandMessage = {
  readonly role: string;
  readonly text: string;
  readonly attachments?: ReadonlyArray<unknown> | undefined;
};

type CommandSession = Pick<ProviderAdapter.ProviderAdapterV2SessionRuntime, "runsAsNativeCommand">;

/** Every message a provider could run as a command opens with `/`, as the projection index reads them. */
export const mayBeNativeCommandMessage = (message: CommandMessage): boolean =>
  message.role === "user" && message.text.trim().startsWith("/");

/** Providers only run a native command that opens the prompt, so its turn defers inline context to the next one. */
export const isNativeCommandMessage = (session: CommandSession, message: CommandMessage): boolean =>
  mayBeNativeCommandMessage(message) &&
  (session.runsAsNativeCommand?.({ text: message.text, attachments: message.attachments ?? [] }) ??
    ProviderAdapter.opensWithSlashCommand(message.text));

/**
 * A turn whose restart notes and missed images wait for the next prompt. Handoffs record their
 * delivery, but these are inferred from history, which older servers classified by text alone.
 */
export const leavesNotesForNextPrompt = (
  session: CommandSession,
  message: CommandMessage,
): boolean =>
  (message.role === "user" && ProviderAdapter.opensWithSlashCommand(message.text)) ||
  isNativeCommandMessage(session, message);

/**
 * Messages of turns that left notes and missed images for the next prompt. A resume replays its
 * unsent source's prompt under an empty message of its own, so that prompt classifies it.
 */
export const notesWaitingMessageIds = (
  session: CommandSession,
  projection: Pick<
    OrchestrationV2ThreadProjection,
    "runs" | "attempts" | "providerTurns" | "messages"
  >,
): ReadonlySet<string> => {
  const ids = new Set<string>(
    projection.messages
      .filter((message) => leavesNotesForNextPrompt(session, message))
      .map((message) => message.id),
  );
  for (const run of projection.runs) {
    // A steering restart replaces the run's message with the text it sent.
    if (
      run.manualContinuationOfRunId === undefined ||
      projection.attempts.some(
        (attempt) => attempt.runId === run.id && attempt.reason === "steering_restart",
      )
    ) {
      continue;
    }
    const replayed = unsentManualContinuationSource(run, projection);
    if (replayed !== undefined && ids.has(replayed.userMessageId)) ids.add(run.userMessageId);
  }
  return ids;
};

/** The provider prompt for a turn that carries inline context ahead of the user's text. */
export const withInlineContext = (context: string, userText: string): string =>
  context === "" ? userText : `${context}\n\nUser message:\n${userText}`;

/** Persist around injection; ambiguous delivery requires a fresh native thread. Call unsent if refused before prompt consumption. */
export const deliverContextHandoffs = Effect.fn("orchestrationV2.deliverContextHandoffs")(
  function* <InjectError = never, PersistError = never, BudgetError = never>(input: {
    readonly handoffs: ReadonlyArray<OrchestrationV2ContextHandoff>;
    readonly providerThread: OrchestrationV2ProviderThread;
    readonly budget: number | Effect.Effect<number, BudgetError>;
    readonly deferInline?: boolean;
    readonly alreadyDeliveredItemIds: ReadonlySet<string>;
    readonly inject?: (
      history: ProviderAdapter.ProviderAdapterV2HistoricalContext,
    ) => Effect.Effect<boolean, InjectError>;
    readonly persist: (handoff: OrchestrationV2ContextHandoff) => Effect.Effect<void, PersistError>;
  }) {
    const nativeThreadId = input.providerThread.nativeThreadRef?.nativeId ?? undefined;
    const pending = input.handoffs.filter(
      (handoff) =>
        nativeThreadId === undefined ||
        handoff.delivery?.nativeThreadId !== nativeThreadId ||
        handoff.delivery.status === "pending",
    );
    if (pending.length === 0 || (input.deferInline && input.inject === undefined))
      return { context: "", delivered: Effect.void, unsent: Effect.void, inlineHandoffs: [] };
    const budget = typeof input.budget === "number" ? input.budget : yield* input.budget;
    let coverage = pending
      .map(
        (handoff) =>
          `Context handoff (${handoff.strategy === "fork_delta_summary" ? "merge_back / fork_delta_summary" : handoff.strategy}):\n${
            handoff.history?.coverage ??
            `From thread ${handoff.threadId}, runs ${handoff.coveredRunOrdinals.from}-${handoff.coveredRunOrdinals.to}. Recover history with oni_thread_read, view=activity, afterPosition=-1; paginate with afterPosition=nextPosition, and use itemId/textOffset for long items.`
          }`,
      )
      .join("\n");
    if (historyCost([], coverage) > Math.min(4_000, budget / 2)) {
      const strategies = Array.from(new Set(pending.map((handoff) => handoff.strategy)));
      coverage = `Context handoff (${strategies.join(", ")}). ${pending.length} handoff records; detailed coverage references omitted. Recover an omitted item only when the request needs it, with oni_thread_read({threadId:"${input.providerThread.appThreadId ?? pending[0]!.threadId}",view:"activity",itemId:"<omitted item ID>",maxCharsPerItem:4000}); continue long text with textOffset=nextTextOffset until null. Follow fork/handoff source references in activity. Items included below need no read.`;
    }
    const seen = new Set(input.alreadyDeliveredItemIds);
    const messages = pending
      .flatMap((handoff) => handoff.history?.messages ?? [])
      .filter((message) => {
        if (seen.has(message.itemId)) return false;
        seen.add(message.itemId);
        return true;
      });
    const oldContext = pending
      .filter((handoff) => handoff.history === undefined)
      .map((handoff) => handoff.summaryText)
      .join("\n\n");
    const fullCoverage =
      oldContext && historyCost([], `${coverage}\n${oldContext}`) + 512 <= budget
        ? `${coverage}\n${oldContext}`
        : coverage;
    const selected = selectHistory({
      messages,
      coverage: fullCoverage,
      omittedItems: pending.reduce((sum, handoff) => sum + (handoff.history?.omittedItems ?? 0), 0),
      omittedItemIds: pending.flatMap((handoff) => handoff.history?.omittedItemIds ?? []),
      budget,
    });
    if (historyCost(selected.messages, selected.context) > budget) {
      if (input.deferInline)
        return { context: "", delivered: Effect.void, unsent: Effect.void, inlineHandoffs: [] };
      return yield* new ContextHandoffBudgetError();
    }
    const omittedItemIds = new Set(selected.omittedItemIds);
    const persist = (status: "pending" | "injected" | "inline") =>
      Effect.forEach(
        pending,
        (handoff) =>
          input.persist({
            ...handoff,
            ...(nativeThreadId === undefined
              ? {}
              : {
                  delivery: {
                    nativeThreadId,
                    status,
                    omittedItemIds: [
                      ...(handoff.history?.omittedItemIds ?? []),
                      ...(handoff.history?.messages ?? [])
                        .filter((message) => omittedItemIds.has(message.itemId))
                        .map((message) => message.itemId),
                    ],
                    itemIds: selected.messages
                      .filter((message) =>
                        handoff.history?.messages.some(
                          (candidate) => candidate.itemId === message.itemId,
                        ),
                      )
                      .map((message) => message.itemId),
                  },
                }),
          }),
        { discard: true },
      );
    if (input.inject !== undefined && nativeThreadId !== undefined) {
      if (
        pending.some(
          (handoff) =>
            handoff.delivery?.nativeThreadId === nativeThreadId &&
            handoff.delivery.status === "pending",
        )
      ) {
        return yield* new ContextHandoffDeliveryUncertainError();
      }
      yield* persist("pending");
      const injected = yield* input.inject({
        messages: selected.messages,
        context: selected.context,
      });
      if (injected) {
        yield* persist("injected");
        return { context: "", delivered: Effect.void, unsent: Effect.void, inlineHandoffs: [] };
      }
    } else {
      yield* persist("pending");
    }
    if (input.deferInline) {
      // Compaction cannot carry inline transcripts; retain for the next turn if native injection is unsupported.
      yield* Effect.forEach(pending, input.persist, { discard: true });
      return { context: "", delivered: Effect.void, unsent: Effect.void, inlineHandoffs: [] };
    }
    return {
      context: renderHistory(selected.messages, selected.context),
      delivered: persist("inline"),
      unsent: Effect.forEach(pending, input.persist, { discard: true }),
      inlineHandoffs: pending,
    };
  },
);

export class ContextHandoffDeliveryUncertainError extends Schema.TaggedError<ContextHandoffDeliveryUncertainError>()(
  "ContextHandoffDeliveryUncertainError",
  {},
) {
  override get message() {
    return "Historical context delivery is uncertain; replace the native thread before retrying.";
  }
}
