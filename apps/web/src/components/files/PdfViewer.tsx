import { FilePasswordPrompt } from "./FilePasswordPrompt";
import { thrownErrorMessage } from "@onicode/client-runtime/errors";
import {
  useEffect,
  useEffectEvent,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent,
  type ReactNode,
} from "react";
import { PasswordResponses, TextLayer, type PDFDocumentProxy } from "pdfjs-dist";
import { msg, useGT } from "gt-react";
import { m } from "~/i18n/messages";
import { ChevronDownIcon, ChevronUpIcon, MinusIcon, PlusIcon, ScanIcon } from "lucide-react";
import { Input } from "~/components/ui/input";
import { observeResize } from "~/lib/observeResize";
import { FileSurfaceAction, FileSurfaceFailure, FileSurfaceLoading } from "./fileSurfaceChrome";
import { readBoundedDocument } from "./readBoundedDocument";
import { previewErrorText } from "./previewFailureMessages";
import { closePdf, openPdf, pdfClosed } from "./pdfDocument";
import {
  currentPdfPage,
  pdfFindIncludes,
  pdfFindMatchRuns,
  pdfPageOffsets,
  pdfPageScrollTop,
  pdfPageText,
  pdfPageWidth,
  pdfResizedScrollTop,
  pdfRestoredScrollTop,
  PDF_MAX_ZOOM,
  PDF_MIN_ZOOM,
  stepPdfZoom,
  visiblePdfPages,
} from "./pdfLayout";
import "./pdfViewer.css";
import { isImeKeyDown } from "../../lib/imeKeyDown";

const MAX_PAGES = 2000;
const MAX_FIND_TEXT = 4 * 1024 * 1024;

const MAX_POSITIONS = 50;
type PdfPosition = PdfViewState & { width: number; ratios: readonly number[] };
// Thread switches unmount the preview; retain only the most recent documents.
const positions = new Map<string, PdfPosition>();

/** An explicit destination and the index of the page it names. */
type PdfDestination = { page: number; dest: readonly unknown[] };
type PdfLink = { box: readonly [number, number, number, number] } & (
  | { url: string }
  | PdfDestination
);

// Destination kinds that name a top edge, and that edge's index after the page and kind.
const DESTINATION_TOP: Readonly<Record<string, number>> = { XYZ: 1, FitH: 0, FitBH: 0 };

/** Resolves a link annotation's destination, named or explicit, without loading its page. */
async function pdfLinkDestination(
  pdf: PDFDocumentProxy,
  dest: unknown,
): Promise<PdfDestination | null> {
  const explicit: unknown = typeof dest === "string" ? await pdf.getDestination(dest) : dest;
  if (!Array.isArray(explicit)) return null;
  const ref: unknown = explicit[0];
  const page = Number.isInteger(ref)
    ? (ref as number)
    : typeof ref === "object" && ref !== null
      ? await pdf.getPageIndex(ref as Parameters<PDFDocumentProxy["getPageIndex"]>[0])
      : null;
  if (page === null || page < 0 || page >= pdf.numPages) return null;
  return { page, dest: explicit };
}

/**
 * Loads a destination's page for its height / width and, when the destination names one, its
 * top edge's distance below the page top in page widths.
 */
async function pdfDestinationPoint(pdf: PDFDocumentProxy, { page, dest }: PdfDestination) {
  const viewport = (await pdf.getPage(page + 1)).getViewport({ scale: 1 });
  const ratio = viewport.height / viewport.width;
  const [, kind, ...args] = dest;
  const name = (kind as { name?: unknown } | undefined)?.name;
  const index = typeof name === "string" ? DESTINATION_TOP[name] : undefined;
  const y = index === undefined ? undefined : args[index];
  if (typeof y !== "number") return { ratio, top: null };
  const left = name === "XYZ" && typeof args[0] === "number" ? args[0] : 0;
  const [, top] = viewport.convertToViewportPoint(left, y);
  return { ratio, top: Math.max(0, top / viewport.width) };
}

