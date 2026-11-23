import type { ScopedThreadRef } from "@onicode/contracts";
import { splitMarkdown } from "@onicode/shared/markdownChunks";
import type { FileOptions } from "@pierre/diffs/react";
import { useGT } from "gt-react";
import { lazy, Suspense, useImperativeHandle, useState, type RefObject } from "react";

import { ChunkedMarkdown } from "./ChunkedMarkdown";
import { FileSurfaceLoading } from "./fileSurfaceChrome";
import { useFullFileMarkdown, useFullFileText } from "./fullFileText";
import {
  estimatedLines,
  highlightsLargeFile,
  largeFileEndsPlain,
  largeFileRevealReady,
  largeFileStartsPlain,
  type ShownFileText,
} from "./largeFilePreviewRoute";
import SourceFilePreview from "./ReadOnlySourcePreview";

const LargeTextViewer = lazy(() => import("./LargeTextViewer"));

/**
 * A file past the 1 MB read, shown like any other file once its full text arrives. Tables, and
 * files too large to hold or lay out, use the worker-backed plain viewer.
 */
export default function LargeFilePreview(props: {
  readonly src: string;
  readonly name: string;
  /** The first megabyte already read. */
  readonly preview: string;
  readonly sizeBytes: number;
  readonly delimiter: "," | "\t" | null;
  /** Renders the file as Markdown resolving links against this workspace; null shows source. */
  readonly markdownCwd: string | null;
  readonly threadRef: ScopedThreadRef;
  readonly workspaceMutationId: string | null;
  readonly copyContents: RefObject<(() => Promise<string>) | null>;
  readonly onRetry: () => void;
  readonly onPostRender?: FileOptions<unknown, undefined>["onPostRender"];
  readonly revealLine: number | null;
  readonly revealRequestId: number;
  readonly wordWrap: boolean;
}) {
  const lines = estimatedLines(props.preview, props.sizeBytes);
  const plain = largeFileStartsPlain({
    preview: props.preview,
    sizeBytes: props.sizeBytes,
    estimatedLines: lines,
    table: props.delimiter !== null,
    markdown: props.markdownCwd !== null,
  });
  return plain ? <PlainViewer {...props} /> : <FullTextPreview {...props} estimatedLines={lines} />;
}

type OnPostRender = NonNullable<FileOptions<unknown, undefined>["onPostRender"]>;

// Revealed now, a line past the text so far would stop at its last line and count as revealed.
const holdReveal =
  (onPostRender: OnPostRender): OnPostRender =>
  (node, instance, phase) =>
    phase === "unmount" ? onPostRender(node, instance, phase) : undefined;

function FullTextPreview(
  props: Parameters<typeof LargeFilePreview>[0] & { readonly estimatedLines: number },
) {
  const file = useFullFileText(props.src, props.preview);
  // A changed callback alone may not render again, so the viewer remounts once the line arrives.
  const [reveal, setReveal] = useState({ held: false, releases: 0 });
  if (file === "too-large" || file === "failed") return <PlainViewer {...props} />;
  // The worker takes the finished text, so the file does not download again.
  if (largeFileEndsPlain(file, props.markdownCwd !== null))
    return <PlainViewer {...props} text={file.text} />;
  const revealHeld =
    props.onPostRender !== undefined && !largeFileRevealReady(props.revealLine, file);
  if (reveal.held !== revealHeld) {
    setReveal({ held: revealHeld, releases: reveal.releases + (revealHeld ? 0 : 1) });
  }
  const onPostRender =
    revealHeld && props.onPostRender ? holdReveal(props.onPostRender) : props.onPostRender;
  return (
    <>
      <CopyText copyContents={props.copyContents} text={file.done ? file.text : null} />
      {props.markdownCwd !== null ? (
        <MarkdownText {...props} markdownCwd={props.markdownCwd} file={file} />
      ) : (
        <SourceFilePreview
          key={reveal.releases}
          name={props.name}
          text={file.text}
          onPostRender={onPostRender}
          wordWrap={props.wordWrap}
          tokenizeMaxLength={
            highlightsLargeFile(props.estimatedLines, file) ? Number.POSITIVE_INFINITY : 0
          }
        />
      )}
    </>
  );
}

function MarkdownText(
  props: Parameters<typeof LargeFilePreview>[0] & {
    readonly markdownCwd: string;
    readonly file: ShownFileText;
  },
) {
  // The preview ahead of the download splits once, as its text does not change between swaps.
  const { chunks, definitions } =
    useFullFileMarkdown(props.src, props.file.text) ?? splitMarkdown(props.file.text);
  return (
    <ChunkedMarkdown
      chunks={chunks}
      definitions={definitions}
      loading={!props.file.done}
      cwd={props.markdownCwd}
      relativePath={props.name}
      threadRef={props.threadRef}
      workspaceMutationId={props.workspaceMutationId}
    />
  );
}

function CopyText(props: {
  readonly copyContents: RefObject<(() => Promise<string>) | null>;
  readonly text: string | null;
}) {
  const gt = useGT();
  const { text } = props;
  useImperativeHandle(
    props.copyContents,
    () => () =>
      text === null
        ? Promise.reject(new Error(gt("The file is still loading. Try again in a moment.")))
        : Promise.resolve(text),
    [text, gt],
  );
  return null;
}

/** The first megabyte alone, for a draft whose server cannot send the whole file. */
export function PartialFilePreview(props: {
  readonly name: string;
  readonly text: string;
  readonly delimiter: "," | "\t" | null;
  readonly revealLine: number | null;
  readonly revealRequestId: number;
  readonly wordWrap: boolean;
}) {
  return (
    <Suspense fallback={<FileSurfaceLoading />}>
      <LargeTextViewer {...props} />
    </Suspense>
  );
}

function PlainViewer(
  props: Parameters<typeof LargeFilePreview>[0] & { readonly text?: string | undefined },
) {
  return (
    <Suspense fallback={<FileSurfaceLoading />}>
      <LargeTextViewer
        name={props.name}
        {...(props.text === undefined ? { src: props.src } : { text: props.text })}
        copyContents={props.copyContents}
        onRetry={props.onRetry}
        delimiter={props.delimiter}
        revealLine={props.revealLine}
        revealRequestId={props.revealRequestId}
        wordWrap={props.wordWrap}
      />
    </Suspense>
  );
}
