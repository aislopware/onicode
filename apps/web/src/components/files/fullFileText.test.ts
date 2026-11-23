import { describe, expect, it } from "@effect/vitest";
import { splitMarkdown } from "@onicode/shared/markdownChunks";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import { FetchHttpClient } from "effect/http";

import { downloadText, nextMarkdownSplit, nextShownFileText } from "./fullFileText";
import { RENDERED_MAX_LINES } from "./largeTextProtocol";
import { MAX_SHOWN_LINE_CHARS } from "./shownText";

const encoder = new TextEncoder();

const download = (...chunks: Uint8Array[]) =>
  downloadText("http://localhost/assets/file.ts").pipe(
    Stream.runLast,
    Effect.map(Option.getOrThrow),
    Effect.provide(FetchHttpClient.layer),
    Effect.provideService(FetchHttpClient.Fetch, () =>
      Promise.resolve(
        new Response(
          new ReadableStream({
            start(controller) {
              for (const chunk of chunks) controller.enqueue(chunk.slice());
              controller.close();
            },
          }),
        ),
      ),
    ),
  );

describe("downloadText", () => {
  it.effect(
    "drops a leading byte order mark as the preview read does, and keeps a later U+FEFF",
    () =>
      Effect.gen(function* () {
        const whole = new Uint8Array([0xef, 0xbb, 0xbf, ...encoder.encode("one\n﻿two\n")]);
        expect((yield* download(whole)).text).toBe("one\n﻿two\n");
        // The mark split across chunks is still the leading one.
        expect((yield* download(whole.subarray(0, 1), whole.subarray(1))).text).toBe("one\n﻿two\n");
      }),
  );

  it.effect("decodes a character split across two chunks", () =>
    Effect.gen(function* () {
      const bytes = encoder.encode("naïve €\n");
      const cut = bytes.indexOf(0xe2) + 1;
      expect((yield* download(bytes.subarray(0, cut), bytes.subarray(cut))).text).toBe("naïve €\n");
    }),
  );

  // Each would be copied as U+FFFD or shown as text, where the preview read calls it binary.
  it.effect.each([
    { name: "an invalid byte", chunks: [encoder.encode("one\n"), new Uint8Array([0xff])] },
    { name: "a cut-off last character", chunks: [encoder.encode("one\n€").subarray(0, 6)] },
    { name: "a NUL byte", chunks: [encoder.encode("one\n"), encoder.encode("t\0wo\n")] },
  ])("fails on $name", ({ chunks }) =>
    Effect.gen(function* () {
      expect((yield* Effect.exit(download(...chunks)))._tag).toBe("Failure");
    }),
  );

  it.effect("stops at the first text past the lines the main thread can lay out", () =>
    Effect.gen(function* () {
      const file = yield* download(encoder.encode("\n".repeat(RENDERED_MAX_LINES + 10)));
      expect(file.lines).toBeGreaterThan(RENDERED_MAX_LINES);
    }),
  );
});

describe("nextShownFileText", () => {
  const streamed = nextShownFileText(null, { text: "a\nb\npart", lines: 3 }, false);
  const longLine = `a\n${"b".repeat(MAX_SHOWN_LINE_CHARS + 1)}\n`;
  it.each([
    {
      name: "a first swap ends at its last newline",
      previous: null,
      file: { text: "a\nb\npart", lines: 3 },
      done: false,
      expected: { text: "a\nb\n", done: false, lines: 3, longLine: false },
    },
    {
      name: "a swap that doubles the text is shown",
      previous: streamed,
      file: { text: "a\nb\nc\nd\n", lines: 5 },
      done: false,
      expected: { text: "a\nb\nc\nd\n", done: false, lines: 5, longLine: false },
    },
    {
      name: "a finished text with a long line",
      previous: streamed,
      file: { text: longLine, lines: 3 },
      done: true,
      expected: { text: longLine, done: true, lines: 3, longLine: true },
    },
    {
      name: "a finished text",
      previous: streamed,
      file: { text: "a\nb", lines: 2 },
      done: true,
      expected: { text: "a\nb", done: true, lines: 2, longLine: false },
    },
  ])("$name", ({ previous, file, done, expected }) => {
    expect(nextShownFileText(previous, file, done)).toEqual(expected);
  });

  it("keeps the shown text until a swap doubles it, or has a whole line", () => {
    expect(nextShownFileText(streamed, { text: "a\nb\nc\n", lines: 4 }, false)).toBe(streamed);
    const empty = nextShownFileText(null, { text: "no line yet", lines: 1 }, false);
    expect(nextShownFileText(empty, { text: "no line yet, still", lines: 1 }, false)).toBe(empty);
  });
});

describe("nextMarkdownSplit", () => {
  const sections = (from: number, to: number) =>
    Array.from(
      { length: to - from },
      (_, index) => `# Section ${from + index}\n\n${"Some words. ".repeat(500)}\n\n`,
    ).join("");

  it("cuts only the tail a swap adds, keeping the earlier pieces", () => {
    const first = nextMarkdownSplit(null, sections(0, 4));
    const text = sections(0, 4) + sections(4, 8);
    const second = nextMarkdownSplit(first, text);
    expect(second.split).toEqual(splitMarkdown(text));
    const kept = first.split.chunks.length - 1;
    expect(kept).toBeGreaterThan(1);
    for (let index = 0; index < kept; index += 1) {
      expect(second.split.chunks[index]).toBe(first.split.chunks[index]);
    }
    expect(nextMarkdownSplit(second, text)).toBe(second);
  });
});
