import { useAtomSet, useAtomValue } from "@effect/atom-react";
import {
  extendMarkdownSplit,
  splitMarkdown,
  type SplitMarkdown,
} from "@onicode/shared/markdownChunks";
import * as Effect from "effect/Effect";
import { FetchHttpClient, HttpClient, HttpClientResponse } from "effect/http";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import { AsyncResult, Atom } from "effect/reactivity";
import { useEffect } from "react";

import { connectionAtomRuntime } from "~/connection/runtime";

import { countNewlines, type ShownFileText } from "./largeFilePreviewRoute";
import { FULL_TEXT_MAX_BYTES, RENDERED_MAX_LINES } from "./largeTextProtocol";
import { hasLineLongerThan, MAX_SHOWN_LINE_CHARS } from "./shownText";

// A slow download swaps in what has arrived this often, so the reader is not left at 1 MB.
const SWAP_INTERVAL = "1 second";
// Switching back to a recently closed file reuses its text instead of downloading it again.
const RECENT_FILE_TTL_MS = 60_000;

interface Downloaded {
  readonly text: string;
  readonly lines: number;
}

// Past this the main thread cannot hold the text. Past the line cap it still downloads, so the
// plain viewer takes the finished text instead of downloading it again.
const tooLarge = (file: Downloaded) => file.text.length > FULL_TEXT_MAX_BYTES;

/** Bytes the 1 MB preview read would also refuse as text. */
class FullFileTextBinaryError extends Schema.TaggedError<FullFileTextBinaryError>()(
  "FullFileTextBinaryError",
  {},
) {}

/** Fails on invalid UTF-8 or a NUL byte like the 1 MB preview read, instead of showing U+FFFD. */
const decodeTextFatally = <E, R>(bytes: Stream.Stream<Uint8Array, E, R>) =>
  Stream.suspend(() => {
    // Drops a leading BOM, as the preview read does.
    const decoder = new TextDecoder("utf-8", { fatal: true });
    const decode = (chunk?: Uint8Array) =>
      Effect.try({
        try: () => decoder.decode(chunk, { stream: chunk !== undefined }),
        catch: () => new FullFileTextBinaryError(),
      }).pipe(
        Effect.filterOrFail(
          (text) => !text.includes("\0"),
          () => new FullFileTextBinaryError(),
        ),
      );
    return bytes.pipe(Stream.mapEffect(decode), Stream.concat(Stream.fromEffect(decode())));
  });

/** A file's text so far, at most once per interval, ending at its whole text or past a cap. */
export const downloadText = (src: string) =>
  HttpClient.get(src).pipe(
    Effect.flatMap(HttpClientResponse.filterStatusOk),
    Effect.map((response) => response.stream),
    Effect.provideService(FetchHttpClient.RequestInit, { cache: "no-store" }),
    Stream.unwrap,
    decodeTextFatally,
    Stream.scan(
      (): Downloaded => ({ text: "", lines: 1 }),
      (file, chunk) => ({ text: file.text + chunk, lines: file.lines + countNewlines(chunk) }),
    ),
    Stream.takeUntil(tooLarge),
    Stream.groupedWithin(Number.MAX_SAFE_INTEGER, SWAP_INTERVAL),
    Stream.map((swaps) => swaps[swaps.length - 1]!),
  );

/** `src` is the asset URL with its revision, so a changed file downloads again. */
const fullFileTextAtom = Atom.family((src: string) =>
  connectionAtomRuntime
    .atom(downloadText(src))
    .pipe(Atom.setIdleTTL(RECENT_FILE_TTL_MS), Atom.withLabel(`full-file-text:${src}`)),
);

export type FullFileText = ShownFileText | "too-large" | "failed";

/** The line a held reveal waits for in each file, so the swap that brings it shows at once. */
const revealWaitAtom = Atom.family((src: string) =>
  Atom.make<number | null>(null).pipe(Atom.withLabel(`full-file-reveal-wait:${src}`)),
);

/** Publishes the line a held reveal waits for while it waits. */
export function useFullFileRevealWait(src: string, line: number | null) {
  const setWait = useAtomSet(revealWaitAtom(src));
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- syncs the waiting line into the download's atom store
  useEffect(() => {
    setWait(line);
    return () => setWait(null);
  }, [setWait, line]);
}

