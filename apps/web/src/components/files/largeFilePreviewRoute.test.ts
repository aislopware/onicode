import { describe, expect, it } from "vite-plus/test";

import {
  estimatedLines,
  HIGHLIGHT_MAX_LINES,
  highlightsLargeFile,
  largeFileEndsPlain,
  largeFileRevealReady,
  largeFileStartsPlain,
} from "./largeFilePreviewRoute";
import { FULL_TEXT_MAX_BYTES, RENDERED_MAX_LINES } from "./largeTextProtocol";

// Both previews are 2,000 bytes: one line every 2 bytes, or one every 100.
const densePreview = "a\n".repeat(1000);
const sparsePreview = `${"a".repeat(99)}\n`.repeat(20);
const sizeBytes = 400_000;
const dense = estimatedLines(densePreview, sizeBytes);
const sparse = estimatedLines(sparsePreview, sizeBytes);

describe("largeFileStartsPlain", () => {
  const file = {
    preview: sparsePreview,
    sizeBytes,
    estimatedLines: sparse,
    table: false,
    markdown: false,
  };
  it.each([
    { name: "ordinary source", input: file, plain: false },
    { name: "a table", input: { ...file, table: true }, plain: true },
    {
      name: "past the bytes it can hold",
      input: { ...file, sizeBytes: FULL_TEXT_MAX_BYTES + 1 },
      plain: true,
    },
    // Its full text would stop at the cap and download again in the plain viewer.
    {
      name: "an estimate past the lines it can lay out",
      input: { ...file, estimatedLines: RENDERED_MAX_LINES + 1 },
      plain: true,
    },
    {
      name: "source with a long line",
      input: { ...file, preview: `${"a".repeat(10_001)}\n` },
      plain: true,
    },
    {
      name: "Markdown with a long line",
      input: { ...file, preview: `${"a".repeat(10_001)}\n`, markdown: true },
      plain: false,
    },
  ])("$name: plain $plain", ({ input, plain }) => {
    expect(largeFileStartsPlain(input)).toBe(plain);
  });
});

describe("highlightsLargeFile", () => {
  const over = HIGHLIGHT_MAX_LINES + 1;
  it.each([
    {
      name: "sparse preview, preview shown",
      estimate: sparse,
      lines: null,
      done: false,
      highlight: true,
    },
    {
      name: "dense preview, preview shown",
      estimate: dense,
      lines: null,
      done: false,
      highlight: false,
    },
    // A sparse prefix with a dense tail: dropped once the real count passes the limit.
    {
      name: "sparse prefix, dense tail streaming",
      estimate: sparse,
      lines: over,
      done: false,
      highlight: false,
    },
    {
      name: "sparse prefix, dense tail done",
      estimate: sparse,
      lines: over,
      done: true,
      highlight: false,
    },
    // A dense prefix with a sparse tail: not highlighted while streaming, then by the real count.
    {
      name: "dense prefix, sparse tail streaming",
      estimate: dense,
      lines: 5_000,
      done: false,
      highlight: false,
    },
    {
      name: "dense prefix, sparse tail done",
      estimate: dense,
      lines: 5_000,
      done: true,
      highlight: true,
    },
  ])("$name: $highlight", ({ estimate, lines, done, highlight }) => {
    expect(estimate > HIGHLIGHT_MAX_LINES).toBe(estimate === dense);
    expect(highlightsLargeFile(estimate, { text: "", done, lines, longLine: false })).toBe(
      highlight,
    );
  });
});

describe("largeFileEndsPlain", () => {
  it.each([
    { name: "finished source with a long line", longLine: true, markdown: false, plain: true },
    { name: "finished Markdown with a long line", longLine: true, markdown: true, plain: false },
    { name: "finished source", longLine: false, markdown: false, plain: false },
  ])("$name: plain $plain", ({ longLine, markdown, plain }) => {
    expect(largeFileEndsPlain({ text: "", done: true, lines: 3, longLine }, markdown)).toBe(plain);
  });
});

describe("largeFileRevealReady", () => {
  it.each([
    { name: "no link", line: null, text: "a\n", done: false, lines: 2, ready: true },
    { name: "a line already shown", line: 2, text: "a\nb\n", done: false, lines: 3, ready: true },
    {
      name: "a line not yet downloaded",
      line: 3,
      text: "a\nb\n",
      done: false,
      lines: 3,
      ready: false,
    },
    {
      name: "a line past the end of the finished file",
      line: 9,
      text: "a\nb\n",
      done: true,
      lines: 3,
      ready: true,
    },
    {
      name: "the preview's cut-off last line",
      line: 2,
      text: "a\nb",
      done: false,
      lines: null,
      ready: false,
    },
    {
      name: "a whole line of the preview",
      line: 1,
      text: "a\nb",
      done: false,
      lines: null,
      ready: true,
    },
  ])("$name: $ready", ({ line, text, done, lines, ready }) => {
    expect(largeFileRevealReady(line, { text, done, lines, longLine: false })).toBe(ready);
  });
});
