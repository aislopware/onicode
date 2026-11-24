import { remapComposerContextAttachments } from "@onicode/shared/composerContextReferences";
import { appendUserInputAttachmentPaths } from "../provider/userInputAttachments.ts";
import {
  AuthProvidersManageScope,
  type ChatAttachment,
  CommandId,
  type OrchestrationV2Command,
} from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Schema from "effect/Schema";
import { resolveAttachmentPath } from "../attachmentStore.ts";
import { rpcAuthorizationError } from "../auth/RpcAuthorization.ts";
import * as ServerConfig from "../config.ts";
import * as DocumentText from "../provider/DocumentText.ts";
import * as Orchestrator from "./Orchestrator.ts";

import * as AttachmentClaims from "./AttachmentClaims.ts";
import { isProviderSignOutPrompt } from "./CommandPolicy.ts";
import * as ThreadLaunch from "./ThreadLaunchService.ts";
import * as ThreadManagement from "./ThreadManagementService.ts";

// These failures precede commitCommand; generic dispatch errors can follow commits and remain ambiguous.
function dispatchWasNotAccepted(
  error: Orchestrator.OrchestratorV2Error | ThreadManagement.ThreadManagementError,
) {
  switch (error._tag) {
    case "OrchestratorCommandRejectedError":
    case "OrchestratorThreadAlreadyExistsError":
    case "OrchestratorProjectionError":
    case "OrchestratorProviderAdapterError":
    case "OrchestratorCommandPreviouslyRejectedError":
    case "OrchestratorCommandIdConflictError":
    case "OrchestratorSubagentThreadReadOnlyError":
    case "OrchestratorThreadAboveModeLimitError":
    case "OrchestratorProviderSelectionBusyError":
    case "OrchestratorRuntimeModeAboveCeilingError":
    case "ThreadManagementProviderInstanceDisabledError":
    case "ThreadManagementWorktreeRestoreError":
      return true;
    default:
      return false;
  }
}
const isOrchestratorError = Schema.is(Orchestrator.OrchestratorV2Error);
const isThreadManagementError = Schema.is(ThreadManagement.ThreadManagementError);

const releaseUnusedClaims = Effect.fn("ThreadMessageIntake.releaseUnusedClaims")(function* (
  claimedPaths: ReadonlyArray<string>,
  accepted: ReadonlyArray<ChatAttachment>,
) {
  if (claimedPaths.length === 0) return;
  const config = yield* ServerConfig.ServerConfig;
  const retained = new Set(
    accepted.flatMap((attachment) => {
      const path = resolveAttachmentPath({
        attachmentsDir: config.attachmentsDir,
        attachment,
      });
      return path ? [path, DocumentText.extractedTextPath(path)] : [];
    }),
  );
  yield* AttachmentClaims.releaseClaimedAttachments(
    claimedPaths.filter((path) => !retained.has(path)),
  );
});

// Another attempt can commit after this one read no receipt, and the pending sweep then
// removes the uploads this one goes on to claim; the receipt decides, not the missing file.
const claimUnlessAccepted = <A, E, R, E2, R2>(
  claim: Effect.Effect<A, E, R>,
  accepted: Effect.Effect<boolean, E2, R2>,
) =>
  claim.pipe(
    Effect.catch((error) =>
      Effect.flatMap(accepted, (isAccepted) =>
        isAccepted ? Effect.undefined : Effect.fail(error),
      ),
    ),
  );

/**
 * For a caller without `providers:manage`. An edit that omits `attachments` keeps the queued
 * ones, so `/logout` signs out only when the queued message has none. The edit goes on with
 * the files read here, so an edit racing this one cannot strip them afterwards.
 */
