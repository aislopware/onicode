import type { ScopedThreadRef } from "@onicode/contracts";
import { splitMarkdown } from "@onicode/shared/markdownChunks";
import type { FileOptions } from "@pierre/diffs/react";
import { useGT } from "gt-react";
import { lazy, Suspense, useImperativeHandle, useState, type RefObject } from "react";

import { ChunkedMarkdown } from "./ChunkedMarkdown";
import { FileSurfaceLoading } from "./fileSurfaceChrome";
import { useFullFileMarkdown, useFullFileRevealWait, useFullFileText } from "./fullFileText";
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

interface RevealState {
  readonly held: boolean;
  readonly releases: number;
  /** The request the reader scrolled away from while it waited: it never scrolls after that. */
  readonly abandoned: number | null;
  /** The request the source viewer took unheld, which the plain viewer then does not repeat. */
  readonly passed: number | null;
}

function FullTextPreview(
  props: Parameters<typeof LargeFilePreview>[0] & { readonly estimatedLines: number },
) {
  const file = useFullFileText(props.src, props.preview);
  // A changed callback alone may not render again, so the viewer remounts once the line arrives.
  const [reveal, setReveal] = useState<RevealState>({
    held: false,
    releases: 0,
    abandoned: null,
    passed: null,
  });
  const abandoned = reveal.abandoned === props.revealRequestId;
  const endsPlain =
    typeof file !== "string" && largeFileEndsPlain(file, props.markdownCwd !== null);
  const revealHeld =
    typeof file !== "string" &&
    !endsPlain &&
    props.onPostRender !== undefined &&
    !largeFileRevealReady(props.revealLine, file);
  useFullFileRevealWait(props.src, revealHeld && !abandoned ? props.revealLine : null);
  const plainRevealLine =
    abandoned || reveal.passed === props.revealRequestId ? null : props.revealLine;
  if (typeof file === "string") return <PlainViewer {...props} revealLine={plainRevealLine} />;
  // The worker takes the finished text, so the file does not download again.
  if (endsPlain) return <PlainViewer {...props} text={file.text} revealLine={plainRevealLine} />;
  const reachesSource =
    !revealHeld &&
    !abandoned &&
    props.revealLine !== null &&
    props.onPostRender !== undefined &&
    props.markdownCwd === null;
  if (reveal.held !== revealHeld || (reachesSource && reveal.passed !== props.revealRequestId)) {
    setReveal({
      ...reveal,
      held: revealHeld,
      releases: reveal.releases + (reveal.held && !revealHeld && !abandoned ? 1 : 0),
      passed: reachesSource ? props.revealRequestId : reveal.passed,
    });
  }
  const onPostRender =
    (revealHeld || abandoned) && props.onPostRender
      ? holdReveal(props.onPostRender)
      : props.onPostRender;
  const abandonHeldReveal = () => {
    const requestId = props.revealRequestId;
    if (revealHeld && !abandoned) setReveal((current) => ({ ...current, abandoned: requestId }));
  };
  return (
    <>
      <CopyText copyContents={props.copyContents} text={file.done ? file.text : null} />
      {props.markdownCwd !== null ? (
        <MarkdownText {...props} markdownCwd={props.markdownCwd} file={file} />
      ) : (
        <div
          className="contents"
          onWheelCapture={abandonHeldReveal}
          onTouchStartCapture={abandonHeldReveal}
          onPointerDownCapture={abandonHeldReveal}
          onKeyDownCapture={abandonHeldReveal}
        >
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
        </div>
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
  const { chunks, definitions, footnotes } =
    useFullFileMarkdown(props.src, props.file.text) ??
    splitMarkdown(props.file.text, undefined, props.file.done);
  return (
    <ChunkedMarkdown
      chunks={chunks}
      definitions={definitions}
      footnotes={footnotes}
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
