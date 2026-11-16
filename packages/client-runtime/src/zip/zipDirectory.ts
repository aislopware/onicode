// @effect-diagnostics globalDate:off globalTimers:off - Plain parsing for web workers and React Native, outside an Effect runtime.
import * as Schema from "effect/Schema";
import { Inflate } from "fflate";

// Deflate expands at most about 1032:1, so one slice inflates to at most ~16 MiB before limits run.
const INFLATE_SLICE_BYTES = 16 * 1024;
// Office section parsing keeps a 128 KiB window per write, so no chunk may exceed 64 KiB.
const CHUNK_BYTES = 64 * 1024;
const YIELD_EVERY_BYTES = 1024 * 1024;

export type ZipEntry = {
  name: string;
  originalName: string;
  directory: boolean;
  date: Date;
  size: number;
  read(onChunk: (chunk: Uint8Array) => void): Promise<void>;
};

export class UnreadableZipEntryError extends Schema.TaggedError<UnreadableZipEntryError>()(
  "UnreadableZipEntryError",
  { message: Schema.String },
) {}

export function readZipDirectory(input: ArrayBuffer | Uint8Array, entryLimit = 2048) {
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let directoryEnd = -1;
  let directorySize = 0;
  let directoryOffset = 0;
  for (
    let offset = bytes.byteLength - 22;
    offset >= Math.max(0, bytes.byteLength - 65557);
    offset--
  ) {
    if (view.getUint32(offset, true) !== 0x06054b50) continue;
    if (offset + 22 + view.getUint16(offset + 20, true) !== bytes.byteLength)
      throw new Error("This is not a readable ZIP archive.");
    let count = view.getUint16(offset + 10, true);
    directorySize = view.getUint32(offset + 12, true);
    directoryOffset = view.getUint32(offset + 16, true);
    directoryEnd = offset;
    if (
      [4, 6, 8, 10].some((field) => view.getUint16(offset + field, true) === 0xffff) ||
      directorySize === 0xffffffff ||
      directoryOffset === 0xffffffff
    ) {
      const locator = offset - 20;
      const record = locator - 56;
      // Locator signatures in the comment are rejected so only one ZIP64 record can apply.
      for (let candidate = bytes.byteLength - 4; candidate > locator; candidate--)
        if (view.getUint32(candidate, true) === 0x07064b50)
          throw new Error("Unsupported ZIP64 directory.");
      if (
        record < 0 ||
        view.getUint32(locator, true) !== 0x07064b50 ||
        view.getUint32(record, true) !== 0x06064b50 ||
        view.getBigUint64(record + 4, true) !== 44n ||
        view.getUint32(record + 16, true) !== 0 ||
        view.getUint32(record + 20, true) !== 0 ||
        view.getUint32(locator + 4, true) !== 0 ||
        view.getUint32(locator + 16, true) !== 1 ||
        view.getBigUint64(locator + 8, true) !== BigInt(record)
      )
        throw new Error("Unsupported ZIP64 directory.");
      const records = view.getBigUint64(record + 32, true);
      const size = view.getBigUint64(record + 40, true);
      const start = view.getBigUint64(record + 48, true);
      if (
        records > BigInt(entryLimit) ||
        size > BigInt(record) ||
        start > BigInt(record) ||
        view.getBigUint64(locator + 8, true) !== start + size ||
        view.getBigUint64(record + 24, true) !== records
      )
        throw new Error("This ZIP64 directory is too large or damaged.");
      count = Number(records);
      directorySize = Number(size);
      directoryOffset = Number(start);
      directoryEnd = record;
    }
    if (count > entryLimit) throw new Error("This archive contains more than 2,048 entries.");
    if (directoryOffset > directoryEnd - directorySize)
      throw new Error("This ZIP directory is damaged.");
    break;
  }
  if (directoryEnd < 0 || directorySize > directoryEnd)
    throw new Error("This is not a readable ZIP archive.");
  const records: number[] = [];
  for (let offset = directoryEnd - directorySize; offset < directoryEnd;) {
    if (offset + 46 > directoryEnd || view.getUint32(offset, true) !== 0x02014b50)
      throw new Error("This ZIP directory is damaged.");
    if (records.push(offset) > entryLimit)
      throw new Error("This archive contains more than 2,048 entries.");
    const nameLength = view.getUint16(offset + 28, true);
    if (nameLength > 1024) throw new Error("An archive path is too long to preview.");
    offset +=
      46 + nameLength + view.getUint16(offset + 30, true) + view.getUint16(offset + 32, true);
    if (offset > directoryEnd) throw new Error("This ZIP directory is damaged.");
  }
  // Data prepended to the archive (such as a self-extractor stub) shifts every local offset.
  const shift = directoryEnd - directorySize - directoryOffset;
  const entries = new Map<string, ZipEntry>();
  for (const offset of records) {
    const entry = zipEntry(bytes, view, offset, shift);
    entries.set(entry.name, entry);
  }
  return entries;
}

