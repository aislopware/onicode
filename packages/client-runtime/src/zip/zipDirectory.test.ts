import { strToU8, zipSync } from "fflate";
import { describe, expect, it } from "vite-plus/test";

import { readZipDirectory, UnreadableZipEntryError, type ZipEntry } from "./zipDirectory.ts";

async function text(entry: ZipEntry) {
  const decoder = new TextDecoder();
  let result = "";
  await entry.read((chunk) => (result += decoder.decode(chunk, { stream: true })));
  return result + decoder.decode();
}

function centralRecord(bytes: Uint8Array) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  for (let offset = 0; offset + 4 <= bytes.length; offset++)
    if (view.getUint32(offset, true) === 0x02014b50) return { view, offset };
  throw new Error("No central directory record.");
}

describe("readZipDirectory", () => {
  it("stops inflating as soon as a chunk passes the caller's limit", async () => {
    const entry = readZipDirectory(zipSync({ "bomb.bin": new Uint8Array(32 * 1024 * 1024) })).get(
      "bomb.bin",
    )!;
    let seen = 0;
    await expect(
      entry.read((chunk) => {
        seen += chunk.length;
        if (seen > 1024 * 1024) throw new Error("limit");
      }),
    ).rejects.toThrow("limit");
    expect(seen).toBeLessThanOrEqual(1024 * 1024 + 64 * 1024);
  });

  it("resolves relative names, keeps the last duplicate, and reads past prepended data", async () => {
    const zip = zipSync(
      {
        "a/../../escape.txt": strToU8("escaped"),
        "dup.txt": strToU8("first"),
        "./dup.txt": strToU8("second"),
        "folder/": new Uint8Array(),
      },
      { level: 0 },
    );
    const bytes = new Uint8Array(100 + zip.length);
    bytes.set(zip, 100);
    const entries = readZipDirectory(bytes);
    expect([...entries.keys()]).toEqual(["escape.txt", "dup.txt", "folder/"]);
    expect(entries.get("escape.txt")?.originalName).toBe("a/../../escape.txt");
    expect(await text(entries.get("escape.txt")!)).toBe("escaped");
    expect(await text(entries.get("dup.txt")!)).toBe("second");
    expect(entries.get("folder/")?.directory).toBe(true);
  });

  it("reads the Unicode path extra field unless its CRC is stale", () => {
    const unicodePath = (crc: number, name: string) => {
      const field = new Uint8Array([1, 0, 0, 0, 0, ...strToU8(name)]);
      new DataView(field.buffer).setUint32(1, crc, true);
      return { extra: { 0x7075: field } };
    };
    const entries = readZipDirectory(
      zipSync({
        "t_n.txt": [strToU8("a"), unicodePath(0xfd0a0612, "tên.txt")],
        "stale.txt": [strToU8("b"), unicodePath(0xfd0a0612, "renamed.txt")],
      }),
    );
    expect([...entries.keys()]).toEqual(["tên.txt", "stale.txt"]);
  });

  it.each([
    ["encrypted", 8, 1],
    ["compressed with an unknown method", 10, 99],
  ])("rejects an entry %s before reading any entry", (_, field, value) => {
    const bytes = zipSync({ "secret.txt": strToU8("secret") });
    const { view, offset } = centralRecord(bytes);
    view.setUint16(offset + field, value, true);
    expect(() => readZipDirectory(bytes)).toThrow(UnreadableZipEntryError);
  });

  it("rejects a local header offset outside the archive and truncated deflate data", async () => {
    const moved = zipSync({ "a.txt": strToU8("a".repeat(1000)) });
    const record = centralRecord(moved);
    record.view.setUint32(record.offset + 42, moved.length, true);
    expect(() => readZipDirectory(moved)).toThrow(UnreadableZipEntryError);

    const truncated = zipSync({ "a.txt": strToU8("abcdefghij".repeat(1000)) });
    const truncatedRecord = centralRecord(truncated);
    const size = truncatedRecord.view.getUint32(truncatedRecord.offset + 20, true);
    truncatedRecord.view.setUint32(truncatedRecord.offset + 20, size - 4, true);
    await expect(text(readZipDirectory(truncated).get("a.txt")!)).rejects.toThrow();
  });
});