export const withQueuedAttachmentsOnSignOutText = Effect.fn(
  "ThreadMessageIntake.withQueuedAttachmentsOnSignOutText",
)(function* (command: Extract<OrchestrationV2Command, { readonly type: "queued-run.edit" }>) {
  if (
    command.attachments !== undefined ||
    !isProviderSignOutPrompt({ text: command.text, attachments: [] })
  ) {
    return command;
  }
  const threads = yield* ThreadManagement.ThreadManagementService;
  // A retry of an accepted edit replays its receipt, even after its run left the queue.
  if (yield* threads.hasReceipt(command.commandId)) return command;
  const { runs } = yield* threads.getThreadRecords(command.threadId, ["runs"], {
    runIds: [command.runId],
  });
  const queuedRun = runs.find((run) => run.id === command.runId && run.status === "queued");
  const attachments =
    queuedRun === undefined
      ? []
      : ((yield* threads.getMessagesById(command.threadId, [queuedRun.userMessageId])).get(
          queuedRun.userMessageId,
        )?.attachments ?? []);
  if (attachments.length === 0) return yield* rpcAuthorizationError(AuthProvidersManageScope);
  return { ...command, attachments };
});

/**
 * `sender.managesProviders` is whether the caller holds `providers:manage`. RPC authorization
 * has already checked every command whose payload shows a provider sign-out; a queued edit
 * that keeps its attachments is the one case decided here.
 */