function zipEntry(bytes: Uint8Array, view: DataView, offset: number, shift: number) {
  const flags = view.getUint16(offset + 8, true);
  const compression = view.getUint16(offset + 10, true);
  const time = view.getUint32(offset + 12, true);
  const nameLength = view.getUint16(offset + 28, true);
  const extraStart = offset + 46 + nameLength;
  const extraEnd = extraStart + view.getUint16(offset + 30, true);
  const external = view.getUint32(offset + 38, true);
  let compressedSize = view.getUint32(offset + 20, true);
  let size = view.getUint32(offset + 24, true);
  let local = view.getUint32(offset + 42, true);
  if (flags & 1)
    throw new UnreadableZipEntryError({ message: "Encrypted ZIP entries are not supported." });
  if (compression !== 0 && compression !== 8)
    throw new UnreadableZipEntryError({
      message: "This ZIP archive uses an unsupported compression method.",
    });
  let storedName = bytes.subarray(offset + 46, extraStart);
  for (let field = extraStart; field + 4 <= extraEnd;) {
    const end = Math.min(field + 4 + view.getUint16(field + 2, true), extraEnd);
    const id = view.getUint16(field, true);
    if (id === 1) {
      let value = field + 4;
      const next = (current: number) => {
        if (current !== 0xffffffff || value + 8 > end) return current;
        value += 8;
        return Number(view.getBigUint64(value - 8, true));
      };
      size = next(size);
      compressedSize = next(compressedSize);
      local = next(local);
    } else if (
      // Info-ZIP Unicode Path: the UTF-8 name, stale once its CRC no longer matches the stored name.
      id === 0x7075 &&
      !(flags & 0x800) &&
      end - field >= 9 &&
      bytes[field + 4] === 1 &&
      view.getUint32(field + 5, true) === crc32(storedName)
    )
      storedName = bytes.subarray(field + 9, end);
    field = end;
  }
  const header = local + shift;
  if (header + 30 > bytes.length || view.getUint32(header, true) !== 0x04034b50)
    throw damagedEntry();
  const start = header + 30 + view.getUint16(header + 26, true) + view.getUint16(header + 28, true);
  if (start + compressedSize > bytes.length) throw damagedEntry();
  const originalName = new TextDecoder().decode(storedName);
  const directory =
    (external & 0x10) !== 0 ||
    (view.getUint16(offset + 4, true) >> 8 === 3 && ((external >>> 16) & 0x4000) !== 0) ||
    originalName.endsWith("/");
  const name = resolvePath(originalName);
  const data = bytes.subarray(start, start + compressedSize);
  return {
    name: directory && !name.endsWith("/") ? `${name}/` : name,
    originalName,
    directory,
    date: new Date(
      Date.UTC(
        ((time >>> 25) & 0x7f) + 1980,
        ((time >>> 21) & 0x0f) - 1,
        (time >>> 16) & 0x1f,
        (time >>> 11) & 0x1f,
        (time >>> 5) & 0x3f,
        (time & 0x1f) << 1,
      ),
    ),
    size,
    async read(onChunk: (chunk: Uint8Array) => void) {
      let emitted = 0;
      const emit = (chunk: Uint8Array) => {
        if ((emitted += chunk.length) > size) throw damagedEntry();
        for (let at = 0; at < chunk.length; at += CHUNK_BYTES)
          onChunk(chunk.subarray(at, at + CHUNK_BYTES));
      };
      const inflate = compression === 8 ? new Inflate(emit) : null;
      let position = 0;
      do {
        const slice = data.subarray(position, (position += INFLATE_SLICE_BYTES));
        if (inflate) inflate.push(slice, position >= data.length);
        else emit(slice);
        if (position < data.length && position % YIELD_EVERY_BYTES === 0)
          await new Promise((resolve) => setTimeout(resolve, 0));
      } while (position < data.length);
      if (emitted !== size) throw damagedEntry();
    },
  } satisfies ZipEntry;
}

function damagedEntry() {
  return new UnreadableZipEntryError({ message: "This ZIP archive has a damaged entry." });
}

function crc32(bytes: Uint8Array) {
  let crc = ~0;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return ~crc >>> 0;
}

// Resolves "." and ".." segments so a name cannot climb above the archive root.
function resolvePath(path: string) {
  const parts = path.split("/");
  const result: string[] = [];
  parts.forEach((part, index) => {
    if (part === "." || (part === "" && index !== 0 && index !== parts.length - 1)) return;
    if (part === "..") result.pop();
    else result.push(part);
  });
  return result.join("/");
}
