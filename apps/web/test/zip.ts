import { strFromU8, strToU8, unzipSync, zipSync } from "fflate";

/** Stores entries uncompressed unless `deflate` is set. Names ending in "/" are directories. */
export function zipFixture(
  files: Record<string, string | Uint8Array>,
  { deflate = false, comment = "" } = {},
) {
  const zipped = zipSync(
    Object.fromEntries(
      Object.entries(files).map(([path, content]) => [
        path,
        typeof content === "string" ? strToU8(content) : content,
      ]),
    ),
    { level: deflate ? 6 : 0 },
  );
  if (!comment) return zipped;
  const text = strToU8(comment);
  const bytes = new Uint8Array(zipped.length + text.length);
  bytes.set(zipped);
  bytes.set(text, zipped.length);
  new DataView(bytes.buffer).setUint16(zipped.length - 2, text.length, true);
  return bytes;
}

export function zipEntries(bytes: ArrayBuffer | Uint8Array) {
  const files = unzipSync(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
  return {
    names: Object.keys(files),
    bytes: (path: string) => files[path],
    text: (path: string) => (files[path] ? strFromU8(files[path]) : undefined),
  };
}
