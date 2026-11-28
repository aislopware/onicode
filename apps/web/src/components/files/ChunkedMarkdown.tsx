import type { ScopedThreadRef } from "@onicode/contracts";
import { LegendList, type LegendListRef } from "@legendapp/list/react";
import {
  type KeyboardEventHandler,
  type MouseEvent as ReactMouseEvent,
  useEffect,
  useRef,
  useState,
} from "react";

import { markdownFragmentSlug } from "~/components/chat/markdownFragments";
import { Spinner } from "~/components/ui/spinner";
import type { MarkdownExtensions } from "~/markdown/wordsHast";
import { FileMarkdownPreview } from "./FileMarkdownPreview";
import { LineCappedText, useMoreCharactersLabel } from "./shownText";
import { createPositionMemory, previewPositionKey } from "./previewPosition";
import {
  buildMarkdownFootnotesSection,
  findFootnoteRefChunkIndex,
  joinMarkdownChunks,
  LARGE_MARKDOWN_CHUNK_MAX_CHARS,
  markdownHeadingPiece,
  type MarkdownChunk,
  type MarkdownFootnotes,
  type MarkdownFootnotesSection,
  prepareMarkdownChunk,
} from "@onicode/shared/markdownChunks";

const ESTIMATED_LINE_PX = 24;
const FOOTNOTE_TARGET_ID = /^fn-\d+$/;
const FOOTNOTE_REF_ID = /^fnref-(\d+)(?:-(\d+))?$/;

const CHUNK_FOOTNOTES_OMIT: MarkdownExtensions = { footnotes: "omit" };
const CHUNK_FOOTNOTES_OMIT_SEEDED: MarkdownExtensions = { footnotes: "omit-seeded" };
const CHUNK_FOOTNOTES_ONLY: MarkdownExtensions = { footnotes: "only" };

const savedPositions = createPositionMemory<{
  readonly index: number;
  readonly viewOffset: number;
}>();

type ChunkItem =
  | {
      readonly kind: "chunk";
      readonly index: number;
      readonly start: number;
      readonly chunk: MarkdownChunk;
    }
  | {
      readonly kind: "footnotes";
      readonly index: number;
      readonly section: MarkdownFootnotesSection;
    };

interface HeadingTarget {
  readonly index: number;
  readonly id: string;
}

function mountedPiece(root: HTMLElement, target: HeadingTarget): Element | null {
  return root.querySelector(`[data-markdown-piece="${target.index}"]`);
}

function pieceHeading(piece: Element, target: HeadingTarget): HTMLElement | null {
  for (const element of piece.querySelectorAll<HTMLElement>("[id]")) {
    if (markdownFragmentSlug(`#${element.id}`) === target.id) return element;
  }
  return null;
}

