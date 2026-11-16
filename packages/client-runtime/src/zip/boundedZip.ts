import { readZipDirectory } from "./zipDirectory.ts";

export const ARCHIVE_MAX_BYTES = 50 * 1024 * 1024;
export const ARCHIVE_ENTRY_MAX_BYTES = 10 * 1024 * 1024;
export const ARCHIVE_MAX_ENTRIES = 2048;
export const ARCHIVE_MAX_RATIO = 200;

export async function boundedZip(
  bytes: ArrayBuffer,
  entryLimit = ARCHIVE_MAX_BYTES,
  limits: {
    totalLimit?: number;
    xmlLimit?: number;
    ratioFloor?: number;
    markupLimit?: number;
  } = {},
) {
  if (bytes.byteLength > ARCHIVE_MAX_BYTES)
    throw new Error("Preview limited to files under 50 MB.");
  const entries = readZipDirectory(bytes, ARCHIVE_MAX_ENTRIES);
  const files = new Map<
    string,
    { bytes: Uint8Array<ArrayBuffer>; date: Date; directory: boolean }
  >();
  let total = 0;
  let markup = 0;
  for (const entry of entries.values()) {
    if (entry.name.length > 1024) throw new Error("An archive path is too long to preview.");
    const chunks: Uint8Array[] = [];
    let length = 0;
    await entry.read((chunk) => {
      total += chunk.length;
      length += chunk.length;
      // Word relationship targets can use arbitrary extensions, so count every part.
      if (limits.markupLimit !== undefined) for (const byte of chunk) if (byte === 0x3c) markup++;
      if (
        markup > (limits.markupLimit ?? Infinity) ||
        total > (limits.totalLimit ?? ARCHIVE_MAX_BYTES) ||
        total >
          Math.max(limits.ratioFloor ?? 0, Math.max(1, bytes.byteLength) * ARCHIVE_MAX_RATIO) ||
        length > entryLimit ||
        (entry.name.endsWith(".xml") && length > (limits.xmlLimit ?? 8 * 1024 * 1024))
      )
        throw new Error(
          "The expanded archive exceeds the preview size or compression ratio limit.",
        );
      chunks.push(chunk);
    });
    const content = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      content.set(chunk, offset);
      offset += chunk.length;
    }
    files.set(entry.name, { bytes: content, date: entry.date, directory: entry.directory });
  }
  return files;
}
