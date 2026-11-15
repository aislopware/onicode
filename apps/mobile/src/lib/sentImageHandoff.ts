export interface SentImageHandoff {
  readonly uri: string;
  readonly aspectRatio: number;
}

// A sent image's local pixels and shape, so the pending bubble and then the delivered copy open at
// their final size over pixels already on screen instead of a placeholder that later resizes.
// The server re-ids attachments into the thread, so delivered images match by message and position,
// and by name, since editing a queued message can put another image at the same position.
const MAX_ENTRIES = 64;
const images = new Map<string, SentImageHandoff & { readonly name: string }>();

function remember(key: string, name: string, image: SentImageHandoff) {
  if (!Number.isFinite(image.aspectRatio) || image.aspectRatio <= 0) return;
  images.delete(key);
  images.set(key, { uri: image.uri, aspectRatio: image.aspectRatio, name });
  if (images.size > MAX_ENTRIES) images.delete(images.keys().next().value!);
}

export function rememberDraftImage(
  attachmentId: string,
  name: string,
  image: SentImageHandoff,
): void {
  remember(`draft:${attachmentId}`, name, image);
}

export function rememberSentImage(
  messageId: string,
  index: number,
  name: string,
  image: SentImageHandoff,
): void {
  remember(`message:${messageId}:${index}`, name, image);
}

/** Drops a message's images after an edit, which can replace an image with one of the same name. */
export function forgetSentImages(messageId: string): void {
  const prefix = `message:${messageId}:`;
  for (const key of images.keys()) if (key.startsWith(prefix)) images.delete(key);
}

export function sentImageHandoff(
  messageId: string,
  index: number,
  name: string,
  draftAttachmentId?: string,
): SentImageHandoff | undefined {
  const image =
    images.get(`message:${messageId}:${index}`) ??
    (draftAttachmentId === undefined ? undefined : images.get(`draft:${draftAttachmentId}`));
  return image?.name === name ? { uri: image.uri, aspectRatio: image.aspectRatio } : undefined;
}