/**
 * The text a swap shows. A streaming swap ends at a newline, so the last row does not flicker, and
 * shows only once it doubles the text shown, as each layout redoes the whole text, or once it
 * brings the line a reveal waits for. Past the line cap the finished text goes to the plain viewer.
 */
export function nextShownFileText(
  previous: FullFileText | null,
  file: Downloaded,
  done: boolean,
  waitingLine: number | null = null,
): FullFileText | null {
  if (done) {
    return { ...file, done, longLine: hasLineLongerThan(file.text, MAX_SHOWN_LINE_CHARS) };
  }
  if (file.lines > RENDERED_MAX_LINES) return previous;
  const text = file.text.slice(0, file.text.lastIndexOf("\n") + 1);
  // A streaming swap's whole lines are all of its newlines.
  const bringsWaitingLine =
    waitingLine !== null &&
    waitingLine <= file.lines - 1 &&
    (previous === null || typeof previous === "string" || (previous.lines ?? 0) - 1 < waitingLine);
  if (
    previous !== null &&
    typeof previous !== "string" &&
    !bringsWaitingLine &&
    text.length < Math.max(1, 2 * previous.text.length)
  ) {
    return previous;
  }
  return { text, done, lines: file.lines, longLine: false };
}

/** Each swap's shown text, shared by every reader of the file. */
const shownFileTextAtom = Atom.family((src: string) =>
  Atom.make((get): FullFileText | null => {
    const result = get(fullFileTextAtom(src));
    if (AsyncResult.isFailure(result)) return "failed";
    if (!AsyncResult.isSuccess(result)) return null;
    if (tooLarge(result.value)) return "too-large";
    const previous = Option.getOrNull(get.self<FullFileText | null>());
    return nextShownFileText(previous, result.value, !result.waiting, get(revealWaitAtom(src)));
  }).pipe(Atom.withLabel(`shown-file-text:${src}`)),
);

/** A file past the 1 MB read: the preview at once, then the full file as it downloads. */
export function useFullFileText(src: string, preview: string): FullFileText {
  const shown = useAtomValue(shownFileTextAtom(src));
  // The first swaps can hold less than the preview already shows.
  if (
    shown === null ||
    (typeof shown !== "string" && !shown.done && shown.text.length < preview.length)
  ) {
    return { text: preview, done: false, lines: null, longLine: false };
  }
  return shown;
}

interface MarkdownSplitState {
  readonly text: string;
  readonly split: SplitMarkdown;
}

/**
 * Splits a swap's text, cutting only the newly arrived tail when it extends the last swap's. A
 * finished text past the line cap goes to the plain viewer, so it is not split.
 */
export function nextMarkdownSplit(
  previous: MarkdownSplitState | null,
  shown: FullFileText | null,
): MarkdownSplitState | null {
  if (shown === null || typeof shown === "string" || (shown.lines ?? 0) > RENDERED_MAX_LINES) {
    return null;
  }
  const { text } = shown;
  if (previous?.text === text) return previous;
  // One download only appends, so a longer text starts with the one split before.
  const split =
    previous !== null && text.length > previous.text.length
      ? extendMarkdownSplit(previous.split, text)
      : splitMarkdown(text);
  return { text, split };
}

/** Markdown pieces of each swap, kept as long as the text; splitting 12 MB at once takes 0.5 s. */
const fullFileMarkdownAtom = Atom.family((src: string) =>
  Atom.make((get) =>
    nextMarkdownSplit(
      Option.getOrNull(get.self<MarkdownSplitState | null>()),
      get(shownFileTextAtom(src)),
    ),
  ).pipe(Atom.setIdleTTL(RECENT_FILE_TTL_MS), Atom.withLabel(`full-file-markdown:${src}`)),
);

/** Markdown pieces of the shown text, or null while the preview shows ahead of the download. */
export function useFullFileMarkdown(src: string, text: string): SplitMarkdown | null {
  const state = useAtomValue(fullFileMarkdownAtom(src));
  return state?.text === text ? state.split : null;
}
