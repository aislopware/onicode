import type {
  ComposerImageAttachment,
  PersistedComposerImageAttachment,
} from "../../composerDraftStore";
import { readFileAsDataUrl } from "../../lib/imageCompression";

// A recovered image is serialized by its intake and by the mounted composer at the same time.
const pendingReads = new WeakMap<Blob, Promise<string>>();

function readImageDataUrl(file: Blob): Promise<string> {
  const pending = pendingReads.get(file);
  if (pending) return pending;
  const read = readFileAsDataUrl(file).finally(() => pendingReads.delete(file));
  pendingReads.set(file, read);
  return read;
}

export async function serializeComposerImages(
  images: ReadonlyArray<ComposerImageAttachment>,
  persistedAttachments: ReadonlyArray<PersistedComposerImageAttachment>,
): Promise<PersistedComposerImageAttachment[]> {
  const existingPersistedById = new Map(
    persistedAttachments.map((attachment) => [attachment.id, attachment]),
  );
  const staged = await Promise.all(
    images.map(async (image) => {
      try {
        const dataUrl =
          existingPersistedById.get(image.id)?.dataUrl ?? (await readImageDataUrl(image.file));
        return {
          id: image.id,
          name: image.name,
          mimeType: image.mimeType,
          sizeBytes: image.sizeBytes,
          dataUrl,
          ...(image.source ? { source: image.source } : {}),
        };
      } catch {
        return existingPersistedById.get(image.id);
      }
    }),
  );
  return staged.filter((attachment) => attachment !== undefined);
}
