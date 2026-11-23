import { msg } from "gt-react";
import { m } from "~/i18n/messages";
import {
  ConnectionBlockedError,
  ConnectionTransientError,
} from "@onicode/client-runtime/connection";
import { EnvironmentRpcUnavailableError } from "@onicode/client-runtime/rpc";
import * as Cause from "effect/Cause";
import * as Schema from "effect/Schema";
import type { RefObject } from "react";
import type { ChatComposerProps } from "./ChatComposer";
import {
  type ChatAttachment,
  type ComposerContextClipboardFragment,
  type ComposerContextRecord,
  type EnvironmentId,
  PROVIDER_SEND_TURN_MAX_ATTACHMENTS,
} from "@onicode/contracts";
import {
  type ComposerFileAttachment,
  type ComposerImageAttachment,
  type DraftId,
  composerFileMatchesReattachMarker,
  composerFileNeedsReattach,
  useComposerDraftStore,
} from "../../composerDraftStore";
import { type PendingUserInput } from "../../session-logic";
import {
  asKnownContextRecord,
  composerContextImportLookupIds,
  isSameComposerContextPayload,
  previewAnnotationContextId,
  previewAnnotationContextRecord,
  previewAnnotationFromRecord,
  reviewCommentContextId,
  reviewCommentContextRecord,
  reviewCommentFromRecord,
  terminalContextDraftFromRecord,
  terminalContextRecord,
  terminalContextReference,
} from "~/lib/composerContextRecords";
import { assetEnvironment } from "~/state/assets";
import { copiedComposerAttachmentFile } from "~/lib/copiedComposerAttachments";
import {
  countQuestionAttachments,
  questionAttachmentDraftId,
  startQuestionDraftWork,
} from "../../questionAttachments";
import { collectInlineContextIds } from "../../lib/composerContextReferences";
import { elementContextToPreviewAnnotation } from "../../lib/elementContext";
import { orchestrationEnvironment } from "~/state/orchestration";
import { pendingDraftWork } from "./pendingDraftWork";
import { readComposerImageBytes } from "./composerAttachmentFiles";
import { serializeComposerImages } from "./composerImagePersistence";
import { randomUUID } from "~/lib/utils";
import { readPreparedConnection } from "~/state/session";
import { resolveAssetUrl } from "~/assets/assetUrls";
import { toKindScopedComposerContextId } from "@onicode/shared/composerContextReferences";
import { toastManager } from "../ui/toast";
import {
  type ComposerDraftContextRecords,
  uploadedContextRecordFromDraft,
} from "../composerContextPresentation";
import { useAtomCommand } from "../../state/use-atom-command";
import { useAtomQueryRunner } from "~/state/use-atom-query-runner";
import { useStableCallback } from "../../hooks/useStableCallback";
import { countHeldComposerSlots, countReservedComposerSlots } from "./composerThreadImport";
import type { QueuedAttachmentContextBinding } from "./composerContextUndo";
import { matchRestorableQueuedAttachment } from "./queuedMessageEdit";

const isUnreachable = Schema.is(
  Schema.Union([ConnectionTransientError, ConnectionBlockedError, EnvironmentRpcUnavailableError]),
);

async function downloadAttachmentBlob(url: string): Promise<Blob | null> {
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(60_000) });
    return response.ok ? await response.blob() : null;
  } catch {
    return null;
  }
}

function reportAttachmentImportFailure(name: string, reason: string, forQuestion: boolean) {
  toastManager.add({
    type: "error",
    title: m(msg("Couldn't bring {name} into this message", { name })),
    // A question's attachment has no chip, before or after it moves to the thread.
    description: forQuestion
      ? reason
      : m(msg("{reason} Remove the chip or attach the file again.", { reason })),
  });
}

/**
 * Imports copied composer context (terminal output, review comments, annotations, threads,
 * images and files) into the open draft, and counts the attachment slots already spoken for.
 */