export const dispatchCommand = Effect.fn("ThreadMessageIntake.dispatchCommand")(function* (
  incoming: OrchestrationV2Command,
  sender: { readonly managesProviders: boolean },
) {
  const command =
    incoming.type === "queued-run.edit" && !sender.managesProviders
      ? yield* withQueuedAttachmentsOnSignOutText(incoming)
      : incoming;
  const threads = yield* ThreadManagement.ThreadManagementService;
  if (
    command.type === "runtime-request.respond" &&
    command.attachmentsByQuestionId &&
    !(yield* threads.hasReceipt(command.commandId))
  ) {
    const config = yield* ServerConfig.ServerConfig;
    const incomingByQuestionId = command.attachmentsByQuestionId;
    yield* AttachmentClaims.validateAttachmentLimits(Object.values(incomingByQuestionId).flat());
    const claimedPaths: string[] = [];
    const prepared = yield* Effect.gen(function* () {
      const attachmentsByQuestionId: import("@onicode/contracts").UserInputAttachments = {};
      for (const [questionId, attachments] of Object.entries(incomingByQuestionId)) {
        const claimed = yield* AttachmentClaims.claimPendingAttachments({
          threadId: command.threadId,
          attachments,
        });
        claimedPaths.push(...claimed.claimedPaths);
        Object.defineProperty(attachmentsByQuestionId, questionId, {
          value: claimed.attachments,
          enumerable: true,
        });
      }
      // An answer by message carries its files as message attachments, not paths in its text.
      const { runtimeRequests } = yield* threads
        .getThreadRecords(command.threadId, ["runtimeRequests"], {
          runtimeRequestStatuses: ["pending"],
        })
        .pipe(
          Effect.mapError(
            (cause) =>
              new AttachmentClaims.AttachmentClaimError({
                message: "Failed to read the question.",
                cause,
              }),
          ),
        );
      const answersByMessage =
        runtimeRequests.find((request) => request.id === command.requestId)?.responseCapability
          .type === "message";
      const answers = answersByMessage
        ? (command.answers ?? {})
        : yield* appendUserInputAttachmentPaths({
            answers: command.answers ?? {},
            attachmentsByQuestionId,
            attachmentsDir: config.attachmentsDir,
          }).pipe(
            Effect.mapError(
              (cause) => new AttachmentClaims.AttachmentClaimError({ message: cause.issue }),
            ),
          );
      return { answers, attachmentsByQuestionId };
    }).pipe(
      Effect.onError(() => AttachmentClaims.releaseClaimedAttachments(claimedPaths)),
      (prepare) => claimUnlessAccepted(prepare, threads.hasReceipt(command.commandId)),
    );
    if (prepared === undefined) return yield* threads.dispatch(command);
    return yield* threads
      .dispatch({
        ...command,
        answers: prepared.answers,
        attachmentsByQuestionId: prepared.attachmentsByQuestionId,
      })
      .pipe(
        Effect.tap((result) => {
          // Replayed receipts refer to the first answer; release this attempt's unused copies only with a resolved item.
          // Missing questionAnswer means no accepted attachments; missing resolution leaves delivery ambiguous.
          const answeredItems = result.storedEvents.flatMap(({ event }) =>
            event.type === "turn-item.updated" &&
            event.payload.type === "user_input_request" &&
            event.payload.requestId === command.requestId
              ? [event.payload]
              : [],
          );
          return answeredItems.length > 0
            ? releaseUnusedClaims(
                claimedPaths,
                answeredItems.flatMap((item) =>
                  item.questionAnswer === undefined
                    ? []
                    : Object.values(item.questionAnswer.attachmentsByQuestionId).flat(),
                ),
              )
            : Effect.void;
        }),
        Effect.tapError((error) =>
          dispatchWasNotAccepted(error)
            ? AttachmentClaims.releaseClaimedAttachments(claimedPaths)
            : Effect.void,
        ),
      );
  }
  if (
    command.type !== "message.dispatch" &&
    (command.type !== "queued-run.edit" || command.attachments === undefined)
  )
    return yield* threads.dispatch(command);
  // An accepted command's retry replays its receipt even if a later cleanup removed its files.
  if (
    (command.type === "queued-run.edit" ||
      command.attachments.some(AttachmentClaims.attachmentIsPendingUpload)) &&
    (yield* threads.hasReceipt(command.commandId))
  )
    return yield* threads.dispatch(command);
  if (command.type === "queued-run.edit") {
    const config = yield* ServerConfig.ServerConfig;
    const fileSystem = yield* FileSystem.FileSystem;
    // A stale view can keep a file that another client's edit dropped and deleted.
    for (const attachment of command.attachments ?? []) {
      if (AttachmentClaims.attachmentIsPendingUpload(attachment)) continue;
      const path = resolveAttachmentPath({ attachmentsDir: config.attachmentsDir, attachment });
      if (
        path !== null &&
        !(yield* fileSystem.exists(path).pipe(Effect.orElseSucceed(() => false)))
      ) {
        return yield* new AttachmentClaims.AttachmentClaimError({
          message: `Attachment '${attachment.name}' cannot be sent: attachment not found.`,
        });
      }
    }
  }
  const claimed = yield* claimUnlessAccepted(
    AttachmentClaims.claimPendingAttachments({
      threadId: command.threadId,
      attachments: command.attachments ?? [],
    }),
    threads.hasReceipt(command.commandId),
  );
  if (claimed === undefined) return yield* threads.dispatch(command);
  return yield* threads
    .dispatch({
      ...command,
      attachments: claimed.attachments,
      ...(command.context
        ? {
            context: remapComposerContextAttachments(
              command.context,
              command.attachments ?? [],
              claimed.attachments,
            ),
          }
        : {}),
    })
    .pipe(
      Effect.tap((result) =>
        releaseUnusedClaims(
          claimed.claimedPaths,
          result.storedEvents.flatMap(({ event }) =>
            event.type === "message.updated" ? event.payload.attachments : [],
          ),
        ),
      ),
      Effect.tapError((error) =>
        dispatchWasNotAccepted(error)
          ? AttachmentClaims.releaseClaimedAttachments(claimed.claimedPaths)
          : Effect.void,
      ),
    );
});

export const sendToThread = Effect.fn("ThreadMessageIntake.sendToThread")(function* (
  input: ThreadManagement.ThreadManagementSendInput,
) {
  const threads = yield* ThreadManagement.ThreadManagementService;
  if (
    input.attachments.some(AttachmentClaims.attachmentIsPendingUpload) &&
    (yield* threads.hasReceipt(input.commandId))
  )
    return yield* threads.sendToThread(input);
  const claimed = yield* claimUnlessAccepted(
    AttachmentClaims.claimPendingAttachments(input),
    threads.hasReceipt(input.commandId),
  );
  if (claimed === undefined) return yield* threads.sendToThread(input);
  return yield* threads.sendToThread({ ...input, attachments: claimed.attachments }).pipe(
    Effect.tap((result) => releaseUnusedClaims(claimed.claimedPaths, result.message.attachments)),
    Effect.tapError((error) =>
      dispatchWasNotAccepted(error)
        ? AttachmentClaims.releaseClaimedAttachments(claimed.claimedPaths)
        : Effect.void,
    ),
  );
});