export type PdfViewState = {
  page: number;
  offset: number;
  zoom: number | null;
  left: number;
};

function PdfPage(props: {
  document: PDFDocumentProxy;
  index: number;
  width: number;
  query: string;
  onSize: (index: number, ratio: number) => void;
  onError: (error: string) => void;
  onPageLink: (target: PdfDestination) => void;
  onOpenLink: ((url: string, event: MouseEvent) => void) | undefined;
}) {
  const gt = useGT();
  const [links, setLinks] = useState<readonly PdfLink[]>([]);
  const canvas = useRef<HTMLCanvasElement>(null);
  const text = useRef<HTMLDivElement>(null);
  const painted = useRef<PDFDocumentProxy>(null);
  // In PDF units: pdf.js sizes the text layer from raw page dimensions, without `/UserUnit`.
  const rawWidth = useRef(0);
  const { document: pdf, index, width, onSize, onError } = props;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Scale the pdf.js text layer with the CSS-stretched canvas until the debounced repaint.
  useLayoutEffect(() => {
    if (rawWidth.current > 0)
      text.current?.style.setProperty("--total-scale-factor", String(width / rawWidth.current));
  }, [width]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Render the PDF page canvas and text layer, cancelling tasks and releasing pdf.js page resources on cleanup.
  useEffect(() => {
    let cancelled = false;
    let paint: ReturnType<Awaited<ReturnType<PDFDocumentProxy["getPage"]>>["render"]> | undefined;
    let layer: TextLayer | undefined;
    let loadedPage: Awaited<ReturnType<PDFDocumentProxy["getPage"]>> | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    void (async () => {
      // A drag changes the width every frame. CSS scales the painted page until the width settles.
      if (painted.current === pdf)
        await new Promise((resolve) => (timer = setTimeout(resolve, 150)));
      const page = await pdf.getPage(index + 1);
      loadedPage = page;
      if (cancelled || !canvas.current || !text.current) {
        page.cleanup();
        return;
      }
      const natural = page.getViewport({ scale: 1 });
      rawWidth.current = natural.width / natural.userUnit;
      onSize(index, natural.height / natural.width);
      const viewport = page.getViewport({ scale: width / natural.width });
      const ratio = Math.min(
        2,
        window.devicePixelRatio || 1,
        Math.sqrt((8 * 1024 * 1024) / (viewport.width * viewport.height)),
      );
      // Resizing a canvas blanks it, so the page paints off screen and replaces the old one whole.
      const buffer = document.createElement("canvas");
      buffer.width = Math.ceil(viewport.width * ratio);
      buffer.height = Math.ceil(viewport.height * ratio);
      paint = page.render({
        canvas: buffer,
        viewport,
        transform: [ratio, 0, 0, ratio, 0, 0],
        background: "white",
      });
      text.current.replaceChildren();
      text.current.style.setProperty(
        "--total-scale-factor",
        String(viewport.scale * viewport.userUnit),
      );
      layer = new TextLayer({
        container: text.current,
        viewport,
        textContentSource: page.streamTextContent(),
      });
      const shown = paint.promise.then(() => {
        if (cancelled || !canvas.current) return;
        canvas.current.width = buffer.width;
        canvas.current.height = buffer.height;
        canvas.current.getContext("2d", { alpha: false })?.drawImage(buffer, 0, 0);
        buffer.width = 0;
        painted.current = pdf;
      });
      await Promise.all([shown, layer.render()]);
    })().catch((cause: unknown) => {
      if (!cancelled)
        onError(thrownErrorMessage(cause, m) ?? m(msg("This PDF page could not render.")));
    });
    return () => {
      cancelled = true;
      clearTimeout(timer);
      paint?.cancel();
      layer?.cancel();
      // Release operator lists when virtual pages leave the viewport.
      void Promise.allSettled([paint?.promise]).then(() => loadedPage?.cleanup());
    };
  }, [pdf, index, width, onSize, onError]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Read the page's link annotations once per page; their boxes are page-relative, so a zoom keeps them.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const page = await pdf.getPage(index + 1);
      const annotations = await page.getAnnotations({ intent: "display" });
      const viewport = page.getViewport({ scale: 1 });
      const found: PdfLink[] = [];
      for (const annotation of annotations) {
        if (cancelled) return;
        if (annotation.subtype !== "Link") continue;
        const [x1, y1] = viewport.convertToViewportPoint(annotation.rect[0], annotation.rect[1]);
        const [x2, y2] = viewport.convertToViewportPoint(annotation.rect[2], annotation.rect[3]);
        const box = [
          Math.min(x1, x2) / viewport.width,
          Math.min(y1, y2) / viewport.height,
          Math.abs(x2 - x1) / viewport.width,
          Math.abs(y2 - y1) / viewport.height,
        ] as const;
        if (typeof annotation.url === "string" && /^https?:\/\//i.test(annotation.url)) {
          found.push({ box, url: annotation.url });
          continue;
        }
        const target = await pdfLinkDestination(pdf, annotation.dest).catch(() => null);
        if (target !== null) found.push({ box, ...target });
      }
      if (!cancelled) setLinks(found);
    })()
      // A page whose annotations cannot be read still renders, without links.
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [pdf, index]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Highlight pdf.js-owned text nodes and observe asynchronous text layer mutations for the current search query.
  useEffect(() => {
    const node = text.current;
    if (!node) return;
    const mark = () => {
      const elements = Array.from(node.querySelectorAll('span[role="presentation"], br'));
      const hits = pdfFindMatchRuns(
        elements.map((element) => (element.tagName === "BR" ? "\n" : (element.textContent ?? ""))),
        props.query,
      );
      elements.forEach((element, index) => element.classList.toggle("pdf-find-match", hits[index]));
    };
    mark();
    const observer = new MutationObserver(mark);
    observer.observe(node, { childList: true, subtree: true });
    return () => observer.disconnect();
  }, [props.query]);
  return (
    <>
      <canvas
        ref={canvas}
        aria-label={gt("Page {number}", { number: String(index + 1) })}
        className="block h-full w-full"
      />
      <div ref={text} data-pdf-text-layer />
      {links.length ? (
        <div className="pointer-events-none absolute inset-0">
          {links.map((link) => {
            const [left, top, width, height] = link.box;
            const key = `${link.box.join()}:${"url" in link ? link.url : link.page}`;
            const style = {
              left: `${left * 100}%`,
              top: `${top * 100}%`,
              width: `${width * 100}%`,
              height: `${height * 100}%`,
            };
            return "url" in link ? (
              <a
                key={key}
                href={link.url}
                target="_blank"
                rel="noopener noreferrer"
                aria-label={link.url}
                className="pointer-events-auto absolute"
                style={style}
                onClick={(event) => {
                  if (!props.onOpenLink) return;
                  event.preventDefault();
                  props.onOpenLink(link.url, event);
                }}
                // Electron's window-open handler would bypass "Open links in" on middle-click.
                onAuxClick={(event) => {
                  if (!props.onOpenLink || event.button !== 1) return;
                  event.preventDefault();
                  props.onOpenLink(link.url, event);
                }}
              />
            ) : (
              <a
                key={key}
                href={`#page=${link.page + 1}`}
                aria-label={gt("Page {number}", { number: String(link.page + 1) })}
                className="pointer-events-auto absolute"
                style={style}
                onClick={(event) => {
                  event.preventDefault();
                  props.onPageLink(link);
                }}
              />
            );
          })}
        </div>
      ) : null}
    </>
  );
}

