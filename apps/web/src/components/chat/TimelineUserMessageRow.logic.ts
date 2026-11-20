import { type ChatFileAttachment, type ChatImageAttachment } from "../../types";
import type { ComposerContextRecord, KnownComposerContextRecord } from "@onicode/contracts";
import type { ThreadStructureTurnItem } from "@onicode/client-runtime/state/shell";
import { msg } from "gt-react";
import { m, type MessageResolver } from "~/i18n/messages";
import { asKnownContextRecord } from "~/lib/composerContextRecords";

export function userMessageFileLabel(
  file: Pick<ChatFileAttachment, "name" | "source">,
  resolve: MessageResolver = m,
) {
  return file.source?._tag === "pasted-text" ? resolve(msg("Pasted text")) : file.name;
}

export function resolvePreviewAnnotationImage(input: {
  record: Extract<KnownComposerContextRecord, { kind: "preview-annotation" }>;
  recordsById: ReadonlyMap<string, ComposerContextRecord>;
  userImages: ReadonlyArray<ChatImageAttachment>;
  previewImages: ReadonlyArray<ChatImageAttachment>;
  annotationRecordIds: ReadonlyArray<string>;
}): ChatImageAttachment | null {
  const screenshotRecord = input.record.screenshotContextId
    ? asKnownContextRecord(input.recordsById.get(input.record.screenshotContextId))
    : undefined;
  return (
    (screenshotRecord?.kind === "image"
      ? input.userImages.find((image) => image.id === screenshotRecord.attachmentId)
      : undefined) ??
    input.previewImages.find(
      (image) => image.name === `preview-annotation-${input.record.annotationId}.png`,
    ) ??
    input.previewImages[input.annotationRecordIds.indexOf(input.record.contextId)] ??
    null
  );
}

type UserMessageTurnItem = Extract<ThreadStructureTurnItem, { type: "user_message" }>;

/** The steer the steer shortcut sends now when no queued message can take it. */
export function oldestWaitingSteer(
  turnItems: ReadonlyArray<ThreadStructureTurnItem>,
): UserMessageTurnItem | undefined {
  return turnItems.find(
    (item): item is UserMessageTurnItem =>
      item.type === "user_message" &&
      item.status === "pending" &&
      (item.inputIntent === "steer" || item.inputIntent === "promoted_queued_to_steer") &&
      item.sentNow !== true,
  );
}