/** Renders Markdown pieces in a virtualized list; task offsets are file offsets. */
export function ChunkedMarkdown(props: {
  readonly chunks: readonly MarkdownChunk[];
  /** Link definitions from the whole file, so references resolve across pieces. */
  readonly definitions: string;
  /** Document-wide footnotes, so references and definitions resolve across pieces. */
  readonly footnotes?: MarkdownFootnotes | undefined;
  /** Shows a spinner after the last chunk while more of the file streams in. */
  readonly loading?: boolean;
  readonly cwd: string;
  readonly relativePath: string;
  readonly threadRef: ScopedThreadRef;
  readonly workspaceMutationId: string | null;
  readonly onKeyDown?: KeyboardEventHandler<HTMLDivElement>;
  readonly onTaskListChange?:
    | ((input: { readonly markerOffset: number; readonly checked: boolean }) => void)
    | undefined;
}) {
  const { chunks, footnotes, onTaskListChange } = props;
  const more = useMoreCharactersLabel();
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<LegendListRef | null>(null);
  const positionKey = previewPositionKey(
    props.threadRef.environmentId,
    props.threadRef.threadId,
    props.cwd,
    props.relativePath,
  );
  // A linked heading in a piece the list has not mounted, revealed once it mounts. It belongs to
  // the file and thread it was clicked in, which a caller may swap without remounting it.
  const [pendingLink, setPendingLink] = useState<{
    readonly positionKey: string;
    readonly target: HeadingTarget;
  } | null>(null);
  const pendingHeading = pendingLink?.positionKey === positionKey ? pendingLink.target : null;
  const setPendingHeading = (target: HeadingTarget | null) =>
    setPendingLink(target === null ? null : { positionKey, target });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- waits for the virtualized list to mount the piece holding a linked heading
  useEffect(() => {
    const root = rootRef.current;
    if (pendingHeading === null || !root) return;
    // A mounted piece already holds its headings; a cut or plain-text piece never will.
    const reveal = () => {
      const piece = mountedPiece(root, pendingHeading);
      if (piece === null) return;
      pieceHeading(piece, pendingHeading)?.scrollIntoView({ block: "start" });
      setPendingLink(null);
    };
    const observer = new MutationObserver(reveal);
    observer.observe(root, { childList: true, subtree: true });
    const frame = requestAnimationFrame(reveal);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [pendingHeading]);
  const dropPendingHeading = () => setPendingHeading(null);
  // Pieces past the draw distance are not mounted, so a heading link finds its piece in the text.
  const revealLinkedHeading = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
      return;
    }
    const href =
      event.target instanceof Element ? event.target.closest("a")?.getAttribute("href") : undefined;
    if (!href?.startsWith("#")) return;
    const slug = markdownFragmentSlug(href);
    const target = footnoteTarget(slug) ?? markdownHeadingPiece(chunks, slug);
    if (target === null) return;
    event.preventDefault();
    const piece = mountedPiece(event.currentTarget, target);
    if (piece !== null) {
      pieceHeading(piece, target)?.scrollIntoView({ block: "start" });
      setPendingHeading(null);
      return;
    }
    setPendingHeading(target);
    void listRef.current?.scrollToIndex({ index: target.index, viewPosition: 0, animated: false });
  };
  const items: ChunkItem[] = [];
  let start = 0;
  let lines = 0;
  for (const [index, chunk] of chunks.entries()) {
    items.push({ kind: "chunk", index, start, chunk });
    lines += chunk.lines;
    start += chunk.text.length + 1;
  }
  const footnotesSection = buildMarkdownFootnotesSection(props.definitions, footnotes);
  const footnotesIndex = footnotesSection === null ? null : items.length;
  if (footnotesSection !== null) {
    items.push({ kind: "footnotes", index: items.length, section: footnotesSection });
  }
  // Without shared footnotes each piece keeps its own section, which ChatMarkdown resolves.
  function footnoteTarget(slug: string): HeadingTarget | null {
    if (!footnotes) return null;
    if (FOOTNOTE_TARGET_ID.test(slug)) {
      return footnotesIndex === null ? null : { index: footnotesIndex, id: slug };
    }
    const ref = FOOTNOTE_REF_ID.exec(slug);
    if (!ref) return null;
    const index = findFootnoteRefChunkIndex(footnotes, Number(ref[1]), Number(ref[2] ?? 1));
    return index === null ? null : { index, id: slug };
  }
  const saved = savedPositions.get(positionKey);
  // The list applies its initial index once, so wait for the saved chunk to stream in.
  const awaitingSaved = props.loading && saved !== undefined && saved.index >= items.length;
  const spinner = (
    <div className="flex justify-center py-4">
      <Spinner size="md" tone="muted" />
    </div>
  );
  if (awaitingSaved) return spinner;
  return (
    <div
      ref={rootRef}
      // Clicking the text focuses the preview, so Select All stays inside it.
      tabIndex={-1}
      className="flex min-h-0 flex-1 flex-col outline-none"
      onKeyDown={props.onKeyDown}
      onClickCapture={revealLinkedHeading}
      // Scrolling or other input drops a heading still waiting for its piece.
      onWheelCapture={dropPendingHeading}
      onTouchStartCapture={dropPendingHeading}
      onPointerDownCapture={dropPendingHeading}
      onKeyDownCapture={dropPendingHeading}
      onCopy={(event) => {
        // Select All only reaches rendered pieces; copying all of it copies the whole file.
        const list = event.currentTarget.firstElementChild;
        const selection = window.getSelection();
        if (!list || !selection?.rangeCount) return;
        // Chromium's containsNode says false for a list selected through its parent's children.
        const range = selection.getRangeAt(0);
        const listRange = document.createRange();
        listRange.selectNode(list);
        if (
          range.compareBoundaryPoints(Range.START_TO_START, listRange) > 0 ||
          range.compareBoundaryPoints(Range.END_TO_END, listRange) < 0
        ) {
          return;
        }
        event.preventDefault();
        // A rendered piece's own handler already wrote just its part.
        event.clipboardData.clearData();
        event.clipboardData.setData("text/plain", joinMarkdownChunks(chunks));
      }}
    >
      <LegendList<ChunkItem>
        // The initial index only applies on mount, so another file or thread needs a new list.
        key={positionKey}
        ref={listRef}
        data={items}
        {...(saved && {
          initialScrollIndex: {
            index: Math.min(saved.index, Math.max(items.length - 1, 0)),
            viewOffset: saved.viewOffset,
          },
        })}
        onScroll={() => {
          const list = listRef.current?.getState();
          if (!list) return;
          savedPositions.set(positionKey, {
            index: list.start,
            viewOffset: list.positionAtIndex(list.start) - list.scroll,
          });
        }}
        extraData={
          footnotes
            ? `${props.definitions}\0${footnotes.stubs}\0${footnotes.definitions}\0${footnotes.chunkRefs.map((refs) => refs.join(",")).join("|")}`
            : props.definitions
        }
        keyExtractor={(item) => (item.kind === "footnotes" ? "footnotes" : String(item.index))}
        estimatedItemSize={(lines / Math.max(items.length, 1)) * ESTIMATED_LINE_PX}
        drawDistance={800}
        recycleItems={false}
        className="h-full scrollbar-gutter-both"
        contentContainerClassName="py-5"
        ListFooterComponent={props.loading ? spinner : null}
        renderItem={({ item }) => {
          if (item.kind === "footnotes") {
            return (
              <div className="pb-5" data-markdown-piece={item.index}>
                <FileMarkdownPreview
                  text={item.section.text}
                  cwd={props.cwd}
                  relativePath={props.relativePath}
                  threadRef={props.threadRef}
                  workspaceMutationId={props.workspaceMutationId}
                  className="mx-auto max-w-4xl px-6"
                  extensions={CHUNK_FOOTNOTES_ONLY}
                  onTaskListChange={
                    onTaskListChange &&
                    ((input) => {
                      const markerOffset = item.section.fileOffset(input.markerOffset);
                      if (markerOffset !== null) {
                        onTaskListChange({ markerOffset, checked: input.checked });
                      }
                    })
                  }
                />
              </div>
            );
          }
          if (item.chunk.text.length > LARGE_MARKDOWN_CHUNK_MAX_CHARS) {
            return (
              <div className="pb-5" data-markdown-piece={item.index}>
                {/* One block this long (a huge fence or HTML block) is too slow to parse. */}
                <pre className="mx-auto max-w-4xl overflow-x-auto px-6 font-mono text-xs whitespace-pre">
                  <LineCappedText
                    text={item.chunk.text}
                    more={more}
                    total={Number.POSITIVE_INFINITY}
                  />
                </pre>
              </div>
            );
          }
          const prepared = prepareMarkdownChunk(
            item.chunk.text,
            item.index,
            props.definitions,
            footnotes,
          );
          return (
            <div className="pb-5" data-markdown-piece={item.index}>
              <FileMarkdownPreview
                text={item.chunk.text}
                definitions={prepared.prefix}
                cwd={props.cwd}
                relativePath={props.relativePath}
                threadRef={props.threadRef}
                workspaceMutationId={props.workspaceMutationId}
                className="mx-auto max-w-4xl px-6"
                extensions={
                  prepared.footnotesMode === "omit-seeded"
                    ? CHUNK_FOOTNOTES_OMIT_SEEDED
                    : prepared.footnotesMode === "omit"
                      ? CHUNK_FOOTNOTES_OMIT
                      : undefined
                }
                onTaskListChange={
                  onTaskListChange &&
                  ((input) =>
                    onTaskListChange({
                      markerOffset: item.start + input.markerOffset - prepared.offsetShift,
                      checked: input.checked,
                    }))
                }
              />
            </div>
          );
        }}
      />
    </div>
  );
}