export default function PdfViewer(props: {
  src: string;
  title: string;
  positionKey?: string;
  initialView?: PdfViewState;
  onViewChange?: (view: PdfViewState) => void;
  onOpenLink?: (url: string, event: MouseEvent) => void;
  actions?: ReactNode;
}) {
  const gt = useGT();
  const [savedPosition] = useState(() =>
    props.positionKey === undefined ? undefined : positions.get(props.positionKey),
  );
  const [initialView] = useState<PdfViewState>(
    () => savedPosition ?? props.initialView ?? { page: 0, offset: 0, zoom: null, left: 0 },
  );
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [incorrectPassword, setIncorrectPassword] = useState<boolean | null>(null);
  const submitPassword = useRef<((password: string) => void) | null>(null);
  const [revision, setRevision] = useState(0);
  const [ratios, setRatios] = useState<readonly number[]>([]);
  const [zoom, setZoom] = useState(initialView.zoom);
  const [pageWidth, setPageWidth] = useState(612);
  const [viewport, setViewport] = useState({ width: 600, height: 800 });
  const [scrolled, setScrolled] = useState({ top: 0, width: 0, pdf });
  const [query, setQuery] = useState("");
  const [matches, setMatches] = useState<number[]>([]);
  const [finding, setFinding] = useState(false);
  const [findError, setFindError] = useState<string | null>(null);
  const [match, setMatch] = useState(0);
  const scroll = useRef<HTMLDivElement>(null);
  const find = useRef<HTMLInputElement>(null);
  const zoomAnchor = useRef<{ page: number; offset: number; width: number } | null>(null);
  // Bumped by each link, navigation and document change; a page load answers only the latest.
  const linkRequest = useRef(0);
  // Held until the reader navigates, so pages measured above the target cannot push it away.
  // `at` is the scroll top last applied, so only a target that moved or came in reach scrolls.
  const linkJump = useRef<{ page: number; top: number | null; at: number | null } | null>(null);
  const releaseLinkJump = () => {
    linkJump.current = null;
    linkRequest.current += 1;
  };
  const initialPage = initialView.page;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Own the PDF fetch, load timeout, and pdf.js document task across source changes, retries, and unmounts.
  useEffect(() => {
    const controller = new AbortController();
    let disposed = false;
    let task: ReturnType<typeof openPdf> | undefined;
    const startTimeout = () =>
      setTimeout(() => {
        setError(m(msg("This PDF took too long to load. Try again.")));
        controller.abort();
        closePdf(task);
      }, 30_000);
    let timeout = startTimeout();
    void readBoundedDocument(
      props.src,
      controller.signal,
      50 * 1024 * 1024,
      revision > 0 ? "reload" : "default",
    )
      .then(async (bytes) => {
        await pdfClosed();
        controller.signal.throwIfAborted();
        task = openPdf(bytes);
        task.onPassword = (updatePassword: (password: string) => void, reason: number) => {
          if (controller.signal.aborted) return;
          clearTimeout(timeout);
          submitPassword.current = (password) => {
            submitPassword.current = null;
            timeout = startTimeout();
            updatePassword(password);
          };
          setIncorrectPassword(reason === PasswordResponses.INCORRECT_PASSWORD);
        };
        const document = await task.promise;
        if (document.numPages > MAX_PAGES)
          throw new Error(
            m(msg("Preview limited to 2,000 pages. Save this PDF to read it in full.")),
          );
        const first = (await document.getPage(1)).getViewport({ scale: 1 });
        const sizes = await Promise.all(
          Array.from({ length: Math.min(initialPage + 1, document.numPages) }, async (_, index) => {
            const size = (await document.getPage(index + 1)).getViewport({ scale: 1 });
            return size.height / size.width;
          }),
        );
        if (controller.signal.aborted) return;
        setPageWidth(first.width);
        setRatios(
          savedPosition?.ratios.length === document.numPages
            ? savedPosition.ratios
            : Array.from(
                { length: document.numPages },
                (_, index) => sizes[index] ?? first.height / first.width,
              ),
        );
        setPdf(document);
      })
      .catch((cause: unknown) => {
        if (!disposed && !controller.signal.aborted)
          setError(previewErrorText(cause, m(msg("Unable to open PDF."))));
      })
      .finally(() => clearTimeout(timeout));
    return () => {
      disposed = true;
      linkJump.current = null;
      linkRequest.current += 1;
      submitPassword.current = null;
      controller.abort();
      clearTimeout(timeout);
      closePdf(task);
    };
  }, [props.src, revision, initialPage, savedPosition]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Measure the PDF viewport before paint and observe resizes for fit-width sizing and visible-page selection.
  useLayoutEffect(() => {
    const node = scroll.current;
    if (!node) return;
    const update = () => setViewport({ width: node.clientWidth, height: node.clientHeight });
    update();
    return observeResize(node, update);
  }, []);
  const { scale, width } = pdfPageWidth(viewport.width, pageWidth, zoom);
  const offsets = pdfPageOffsets(ratios, width);
  // A new page width moves every page, so the place in view has a new offset before the DOM scrolls.
  const resized = pdf !== null && scrolled.pdf === pdf && scrolled.width !== width;
  const scrollTop = resized ? pdfResizedScrollTop(ratios, scrolled, width) : scrolled.top;
  const current = currentPdfPage(offsets, ratios.length, scrollTop);
  const visible = visiblePdfPages(ratios, offsets, scrollTop, viewport.height);
  // Stable callbacks let page canvases survive scroll and find updates.
  const [onSize] = useState(
    () => (index: number, ratio: number) =>
      setRatios((old) =>
        old[index] === ratio ? old : old.map((value, i) => (i === index ? ratio : value)),
      ),
  );
  const jumpToPage = useEffectEvent((index: number) => {
    releaseLinkJump();
    scroll.current?.scrollTo({ top: pdfPageScrollTop(offsets, index) });
  });
  const saveView = (node: HTMLDivElement, notify = true) => {
    const page = currentPdfPage(offsets, ratios.length, node.scrollTop);
    if (!pdf) return;
    const view = {
      page,
      offset: Math.max(0, node.scrollTop - (offsets[page]! - 16)),
      zoom,
      left: node.scrollLeft,
    };
    if (notify) props.onViewChange?.(view);
    if (props.positionKey !== undefined) {
      positions.delete(props.positionKey);
      positions.set(props.positionKey, { ...view, width, ratios });
      const oldest = positions.keys().next();
      if (positions.size > MAX_POSITIONS && !oldest.done) positions.delete(oldest.value);
    }
  };
  const restoreView = useEffectEvent(() => {
    const node = scroll.current;
    if (!pdf || !node) return;
    node.scrollTo({
      top: pdfRestoredScrollTop(ratios, width, initialView, savedPosition?.width),
      left: initialView.left,
    });
    setScrolled({ top: node.scrollTop, width, pdf });
    saveView(node);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Restore the saved PDF scroll position after the loaded document's page layout commits.
  useLayoutEffect(() => {
    if (pdf) restoreView();
  }, [pdf]);
  const showPage = (index: number) => {
    releaseLinkJump();
    scroll.current?.scrollTo({ top: pdfPageScrollTop(offsets, index) });
  };
  // The target may be an unmeasured page, so its size enters the layout before the scroll.
  const followLink = (link: PdfDestination) => {
    if (!pdf) return;
    const request = ++linkRequest.current;
    void pdfDestinationPoint(pdf, link)
      .catch(() => null)
      .then((point) => {
        if (request !== linkRequest.current) return;
        linkJump.current = { page: link.page, top: point?.top ?? null, at: null };
        // Always a new array, so the layout effect runs even for an already measured page.
        setRatios((old) =>
          old.map((value, index) => (index === link.page ? (point?.ratio ?? value) : value)),
        );
      });
  };
  const changeZoom = (value: number | null) => {
    releaseLinkJump();
    zoomAnchor.current = {
      page: current,
      offset: (scroll.current?.scrollTop ?? 0) - (offsets[current]! - 16),
      width,
    };
    setZoom(value);
  };
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Restore the place in view after a zoom or resize, or follow a link, once the new width or page size has committed to page layout.
  useLayoutEffect(() => {
    const anchor = zoomAnchor.current;
    const jump = linkJump.current;
    zoomAnchor.current = null;
    const node = scroll.current;
    if (!node || !pdf) return;
    if (anchor && anchor.width !== width) {
      node.scrollTo({
        top: offsets[anchor.page]! - 16 + (anchor.offset * width) / anchor.width,
      });
    } else if (resized && !jump) node.scrollTo({ top: scrollTop });
    if (jump) {
      // `top` is a point within the page, in page widths below its top edge.
      const target =
        jump.top === null
          ? pdfPageScrollTop(offsets, jump.page)
          : offsets[jump.page]! + jump.top * width;
      const at = Math.max(0, Math.min(target, node.scrollHeight - node.clientHeight));
      if (at !== jump.at) {
        jump.at = at;
        node.scrollTo({ top: at });
      }
    }
    // A scroll that moves nothing fires no `scroll` event, so record the handled width here.
    if (resized) setScrolled({ top: node.scrollTop, width, pdf });
    saveView(node, anchor !== null);
  });
  const firstMatch = matches[0];
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Scroll to the first asynchronous search result after its measured page offsets have committed.
  useLayoutEffect(() => {
    if (firstMatch !== undefined) jumpToPage(firstMatch);
  }, [firstMatch]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Debounce pdf.js text extraction and cancel obsolete searches when the query or document changes.
  useEffect(() => {
    if (!pdf || !query.trim()) {
      // oxlint-disable-next-line react/set-state-in-effect -- A blank query cancels the previous search.
      setFinding(false);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      setFinding(true);
      setFindError(null);
      setMatches([]);
      setMatch(0);
      void (async () => {
        const found: number[] = [];
        const sizes: number[] = [];
        let size = 0;
        for (let index = 0; index < pdf.numPages; index++) {
          const page = await pdf.getPage(index + 1);
          const pageSize = page.getViewport({ scale: 1 });
          sizes.push(pageSize.height / pageSize.width);
          const content = await page.getTextContent();
          if (cancelled) return;
          const text = pdfPageText(content.items);
          size += text.length;
          if (size > MAX_FIND_TEXT)
            throw new Error(
              m(msg("This PDF has too much text to search. Save it to search in another app.")),
            );
          if (!pdfFindIncludes(text, query)) continue;
          found.push(index);
          if (found.length > 1) continue;
          // Pages up to the first hit are measured, so its jump lands before the scan finishes.
          setRatios((old) => [...sizes, ...old.slice(sizes.length)]);
          setMatches([index]);
        }
        if (cancelled) return;
        setRatios(sizes);
        setMatches(found);
      })()
        .catch((cause: unknown) => {
          if (!cancelled)
            setFindError(thrownErrorMessage(cause, m) ?? m(msg("Could not search this PDF.")));
        })
        .finally(() => {
          if (!cancelled) setFinding(false);
        });
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [pdf, query]);
  return (
    <div
      className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
      onWheel={releaseLinkJump}
      onPointerDown={releaseLinkJump}
      onKeyDown={releaseLinkJump}
    >
      <div className="flex min-h-10 shrink-0 flex-wrap items-center gap-1 border-b border-border px-2 py-1 text-xs">
        <span className="mr-auto px-1 tabular-nums" aria-live="polite">
          {pdf
            ? gt(
                "{count, plural, one {{current} / {count} page} other {{current} / {count} pages}}",
                {
                  count: pdf.numPages,
                  current: String(current + 1),
                },
              )
            : "PDF"}
        </span>
        <FileSurfaceAction
          label={gt("Previous page")}
          disabled={!pdf || current === 0}
          onPress={() => showPage(current - 1)}
        >
          <ChevronUpIcon className="size-3.5" />
        </FileSurfaceAction>
        <FileSurfaceAction
          label={gt("Next page")}
          disabled={!pdf || current + 1 >= pdf.numPages}
          onPress={() => showPage(current + 1)}
        >
          <ChevronDownIcon className="size-3.5" />
        </FileSurfaceAction>
        <FileSurfaceAction
          label={gt("Zoom out")}
          disabled={scale <= PDF_MIN_ZOOM}
          onPress={() => changeZoom(stepPdfZoom(scale, -1))}
        >
          <MinusIcon className="size-3.5" />
        </FileSurfaceAction>
        <span className="min-w-14 text-center tabular-nums">
          {zoom === null ? gt("Fit width") : `${Math.round(scale * 100)}%`}
        </span>
        <FileSurfaceAction
          label={gt("Zoom in")}
          disabled={scale >= PDF_MAX_ZOOM}
          onPress={() => changeZoom(stepPdfZoom(scale, 1))}
        >
          <PlusIcon className="size-3.5" />
        </FileSurfaceAction>
        <FileSurfaceAction
          label={gt("Fit width")}
          onPress={() => {
            changeZoom(null);
            scroll.current?.scrollTo({ left: 0 });
          }}
        >
          <ScanIcon className="size-3.5" />
        </FileSurfaceAction>
        <div className="w-28">
          {/* Native search inputs clear on Escape even during IME composition. */}
          <Input
            ref={find}
            size="compact"
            type="text"
            role="searchbox"
            aria-label={gt("Find in PDF")}
            placeholder={gt("Find in PDF")}
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setMatches([]);
              setFindError(null);
            }}
            onKeyDown={(event) => {
              if (event.key !== "Escape" || isImeKeyDown(event.nativeEvent)) return;
              if (!query && !event.repeat) return;
              event.stopPropagation();
              setQuery("");
            }}
          />
        </div>
        {query ? (
          <>
            <span role="status">
              {finding
                ? gt("Searching…")
                : (findError ?? `${matches.length ? match + 1 : 0}/${matches.length}`)}
            </span>
            <FileSurfaceAction
              label={gt("Next matching page")}
              disabled={!matches.length}
              onPress={() => {
                const next = (match + 1) % matches.length;
                setMatch(next);
                showPage(matches[next]!);
              }}
            >
              <ChevronDownIcon className="size-3.5" />
            </FileSurfaceAction>
          </>
        ) : null}
        {props.actions}
      </div>
      <div
        ref={scroll}
        role="region"
        aria-label={props.title}
        tabIndex={0}
        className="relative min-h-0 min-w-0 w-full flex-1 overflow-auto scrollbar-gutter-stable bg-muted"
        onScroll={(event) => {
          const top = event.currentTarget.scrollTop;
          setScrolled((old) =>
            old.top === top && old.width === width && old.pdf === pdf ? old : { top, width, pdf },
          );
          saveView(event.currentTarget);
        }}
        onKeyDown={(event) => {
          if ((event.ctrlKey || event.metaKey) && event.key === "f") {
            event.preventDefault();
            event.stopPropagation();
            find.current?.focus();
          }
        }}
      >
        {error || incorrectPassword !== null || !pdf ? (
          // The scroll region is a block, so a centered state needs a column that fills it.
          <div className="flex min-h-full flex-col">
            {error ? (
              <FileSurfaceFailure
                message={error}
                onRetry={() => {
                  setError(null);
                  setPdf(null);
                  setRevision((value) => value + 1);
                }}
              />
            ) : incorrectPassword !== null ? (
              <FilePasswordPrompt
                incorrect={incorrectPassword}
                onSubmit={(password) => {
                  setIncorrectPassword(null);
                  submitPassword.current?.(password);
                }}
              />
            ) : (
              <FileSurfaceLoading />
            )}
          </div>
        ) : (
          <div
            className="relative"
            style={{
              height: `max(${offsets.at(-1) ?? 0}px, calc(100% + ${(offsets.at(-2) ?? 16) - 16}px))`,
              minWidth: width + 32,
            }}
          >
            {visible.map(({ index, ratio }) => (
              <div
                key={index}
                className="absolute bg-white shadow-sm"
                style={
                  {
                    top: offsets[index],
                    left: Math.max(16, (viewport.width - width) / 2),
                    width,
                    height: width * ratio,
                    "--total-scale-factor": 1,
                  } as CSSProperties
                }
              >
                <PdfPage
                  document={pdf}
                  index={index}
                  width={width}
                  query={query}
                  onSize={onSize}
                  onError={setError}
                  onPageLink={followLink}
                  onOpenLink={props.onOpenLink}
                />
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