export function useComposerContextImport(input: {
  readonly environmentId: EnvironmentId;
  readonly linkedEnvironments: ReadonlyMap<EnvironmentId, string>;
  readonly activeThread: ChatComposerProps["activeThread"];
  readonly activeThreadId: ChatComposerProps["activeThreadId"];
  readonly composerDraftTarget: ChatComposerProps["composerDraftTarget"];
  readonly attachmentDraftTarget: ChatComposerProps["composerDraftTarget"];
  readonly attachmentTargetKey: string;
  readonly attachmentTargetKeyRef: RefObject<string>;
  readonly questionAttachmentTarget: DraftId | null;
  readonly pendingUserInputs: ReadonlyArray<PendingUserInput>;
  readonly composerContextRecords: ComposerDraftContextRecords;
  readonly composerImages: ReadonlyArray<ComposerImageAttachment>;
  readonly composerFilesRef: ChatComposerProps["composerFilesRef"];
  readonly promptRef: RefObject<string>;
  readonly addComposerFilesToDraft: (files: ComposerFileAttachment[]) => string[];
  readonly removedQueuedAttachments?: ReadonlyArray<QueuedAttachmentContextBinding> | undefined;
  /** Attachments a queued message being edited keeps; they share its limit. */
  readonly existingAttachmentCount?: number | undefined;
  readonly onRestoreQueuedAttachment?: ((attachment: ChatAttachment) => void) | undefined;
}) {
  const {
    environmentId,
    linkedEnvironments,
    activeThread,
    activeThreadId,
    composerDraftTarget,
    attachmentDraftTarget,
    attachmentTargetKey,
    attachmentTargetKeyRef,
    questionAttachmentTarget,
    pendingUserInputs,
    composerContextRecords,
    composerImages,
    composerFilesRef,
    promptRef,
    addComposerFilesToDraft,
    removedQueuedAttachments,
    existingAttachmentCount = 0,
    onRestoreQueuedAttachment,
  } = input;
  const getComposerDraft = useComposerDraftStore((store) => store.getComposerDraft);
  const addComposerDraftImages = useComposerDraftStore((store) => store.addImages);
  const addComposerDraftTerminalContexts = useComposerDraftStore(
    (store) => store.addTerminalContexts,
  );
  const addComposerDraftReviewComment = useComposerDraftStore((store) => store.addReviewComment);
  const addComposerDraftPreviewAnnotation = useComposerDraftStore(
    (store) => store.addPreviewAnnotation,
  );
  const addComposerDraftThreadContexts = useComposerDraftStore((store) => store.addThreadContexts);
  const addComposerDraftEnvironmentContexts = useComposerDraftStore(
    (store) => store.addEnvironmentContexts,
  );
  const createAssetUrl = useAtomQueryRunner(assetEnvironment.createUrl, { reportFailure: false });
  const loadThreadTranscript = useAtomCommand(orchestrationEnvironment.threadTranscript, {
    reportFailure: false,
  });
  const countReservedAttachments = () => {
    const questionRequest = pendingUserInputs[0];
    const otherQuestionKeys =
      questionAttachmentTarget && questionRequest && activeThreadId
        ? questionRequest.questions
            .map((question) =>
              questionAttachmentDraftId(
                environmentId,
                activeThreadId,
                questionRequest.requestId,
                question.id,
              ),
            )
            .filter((key) => key !== questionAttachmentTarget)
        : [];
    const draft = getComposerDraft(attachmentDraftTarget);
    return (
      existingAttachmentCount +
      (draft?.images.length ?? 0) +
      (draft?.files.length ?? 0) +
      countReservedComposerSlots(attachmentTargetKey) +
      countQuestionAttachments(otherQuestionKeys)
    );
  };
  const countReservedAttachmentsStable = useStableCallback(countReservedAttachments);
  const runAttachmentImport = async (
    record: Extract<ComposerContextRecord, { kind: "image" | "file" }>,
    localId: string,
    sourceEnvironmentId: EnvironmentId,
    importTargetKey: string,
    questionWork: ReturnType<typeof startQuestionDraftWork> | null,
  ) => {
    const fail = (reason: string) => {
      // The question was answered elsewhere, so nothing waits for this attachment.
      if (questionWork?.discarded()) return;
      reportAttachmentImportFailure(record.name, reason, questionWork !== null);
    };
    let file = copiedComposerAttachmentFile(sourceEnvironmentId, record.attachmentId);
    if (!file) {
      const sourceConnection = readPreparedConnection(sourceEnvironmentId);
      if (!sourceConnection) {
        fail(m(msg("The environment it came from is not connected.")));
        return;
      }
      const result = await createAssetUrl({
        environmentId: sourceEnvironmentId,
        input: { resource: { _tag: "attachment", attachmentId: record.attachmentId } },
      });
      const url =
        result._tag === "Success"
          ? resolveAssetUrl(sourceConnection.httpBaseUrl, result.value.relativeUrl)
          : null;
      if (!url) {
        const cause = result._tag === "Failure" ? Cause.squash(result.cause) : null;
        fail(
          isUnreachable(cause)
            ? m(msg("The environment it came from is not reachable right now."))
            : m(msg("The original attachment is no longer available.")),
        );
        return;
      }
      const blob = await downloadAttachmentBlob(url);
      if (!blob) {
        fail(m(msg("Downloading it from the source failed.")));
        return;
      }
      file = new File([blob], record.name, { type: record.mimeType || blob.type });
      if (record.kind === "image") await readComposerImageBytes(file);
      // Question attachments have no inline reference; annotations can own their screenshots.
      const references = new Set(collectInlineContextIds(promptRef.current));
      if (
        !questionAttachmentTarget &&
        !references.has(toKindScopedComposerContextId(record.kind, localId)) &&
        !(
          record.kind === "image" &&
          references.has(previewAnnotationContextId(localId)) &&
          getComposerDraft(composerDraftTarget)?.previewAnnotations.some(
            (annotation) => annotation.id === localId,
          )
        )
      )
        return;
    }
    // The draft these bytes belong to may have been sent or switched away from while they
    // downloaded. Dropping them here keeps them out of whatever draft is open now.
    if (questionWork?.discarded()) return;
    const thread = questionWork?.movedThread();
    if (!thread && attachmentTargetKeyRef.current !== importTargetKey) return;
    const destinationFiles = thread
      ? (getComposerDraft(thread)?.files ?? [])
      : composerFilesRef.current;
    const replacesFileMarker =
      record.kind === "file" &&
      destinationFiles.some(
        (candidate) =>
          composerFileNeedsReattach(candidate) &&
          composerFileMatchesReattachMarker(candidate, {
            name: record.name,
            mimeType: file.type,
            sizeBytes: file.size,
          }),
      );
    if (
      !replacesFileMarker &&
      // A moved import already holds one of the thread draft's reserved slots.
      (thread ? countHeldComposerSlots(thread) - 1 : countReservedAttachments()) >=
        PROVIDER_SEND_TURN_MAX_ATTACHMENTS
    ) {
      fail(m(msg("The attachment limit has been reached.")));
      return;
    }
    if (record.kind === "image") {
      const image: ComposerImageAttachment = {
        type: "image",
        id: localId,
        name: record.name,
        mimeType: file.type,
        sizeBytes: file.size,
        previewUrl: URL.createObjectURL(file),
        file,
        ...(record.source ? { source: record.source } : {}),
      };
      const accepted = addComposerDraftImages(thread ?? attachmentDraftTarget, [image], {
        allowDuplicates:
          getComposerDraft(composerDraftTarget)?.previewAnnotations.some(
            (annotation) => annotation.id === localId,
          ) ?? false,
      });
      if (!accepted.includes(localId)) {
        fail(m(msg("The draft rejected this attachment (duplicate or attachment limit reached).")));
      } else if (thread) {
        // In the draft now, so its claim and reserved slot go before the slow save.
        questionWork?.finish();
        const serialized = await serializeComposerImages([image], []);
        await useComposerDraftStore
          .getState()
          .appendPersistedAttachments(thread, [image], serialized);
      }
    } else {
      const attachment: ComposerFileAttachment = {
        type: "file",
        id: localId,
        name: record.name,
        mimeType: file.type,
        sizeBytes: file.size,
        file,
      };
      const accepted = thread
        ? useComposerDraftStore.getState().addFiles(thread, [attachment])
        : addComposerFilesToDraft([attachment]);
      // A file that replaced a reattach marker is not among the appended ids.
      const landed =
        accepted.includes(localId) ||
        (getComposerDraft(thread ?? attachmentDraftTarget)?.files.some(
          ({ id }) => id === localId,
        ) ??
          false);
      if (!landed) {
        fail(m(msg("The draft rejected this attachment (duplicate or attachment limit reached).")));
      } else if (thread) {
        // In the draft now, so its reserved slot goes before another moved import checks.
        questionWork?.finish();
      }
    }
  };
  const importAttachmentRecord = async (
    record: Extract<ComposerContextRecord, { kind: "image" | "file" }>,
    localId: string,
    sourceEnvironmentId: EnvironmentId,
  ) => {
    // The chip lands in the draft immediately while these bytes are still downloading. Count
    // the transfer against its own draft so a send cannot snapshot a message whose chip has no
    // attachment behind it, and so bytes for an abandoned draft never enter the next one.
    const importTargetKey = attachmentTargetKey;
    const questionWork = questionAttachmentTarget
      ? startQuestionDraftWork(questionAttachmentTarget)
      : null;
    if (!questionWork) pendingDraftWork.begin(importTargetKey);
    await runAttachmentImport(
      record,
      localId,
      sourceEnvironmentId,
      importTargetKey,
      questionWork,
    ).finally(() => (questionWork ? questionWork.finish() : pendingDraftWork.end(importTargetKey)));
  };
  const importContextRecords = (
    records: ReadonlyArray<ComposerContextRecord>,
    sourceEnvironmentId: EnvironmentId | null,
    threadDraftTarget = attachmentDraftTarget,
  ): ReadonlyMap<string, string> => {
    const rewritten = new Map<string, string>();
    // Restores in this paste are not in the queued message's attachments until it re-renders.
    let restoredAttachments = 0;
    const dependentAttachmentLocalIds = new Map<string, string>();
    const skippedDependentAttachmentIds = new Set<string>();
    // Resolve annotations before their dependent screenshot records even if a foreign
    // clipboard producer emitted the records in a different order.
    const orderedRecords = records.toSorted((left, right) =>
      left.kind === "preview-annotation" && right.kind !== "preview-annotation"
        ? -1
        : right.kind === "preview-annotation" && left.kind !== "preview-annotation"
          ? 1
          : 0,
    );
    for (const candidate of orderedRecords) {
      // Producer ids fold into context ids, so two different excerpts can collide. Only skip
      // when the draft already holds the same payload; a colliding but different record is
      // re-minted under a fresh id so both survive the paste. Threads resolve below.
      const record = asKnownContextRecord(candidate);
      if (!record) continue;
      // A reminted annotation needs its screenshot under the new annotation id.
      const existing = dependentAttachmentLocalIds.has(record.contextId)
        ? undefined
        : composerContextImportLookupIds(record).flatMap((contextId) => {
            const found = composerContextRecords.get(contextId);
            return found ? [found] : [];
          })[0];
      // Upload completion can change the server id since copy; the local bytes still match.
      if (
        sourceEnvironmentId !== null &&
        (record.kind === "image" || record.kind === "file") &&
        (existing?.kind === "image" || existing?.kind === "file") &&
        existing.record.file !== null &&
        existing.record.file ===
          copiedComposerAttachmentFile(sourceEnvironmentId, record.attachmentId)
      )
        continue;
      const existingRecord =
        existing?.kind === "terminal"
          ? terminalContextRecord(existing.record)
          : existing?.kind === "review-comment"
            ? reviewCommentContextRecord(existing.record)
            : existing?.kind === "preview-annotation"
              ? previewAnnotationContextRecord(existing.record, {
                  screenshotContextId: composerImages.some(
                    (image) => image.id === existing.record.id,
                  )
                    ? existing.record.id
                    : undefined,
                })
              : existing?.kind === "thread" || existing?.kind === "environment"
                ? existing.record
                : existing
                  ? (uploadedContextRecordFromDraft(existing) ?? undefined)
                  : undefined;
      if (
        existingRecord &&
        isSameComposerContextPayload(existingRecord, record) &&
        record.kind !== "thread"
      ) {
        if (record.kind === "preview-annotation" && record.screenshotContextId) {
          skippedDependentAttachmentIds.add(record.screenshotContextId);
        }
        continue;
      }
      const conflicts = existing !== undefined;
      switch (record.kind) {
        case "terminal": {
          const threadId = activeThread?.id ?? activeThreadId;
          if (!threadId) break;
          const imported = terminalContextDraftFromRecord(record, threadId);
          const draft = conflicts ? { ...imported, id: randomUUID() } : imported;
          addComposerDraftTerminalContexts(composerDraftTarget, [draft], {
            appendReference: false,
          });
          rewritten.set(record.contextId, terminalContextReference(draft).contextId);
          break;
        }
        case "review-comment": {
          const imported = reviewCommentFromRecord(record);
          const comment = conflicts ? { ...imported, id: randomUUID() } : imported;
          addComposerDraftReviewComment(composerDraftTarget, comment, {
            appendReference: false,
          });
          rewritten.set(record.contextId, reviewCommentContextId(comment.id));
          break;
        }
        case "element":
        case "preview-annotation": {
          const imported =
            record.kind === "element"
              ? elementContextToPreviewAnnotation(record, randomUUID(), new Date().toISOString())
              : previewAnnotationFromRecord(record);
          const annotation = conflicts ? { ...imported, id: randomUUID() } : imported;
          if (record.kind === "preview-annotation" && record.screenshotContextId) {
            dependentAttachmentLocalIds.set(record.screenshotContextId, annotation.id);
          }
          addComposerDraftPreviewAnnotation(composerDraftTarget, annotation, {
            appendReference: false,
          });
          rewritten.set(record.contextId, previewAnnotationContextId(annotation.id));
          break;
        }
        case "thread": {
          // The agent reads threads on its own server and the machines it links to; a pasted
          // thread from any other machine is dropped.
          if (
            record.environmentId !== environmentId &&
            !linkedEnvironments.has(record.environmentId)
          )
            break;
          // Threads reuse their destination record by identity, reminting only on an ID collision.
          const threads = getComposerDraft(threadDraftTarget)?.threadContexts ?? [];
          const attached = threads.find(
            (thread) =>
              thread.environmentId === record.environmentId && thread.threadId === record.threadId,
          );
          const imported =
            attached ??
            (threads.some((thread) => thread.contextId === record.contextId)
              ? { ...record, contextId: toKindScopedComposerContextId("thread", randomUUID()) }
              : record);
          addComposerDraftThreadContexts(threadDraftTarget, [imported], {
            appendReference: false,
          });
          rewritten.set(record.contextId, imported.contextId);
          break;
        }
        case "environment": {
          // A machine means the same machine from any thread, so a pasted one is kept,
          // but not in an answer to an agent's question, which carries no machines.
          if (questionAttachmentTarget !== null) break;
          addComposerDraftEnvironmentContexts(composerDraftTarget, [record], {
            appendReference: false,
          });
          rewritten.set(record.contextId, record.contextId);
          break;
        }
        case "image":
        case "file": {
          if (skippedDependentAttachmentIds.has(record.contextId)) break;
          if (questionAttachmentTarget === null && !conflicts) {
            const restored = matchRestorableQueuedAttachment({
              environmentId,
              sourceEnvironmentId,
              record,
              removedQueuedAttachments: removedQueuedAttachments ?? [],
            });
            if (restored) {
              if (
                countReservedAttachments() + restoredAttachments >=
                PROVIDER_SEND_TURN_MAX_ATTACHMENTS
              ) {
                reportAttachmentImportFailure(
                  record.name,
                  m(msg("The attachment limit has been reached.")),
                  false,
                );
                break;
              }
              restoredAttachments += 1;
              onRestoreQueuedAttachment?.(restored.attachment);
              rewritten.set(record.contextId, restored.contextId);
              break;
            }
          }
          if (sourceEnvironmentId === null && !conflicts) {
            rewritten.set(record.contextId, record.contextId);
            break;
          }
          if (sourceEnvironmentId === null) break;
          const localId = dependentAttachmentLocalIds.get(record.contextId) ?? randomUUID();
          rewritten.set(record.contextId, toKindScopedComposerContextId(record.kind, localId));
          void importAttachmentRecord(record, localId, sourceEnvironmentId);
          break;
        }
        default:
          break;
      }
    }
    return rewritten;
  };
  const importContextFragment = (
    fragment: ComposerContextClipboardFragment,
  ): ReadonlyMap<string, string> =>
    importContextRecords(fragment.records, fragment.source.environmentId);
  const importContextFragmentStable = useStableCallback(importContextFragment);

  return {
    loadThreadTranscript,
    countReservedAttachments,
    countReservedAttachmentsStable,
    importContextRecords,
    importContextFragment,
    importContextFragmentStable,
  };
}
