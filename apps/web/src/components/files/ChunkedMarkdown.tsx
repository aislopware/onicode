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
import { FileMarkdownPreview } from "./FileMarkdownPreview";
import { LineCappedText, useMoreCharactersLabel } from "./shownText";
import {
  LARGE_MARKDOWN_CHUNK_MAX_CHARS,
  markdownHeadingPiece,
  type MarkdownChunk,
} from "@onicode/shared/markdownChunks";

const ESTIMATED_LINE_PX = 24;
const MAX_SAVED_POSITIONS = 100;

/** Reading positions outlive the preview, which unmounts when the file or thread changes. */
const savedPositions = new Map<string, { readonly index: number; readonly viewOffset: number }>();

interface ChunkItem {
  readonly index: number;
  readonly start: number;
  readonly chunk: MarkdownChunk;
}

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
  const { chunks, onTaskListChange } = props;
  const more = useMoreCharactersLabel();
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<LegendListRef | null>(null);
  const positionKey = JSON.stringify([
    props.threadRef.environmentId,
    props.threadRef.threadId,
    props.cwd,
    props.relativePath,
  ]);
  // A linked heading in a piece the list has not mounted, revealed once it mounts. It belongs to
  // the thread it was clicked in, since the panel keeps this mounted across threads.
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
    const target = markdownHeadingPiece(chunks, markdownFragmentSlug(href));
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
    items.push({ index, start, chunk });
    lines += chunk.lines;
    start += chunk.text.length + 1;
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
        if (!list || !window.getSelection()?.containsNode(list)) return;
        event.preventDefault();
        event.clipboardData.setData("text/plain", chunks.map((chunk) => chunk.text).join("\n"));
      }}
    >
      <LegendList<ChunkItem>
        // The panel keeps this mounted across threads; the initial index only applies on mount.
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
          savedPositions.delete(positionKey);
          savedPositions.set(positionKey, {
            index: list.start,
            viewOffset: list.positionAtIndex(list.start) - list.scroll,
          });
          if (savedPositions.size > MAX_SAVED_POSITIONS) {
            savedPositions.delete(savedPositions.keys().next().value!);
          }
        }}
        extraData={props.definitions}
        keyExtractor={(item) => String(item.index)}
        estimatedItemSize={(lines / Math.max(items.length, 1)) * ESTIMATED_LINE_PX}
        drawDistance={800}
        recycleItems={false}
        className="h-full scrollbar-gutter-both"
        contentContainerClassName="py-5"
        ListFooterComponent={props.loading ? spinner : null}
        renderItem={({ item }) => (
          <div className="pb-5" data-markdown-piece={item.index}>
            {item.chunk.text.length > LARGE_MARKDOWN_CHUNK_MAX_CHARS ? (
              // One block this long (a huge fence or HTML block) is too slow to parse.
              <pre className="mx-auto max-w-4xl overflow-x-auto px-6 font-mono text-xs whitespace-pre">
                <LineCappedText
                  text={item.chunk.text}
                  more={more}
                  total={Number.POSITIVE_INFINITY}
                />
              </pre>
            ) : (
              <FileMarkdownPreview
                text={item.chunk.text}
                definitions={props.definitions}
                cwd={props.cwd}
                relativePath={props.relativePath}
                threadRef={props.threadRef}
                workspaceMutationId={props.workspaceMutationId}
                className="mx-auto max-w-4xl px-6"
                onTaskListChange={
                  onTaskListChange &&
                  ((input) =>
                    onTaskListChange({
                      markerOffset:
                        item.start +
                        input.markerOffset -
                        (props.definitions ? props.definitions.length + 2 : 0),
                      checked: input.checked,
                    }))
                }
              />
            )}
          </div>
        )}
      />
    </div>
  );
}
