import { expect, it } from "vite-plus/test";

import { buildZipTree, zipDownloadTimeLeft, type ZipEntry, type ZipTreeNode } from "./zipTree";

const timeLeft = (seconds: number) =>
  zipDownloadTimeLeft({ receivedBytes: 1, totalBytes: 2, elapsedMs: seconds * 1000 });

it("uses singular units when one minute or one second is left", () => {
  expect(timeLeft(1)).toBe("About 1 second left");
  expect(timeLeft(60)).toBe("About 1 minute left");
  expect(timeLeft(61)).toBe("About 2 minutes left");
});

it("lists ./-prefixed members from the archive root", () => {
  const entry = (path: string, isDirectory = false): ZipEntry => ({
    path,
    name: path,
    isDirectory,
    sizeBytes: 0,
    mimeType: "text/plain",
    bytes: new Uint8Array(0),
  });
  const outline = (node: ZipTreeNode): unknown =>
    node.entry ? node.path : { [node.label]: node.children.map(outline) };
  const tree = buildZipTree([
    entry("./", true),
    entry("./a/", true),
    entry("./a/dot.txt"),
    entry("a/hello.txt"),
    entry("./top.txt"),
  ]);
  expect(tree.map(outline)).toEqual([{ a: ["a/dot.txt", "a/hello.txt"] }, "top.txt"]);
});