export const launchThread = Effect.fn("ThreadMessageIntake.launchThread")(function* (
  input: ThreadLaunch.ThreadLaunchInput,
) {
  const launches = yield* ThreadLaunch.ThreadLaunchService;
  yield* AttachmentClaims.validateAttachmentLimits(input.initialMessage?.attachments ?? []);
  if (!input.initialMessage?.attachments.some(AttachmentClaims.attachmentIsPendingUpload)) {
    return yield* launches.launch(input);
  }
  if (input.threadId === undefined) {
    return yield* new AttachmentClaims.AttachmentClaimError({
      message: "Uploaded attachments need a thread id at launch.",
    });
  }
  const threads = yield* ThreadManagement.ThreadManagementService;
  const threadId = input.threadId;
  const messageId = input.initialMessage.messageId;
  // A new thread's create can carry its first message; a reused thread's takes its own command.
  // A create that carried none, as from an earlier launch with this id, still needs the uploads.
  const messageAccepted = Effect.gen(function* () {
    if (yield* threads.hasReceipt(CommandId.make(`${input.commandId}:initial-message`))) {
      return true;
    }
    if (!(yield* threads.hasReceipt(input.commandId))) return false;
    if (messageId === undefined) return input.reuseExistingThread !== true;
    const messages = yield* threads.getMessagesById(threadId, [messageId]).pipe(
      Effect.mapError(
        (cause) =>
          new ThreadLaunch.ThreadLaunchError({
            operation: "read-accepted-message",
            commandId: input.commandId,
            projectId: input.projectId,
            threadId,
            cause,
          }),
      ),
    );
    return messages.has(messageId);
  });
  if (yield* messageAccepted) return yield* launches.launch(input);
  const claimed = yield* claimUnlessAccepted(
    AttachmentClaims.claimPendingAttachments({
      threadId,
      attachments: input.initialMessage.attachments,
    }),
    messageAccepted,
  );
  if (claimed === undefined) return yield* launches.launch(input);
  return yield* launches
    .launch({
      ...input,
      initialMessage: {
        ...input.initialMessage,
        attachments: claimed.attachments,
        ...(input.initialMessage.context
          ? {
              context: remapComposerContextAttachments(
                input.initialMessage.context,
                input.initialMessage.attachments,
                claimed.attachments,
              ),
            }
          : {}),
      },
    })
    .pipe(
      Effect.tap((result) =>
        releaseUnusedClaims(
          claimed.claimedPaths,
          result.projection.messages.flatMap((message) => message.attachments),
        ),
      ),
      Effect.tapError((error) => {
        if (error._tag === "ThreadManagementProviderInstanceDisabledError") {
          return AttachmentClaims.releaseClaimedAttachments(claimed.claimedPaths);
        }
        // Project/receipt reads precede dispatch, and the accepted message a replay reads holds an
        // earlier attempt's copies. Create-thread errors can wrap post-message reads and prove
        // nothing alone.
        const notAccepted =
          error.operation === "resolve-project" ||
          error.operation === "read-receipt" ||
          error.operation === "read-accepted-message" ||
          ((error.operation === "create-thread" || error.operation === "dispatch-message") &&
            (isOrchestratorError(error.cause) || isThreadManagementError(error.cause)) &&
            // Projection errors under create-thread can occur after the message commit.
            (error.operation !== "create-thread" ||
              error.cause._tag !== "OrchestratorProjectionError") &&
            dispatchWasNotAccepted(error.cause));
        return notAccepted
          ? AttachmentClaims.releaseClaimedAttachments(claimed.claimedPaths)
          : Effect.void;
      }),
    );
});
