/* oxlint-disable react/no-array-index-key -- Positions identify source rows and columns. */
import { useGT } from "gt-react";
import { useEffect, useEffectEvent, useLayoutEffect, useRef, useState } from "react";
import { useClientSettings } from "~/hooks/useSettings";
import { observeResize } from "~/lib/observeResize";
import { Tooltip, TooltipPopup, TooltipTrigger } from "~/components/ui/tooltip";
import { PREVIEW_PAGE_ROWS } from "./largeTextProtocol";

export default function VirtualTablePreview(props: {
  readonly name: string;
  readonly count: number;
  readonly columns: number;
  readonly getRows: (page: number) => readonly (readonly string[])[] | undefined;
  readonly loadPage?: (page: number) => void;
  readonly header?: readonly string[] | undefined;
  readonly source?: boolean;
  readonly wrap?: boolean;
  readonly revealLine?: number | null;
  readonly revealRequestId?: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [left, setLeft] = useState(0);
  const [size, setSize] = useState({ width: 0, height: 600 });
  const { width } = size;
  const widthRef = useRef(0);
  const measureRef = useRef<HTMLTableElement>(null);
  const [measured, setMeasured] = useState<number[] | null>(null);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Measure the viewport before paint and observe container resizes to invalidate column measurements.
  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return;
    const update = () => {
      const width = node.clientWidth;
      const height = node.clientHeight;
      setSize((old) => (old.width === width && old.height === height ? old : { width, height }));
      if (widthRef.current !== width) {
        widthRef.current = width;
        setMeasured(null);
      }
    };
    update();
    return observeResize(node, update);
  }, []);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Read the browser's laid-out table column widths before displaying the virtual table.
  useLayoutEffect(() => {
    if (!width || !props.header || !measureRef.current || measured) return;
    const widths = [...measureRef.current.querySelectorAll("thead th")].map(
      (cell) => cell.getBoundingClientRect().width,
    );
    if (widths.length) setMeasured(widths);
  }, [measured, width, props.header]);
  const columnWidth = props.source
    ? 0
    : Math.max(128, Math.min(320, (width - 56) / Math.min(props.columns, 6)));
  const rowHeader = 56;
  const sizes = Array.from(
    { length: props.columns },
    (_, column) => measured?.[column] ?? columnWidth,
  );
  const offsets = [0];
  for (const size of sizes) offsets.push(offsets[offsets.length - 1]! + size);
  const firstColumn = props.source
    ? 0
    : Math.max(0, sizes.findIndex((_, column) => offsets[column + 1]! > left - rowHeader) - 1);
  const lastColumn = props.source
    ? 1
    : Math.min(
        props.columns,
        offsets.findIndex((offset) => offset >= left + width - rowHeader) + 1 || props.columns,
      );
  const columns = Array.from(
    { length: Math.max(0, lastColumn - firstColumn) },
    (_, i) => firstColumn + i,
  );
  const offset = (column: number) => offsets[column]!;
  const tableWidth = props.source ? "100%" : Math.max(width, offset(props.columns) + rowHeader);
  const hasHorizontalOverflow = !props.source && offset(props.columns) + rowHeader > width;
  return (
    <div
      ref={ref}
      className="relative min-h-0 flex-1 overflow-x-auto contain-strict"
      onScroll={(event) => {
        event.currentTarget.style.setProperty(
          "--preview-scroll-left",
          `${event.currentTarget.scrollLeft}px`,
        );
        setLeft(event.currentTarget.scrollLeft);
      }}
    >
      {props.header && props.columns <= 30 && !measured && width > 0 ? (
        <table
          ref={measureRef}
          aria-hidden="true"
          className="invisible absolute top-0 border-separate border-spacing-0 text-xs"
          style={{ width: Math.max(width - rowHeader, props.columns * 128) }}
        >
          <thead>
            <tr>
              {props.header.map((cell, i) => (
                <th
                  key={i}
                  className="min-w-32 max-w-80 border-b px-3 py-1.5 text-left font-medium whitespace-nowrap"
                >
                  {cell}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {props
              .getRows(0)
              ?.slice(1, 20)
              .map((row, i) => (
                <tr key={i}>
                  {row.map((cell, j) => (
                    <td
                      key={j}
                      className="max-w-80 border-b px-3 py-1.5 whitespace-pre-wrap break-words tabular-nums"
                    >
                      {cell}
                    </td>
                  ))}
                </tr>
              ))}
          </tbody>
        </table>
      ) : null}
      {width > 0 && (!props.header || props.columns > 30 || measured) ? (
        <div
          role={props.source ? "region" : "table"}
          aria-label={props.name}
          aria-rowcount={props.count}
          aria-colcount={props.source ? undefined : props.columns}
          className="flex h-full min-h-0 flex-col text-xs"
          style={{ width: tableWidth }}
        >
          {!props.source ? (
            <div
              role="row"
              className="relative z-10 flex min-h-[29px] shrink-0 border-b border-border bg-muted/60 font-medium backdrop-blur"
            >
              <div className="sticky left-0 z-10 w-14 shrink-0 border-r border-border bg-background" />
              <div style={{ width: offset(firstColumn), flexShrink: 0 }} />
              {columns.map((column) => (
                <Tooltip key={column}>
                  <TooltipTrigger
                    render={
                      <div
                        role="columnheader"
                        aria-colindex={column + 1}
                        className="shrink-0 overflow-hidden border-r border-border px-3 py-1.5 text-ellipsis whitespace-pre"
                        style={{ width: sizes[column] }}
                      >
                        {props.header?.[column] ?? columnLabel(column)}
                      </div>
                    }
                  />
                  <TooltipPopup>{props.header?.[column] ?? columnLabel(column)}</TooltipPopup>
                </Tooltip>
              ))}
            </div>
          ) : null}
          <ScaledRows
            {...props}
            columnsVisible={columns}
            sizes={sizes}
            hasHorizontalOverflow={hasHorizontalOverflow}
            rowHeader={rowHeader}
            firstColumn={firstColumn}
            width={width}
            viewportHeight={size.height - (props.source ? 0 : 29)}
          />
        </div>
      ) : null}
    </div>
  );
}
function RowPage(
  props: Parameters<typeof VirtualTablePreview>[0] & {
    page: number;
    columnsVisible: number[];
    sizes: number[];
    rowHeights?: ReadonlyMap<number, number> | undefined;
    rowHeader: number;
    firstColumn: number;
    viewportHeight: number;
    from: number;
    limit?: number;
    ready: (index: number, row: readonly string[]) => boolean;
    more: (count: number) => string;
  },
) {
  const rows = props.getRows(props.page);
  // A page stays mounted while the worker fills it, preserving the scroll position.
  const { loadPage, page } = props;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Request this page's rows from the indexing worker when they are missing or incomplete.
  useEffect(() => {
    const expected = Math.min(PREVIEW_PAGE_ROWS, props.count - page * PREVIEW_PAGE_ROWS);
    if (!rows || rows.length < expected) loadPage?.(page);
  }, [rows, page, loadPage, props.count]);
  const start = props.page * PREVIEW_PAGE_ROWS;
  const count = Math.min(PREVIEW_PAGE_ROWS, props.count - start, props.limit ?? PREVIEW_PAGE_ROWS);
  return (
    <div role="rowgroup">
      {Array.from({ length: count - props.from }, (_, offset) => {
        const local = props.from + offset;
        const row = rows?.[local];
        const index = start + local;
        const rowMinHeight = !props.source && row ? props.rowHeights?.get(index) : undefined;
        // A long row shows its text once the probe has measured it, so rows past the viewport never lay it out.
        const ready = row ? props.ready(index, row) : false;
        return (
          <div
            key={local}
            data-preview-row={index}
            role={props.source ? undefined : "row"}
            aria-rowindex={props.source ? undefined : index + 1}
            className={
              props.source
                ? `flex min-h-[29px] font-mono ${props.revealLine != null && Math.min(props.count, Math.max(1, props.revealLine)) === index + 1 ? "bg-primary/10" : ""}`
                : `flex min-h-[29px] border-b border-border ${(index - (props.header ? 1 : 0)) % 2 ? "bg-muted" : ""}`
            }
            style={rowMinHeight ? { minHeight: rowMinHeight } : undefined}
            data-preview-reveal={
              props.revealLine != null &&
              Math.min(props.count, Math.max(1, props.revealLine)) === index + 1
                ? "true"
                : undefined
            }
          >
            <div
              role={props.source ? undefined : "rowheader"}
              className={`${props.source ? "sticky left-0" : "relative"} z-1 shrink-0 border-r border-border bg-background px-2 py-1.5 text-right font-normal text-muted-foreground tabular-nums select-none`}
              style={{
                width: props.rowHeader,
                transform: props.source ? undefined : "translateX(var(--preview-scroll-left, 0px))",
              }}
            >
              <span className="sticky top-0 block">{index + (props.header ? 0 : 1)}</span>
            </div>
            {!props.source ? (
              <div
                style={{
                  width: props.sizes.slice(0, props.firstColumn).reduce((a, b) => a + b, 0),
                  flexShrink: 0,
                }}
              />
            ) : null}
            {props.columnsVisible.map((column) => (
              <div
                key={column}
                role={props.source ? undefined : "cell"}
                aria-colindex={props.source ? undefined : column + 1}
                className={
                  props.source
                    ? `min-w-0 flex-1 px-3 py-1 ${props.wrap ? "whitespace-pre-wrap break-words" : "whitespace-pre"}`
                    : "shrink-0 border-r border-border px-3 py-1.5 whitespace-pre-wrap break-words tabular-nums"
                }
                style={props.source ? undefined : { width: props.sizes[column] }}
              >
                {ready ? (
                  <CellText
                    text={row?.[column] ?? ""}
                    limit={props.source ? MAX_SHOWN_LINE_CHARS : MAX_SHOWN_CELL_CHARS}
                    more={props.more}
                  />
                ) : (
                  ""
                )}
              </div>
            ))}
          </div>
        );
      })}
    </div>
  );
}

function CellText(props: { text: string; limit: number; more: (count: number) => string }) {
  const end = shownLength(props.text, props.limit);
  if (end === props.text.length) return props.text;
  return (
    <>
      {props.text.slice(0, end)}
      <span className="text-muted-foreground">
        {cutSuffix(props.text.length - end, props.more)}
      </span>
    </>
  );
}

function ScaledRows(
  props: Omit<Parameters<typeof RowPage>[0], "page" | "from" | "rowHeights" | "ready" | "more"> & {
    hasHorizontalOverflow: boolean;
    width: number;
  },
) {
  const gt = useGT();
  const more = (count: number) =>
    gt("{count, plural, one {{formatted} more character} other {{formatted} more characters}}", {
      count,
      formatted: count.toLocaleString(),
    });
  const ref = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const rowProbeRef = useRef<HTMLDivElement>(null);
  const rowHeightCacheRef = useRef<{
    key: string;
    entries: Map<number, CachedRowHeight>;
  }>({ key: "", entries: new Map() });
  // Heights of measured multi-line rows, and the probe's metrics for telling single-line rows apart.
  const [rowHeights, setRowHeights] = useState<{
    key: string;
    heights: ReadonlyMap<number, number>;
    fontSize: number;
    sizes: readonly number[];
  }>({ key: "", heights: new Map(), fontSize: 0, sizes: [] });
  const [, setRowMeasureTick] = useState(0);
  const syncMeasurePassesRef = useRef(0);
  // Whether this commit laid out the final row heights; End waits for them before it settles.
  const rowHeightsSettledRef = useRef(true);
  const typographyKey = useClientSettings(
    (settings) => `${settings.fontFamilySans}:${settings.fontSizeInterface}`,
  );
  const [layout, setLayout] = useState({ height: 0, rowHeight: 29 });
  const [top, setTop] = useState(0);
  const handledReveal = useRef<string | null>(null);
  const pendingReveal = useRef<{ row: number; attempts: number } | null>(null);
  const pendingEnd = useRef(false);
  const position = { top, height: props.viewportHeight };
  const scrollHeight = Math.min(8_000_000, props.count * 29);
  // Chromium caps a scroll surface at 33 million pixels. Scale the track for
  // multi-million-line logs while retaining real row heights in the viewport.
  const headerRows = props.header ? 1 : 0;
  const maxFirst = Math.max(0, props.count - headerRows - Math.floor(position.height / 29));
  // The scaled track ends 2px early so a bottom that zoom or float error leaves short still
  // reaches it and pins the native tail.
  const scale = Math.max(1, scrollHeight - position.height - 2) / Math.max(1, maxFirst * 29);
  const logicalTop = position.top / scale / 29;
  const first = Math.min(maxFirst, Math.floor(logicalTop)) + headerRows;
  const rowFraction = logicalTop - (first - headerRows);
  // The final mounted rows keep their measured height and scroll natively.
  const nativeTail = first - headerRows === maxFirst;
  const scrollModel = useRef({ scale, maxFirst, heights: [] as number[] });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Measure native tail rows and preserve the DOM scroll position when the virtual track scale changes.
  useLayoutEffect(() => {
    const previous = scrollModel.current;
    const heights = nativeTail
      ? Array.from(
          contentRef.current?.querySelectorAll<HTMLElement>("[data-preview-row]") ?? [],
          (row) => row.offsetHeight,
        )
      : [];
    scrollModel.current = { scale, maxFirst, heights };
    const node = ref.current;
    if (!node || (previous.scale === scale && previous.maxFirst === maxFirst)) return;
    const previousEnd = previous.maxFirst * 29 * previous.scale;
    let logicalTop = Math.min(previous.maxFirst * 29, node.scrollTop / previous.scale);
    let remaining = Math.max(0, node.scrollTop - previousEnd);
    // Native-tail pixels cross rows of different heights before returning to the scaled track.
    for (const height of previous.heights) {
      logicalTop += Math.min(1, remaining / height) * 29;
      remaining = Math.max(0, remaining - height);
      if (!remaining) break;
    }
    let nativeOffset = 0;
    remaining = Math.max(0, logicalTop / 29 - maxFirst);
    for (const height of heights) {
      nativeOffset += Math.min(1, remaining) * height;
      remaining = Math.max(0, remaining - 1);
      if (!remaining) break;
    }
    node.scrollTop = Math.min(maxFirst * 29, logicalTop) * scale + nativeOffset;
    setTop(node.scrollTop);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Scroll the DOM viewport to a requested line after its virtual track has committed.
  useLayoutEffect(() => {
    const node = ref.current;
    if (props.revealLine == null) {
      handledReveal.current = null;
      pendingReveal.current = null;
      return;
    }
    const key = `${props.revealRequestId}:${props.revealLine}`;
    if (!node || !props.count || handledReveal.current === key) return;
    const row = Math.min(props.count, Math.max(1, props.revealLine)) - 1;
    node.scrollTop = Math.max(0, (row - headerRows) * 29) * scale;
    setTop(node.scrollTop);
    pendingEnd.current = false;
    pendingReveal.current = { row, attempts: 0 };
    handledReveal.current = key;
  }, [props.revealLine, props.revealRequestId, props.count, headerRows, scale]);
  const atStart = position.top === 0;
  const rowHeight = () =>
    contentRef.current?.querySelector<HTMLElement>(`[data-preview-row="${first}"]`)?.offsetHeight ||
    29;
  const scrollByContent = (node: HTMLElement, delta: number) => {
    if (!nativeTail) {
      node.scrollTop += delta * ((scale * 29) / rowHeight());
      return;
    }
    // Above the tail's first row the track is scaled again, at 29px a row.
    const native = Math.max(delta, maxFirst * 29 * scale - node.scrollTop);
    node.scrollTop += native + (delta - native) * scale;
  };
  const onWheel = useEffectEvent((event: WheelEvent) => {
    const node = ref.current;
    if (!node || event.ctrlKey || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
    const delta =
      event.deltaY * (event.deltaMode === 1 ? 29 : event.deltaMode === 2 ? node.clientHeight : 1);
    if (!delta) return;
    event.preventDefault();
    pendingEnd.current = false;
    scrollByContent(node, delta);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Install a non-passive native wheel listener so virtual scrolling can prevent the browser's default scroll.
  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    const wheel = (event: WheelEvent) => onWheel(event);
    node.addEventListener("wheel", wheel, { passive: false });
    return () => {
      node.removeEventListener("wheel", wheel);
    };
  }, []);
  const page = Math.floor(first / PREVIEW_PAGE_ROWS);
  // Rows mount from `first`: rows above it never show, and measuring their full text is costly.
  const from = first % PREVIEW_PAGE_ROWS;
  // Unwrapped source lines are always one row tall; other rows are measured by the probe.
  const measures = !props.source || props.wrap === true;
  const sizesKey = props.source
    ? `source:${props.width}|${typographyKey}`
    : `${props.sizes.join(",")}|${typographyKey}`;
  const knownHeights =
    measures && rowHeights.key.startsWith(`${sizesKey}|`) ? rowHeights.heights : undefined;
  const singleLine = (row: readonly string[]) =>
    rowHeights.fontSize > 0 &&
    isSingleLineRow(
      row,
      props.source ? 1 : props.columns,
      props.source ? rowHeights.sizes : props.sizes,
      rowHeights.fontSize,
    );
  const ready = (index: number, row: readonly string[]) =>
    !measures || singleLine(row) || knownHeights?.has(index) === true;
  let visibleRows = Math.ceil(position.height / 29) + 1;
  // Rows past the measured heights that fill the viewport cannot show, so they are neither mounted
  // nor measured. The native tail mounts every row so its scroll extent reaches the last one.
  if (!nativeTail && knownHeights) {
    const target = position.height + rowFraction * (knownHeights.get(first) ?? BASE_ROW_HEIGHT);
    let rows = 0;
    for (let covered = 0; rows < visibleRows && covered < target; rows++) {
      covered += knownHeights.get(first + rows) ?? BASE_ROW_HEIGHT;
    }
    visibleRows = Math.min(visibleRows, rows + 1);
  }
  const visibleEnd = from + visibleRows;
  const pageCount = Math.min(
    Math.ceil(visibleEnd / PREVIEW_PAGE_ROWS),
    Math.ceil(props.count / PREVIEW_PAGE_ROWS) - page,
  );
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Measure mounted rows across all columns before paint so horizontal column virtualization keeps row heights and vertical scroll position stable.
  useLayoutEffect(() => {
    const probe = rowProbeRef.current;
    if (!measures || !probe) {
      syncMeasurePassesRef.current = 0;
      rowHeightsSettledRef.current = true;
      return;
    }
    const probeStyle = getComputedStyle(probe);
    const columns = props.source ? 1 : props.columns;
    const sizes = props.source ? [Math.max(1, probe.clientWidth - props.rowHeader)] : props.sizes;
    const cacheKey = `${sizesKey}|${probeStyle.font}|${sizes.join(",")}`;
    if (rowHeightCacheRef.current.key !== cacheKey) {
      rowHeightCacheRef.current = { key: cacheKey, entries: new Map() };
      syncMeasurePassesRef.current = 0;
    }
    const cache = rowHeightCacheRef.current.entries;
    const fontSize = Number.parseFloat(probeStyle.fontSize) || 12;
    const lineHeight = Number.parseFloat(probeStyle.lineHeight) || fontSize * 1.333;
    const mounted: Array<{ index: number; row: readonly string[] }> = [];
    for (let pageOffset = 0; pageOffset < pageCount; pageOffset++) {
      const currentPage = page + pageOffset;
      const rows = props.getRows(currentPage);
      if (!rows) continue;
      const start = currentPage * PREVIEW_PAGE_ROWS;
      const limit = visibleEnd - pageOffset * PREVIEW_PAGE_ROWS;
      const count = Math.min(PREVIEW_PAGE_ROWS, props.count - start, limit);
      for (let local = pageOffset === 0 ? from : 0; local < count; local++) {
        const row = rows[local];
        if (row) mounted.push({ index: start + local, row });
      }
    }
    // Rows are measured in order and only until they fill the viewport, like the mounted range.
    const next = new Map<number, number>();
    let measuredRows = 0;
    let covered = 0;
    let target = Number.POSITIVE_INFINITY;
    let needed = mounted.length;
    let scannedChars = 0;
    let hasRemainingUncached = false;
    for (const { index, row } of mounted) {
      if (measuredRows >= needed) break;
      let height = BASE_ROW_HEIGHT;
      if (!isSingleLineRow(row, columns, sizes, fontSize)) {
        const existing = cache.get(index);
        if (existing && (existing.row === row || sameRowCells(existing.row, row))) {
          existing.row = row;
        } else if (next.size > 0 && scannedChars >= MAX_SYNC_SCAN_CHARS) {
          hasRemainingUncached = true;
          break;
        } else {
          const measured = measureRowFullHeight(probe, row, columns, sizes, fontSize, lineHeight, {
            limit: props.source ? MAX_SHOWN_LINE_CHARS : MAX_SHOWN_CELL_CHARS,
            className: props.source ? SOURCE_PROBE_CELL_CLASS_NAME : PROBE_CELL_CLASS_NAME,
            more,
          });
          scannedChars += measured.scannedChars;
          cache.set(index, { row, height: measured.height });
        }
        height = cache.get(index)!.height;
        next.set(index, height);
      }
      measuredRows += 1;
      if (nativeTail || covered >= target) continue;
      if (measuredRows === 1) target = props.viewportHeight + rowFraction * height;
      covered += height;
      // The rows that reach the target plus one spare, the same range the render mounts.
      if (covered >= target) needed = measuredRows + 1;
    }
    if (cache.size > MAX_CACHED_ROW_HEIGHTS) {
      const keep = new Set(mounted.map((entry) => entry.index));
      for (const key of cache.keys()) {
        if (cache.size <= MAX_CACHED_ROW_HEIGHTS) break;
        if (!keep.has(key)) cache.delete(key);
      }
    }
    const publish = () =>
      setRowHeights((old) =>
        old.key === cacheKey && old.fontSize === fontSize && sameRowHeightMap(old.heights, next)
          ? old
          : { key: cacheKey, heights: next, fontSize, sizes },
      );
    rowHeightsSettledRef.current =
      !hasRemainingUncached &&
      rowHeights.key === cacheKey &&
      rowHeights.fontSize === fontSize &&
      sameRowHeightMap(rowHeights.heights, next);
    if (!hasRemainingUncached) {
      syncMeasurePassesRef.current = 0;
      publish();
      return undefined;
    }
    // Each pass published here re-runs this effect synchronously, one nested update under React's
    // limit of 50, so only the first few land before paint and the rest publish a frame apart.
    const synchronous = syncMeasurePassesRef.current < MAX_SYNC_MEASURE_PASSES;
    if (synchronous) {
      syncMeasurePassesRef.current += 1;
      publish();
    }
    const frameId = requestAnimationFrame(() => {
      if (!synchronous) publish();
      setRowMeasureTick((tick) => tick + 1);
    });
    return () => cancelAnimationFrame(frameId);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Measure mounted row offsets and observe content resizes to position the virtual rows against the scroll track.
  useLayoutEffect(() => {
    if (atStart) return;
    const node = contentRef.current;
    if (!node) return;
    const update = () => {
      const row = node.querySelector<HTMLElement>(`[data-preview-row="${first}"]`);
      const height = node.clientHeight;
      const rowHeight = row?.offsetHeight || 29;
      setLayout((old) =>
        old.height === height && old.rowHeight === rowHeight ? old : { height, rowHeight },
      );
    };
    update();
    return observeResize(node, update);
  }, [first, atStart]);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Correct the DOM scroll position once a requested worker row has loaded and its actual bounds are measurable.
  useLayoutEffect(() => {
    const pending = pendingReveal.current;
    const node = ref.current;
    const row =
      pending &&
      contentRef.current?.querySelector<HTMLElement>(`[data-preview-row="${pending.row}"]`);
    if (
      !pending ||
      !node ||
      !row ||
      !props.getRows(Math.floor(pending.row / PREVIEW_PAGE_ROWS))?.[pending.row % PREVIEW_PAGE_ROWS]
    )
      return;
    const content = contentRef.current!;
    const firstRow = content.querySelector<HTMLElement>(`[data-preview-row="${first}"]`);
    if (
      !atStart &&
      (layout.height !== content.clientHeight ||
        layout.rowHeight !== (firstRow?.offsetHeight || 29))
    )
      return;
    const bounds = row.getBoundingClientRect();
    const viewport = node.getBoundingClientRect();
    const delta =
      bounds.top < viewport.top
        ? bounds.top - viewport.top
        : Math.min(bounds.bottom, bounds.top + viewport.height) > viewport.bottom
          ? Math.min(bounds.bottom, bounds.top + viewport.height) - viewport.bottom
          : 0;
    if (Math.abs(delta) < 1 || pending.attempts++ >= 8) {
      pendingReveal.current = null;
      return;
    }
    const step = delta * (nativeTail ? 1 : (scale * 29) / Math.max(29, bounds.height));
    // A compressed track moves many content pixels per scroll pixel, so never round a step to 0.
    node.scrollTop += delta < 0 ? Math.floor(step) : Math.ceil(step);
    setTop(node.scrollTop);
  });
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- Complete End-key scrolling after the final worker row has loaded and its DOM layout has settled.
  useLayoutEffect(() => {
    const node = ref.current;
    const content = contentRef.current;
    if (!pendingEnd.current || !nativeTail || !node || !content || !props.count) return;
    const last = props.count - 1;
    if (!props.getRows(Math.floor(last / PREVIEW_PAGE_ROWS))?.[last % PREVIEW_PAGE_ROWS]) return;
    const row = content.querySelector<HTMLElement>(`[data-preview-row="${last}"]`);
    if (!row || (!atStart && layout.height !== content.clientHeight)) return;
    node.scrollTop = node.scrollHeight;
    setTop(node.scrollTop);
    // Tail rows shorter than the viewport end above its bottom, so End is done once the last shows.
    if (
      rowHeightsSettledRef.current &&
      row.getBoundingClientRect().bottom < node.getBoundingClientRect().bottom + 1
    )
      pendingEnd.current = false;
  });
  return (
    <div
      ref={ref}
      tabIndex={0}
      className="min-h-0 flex-1 overflow-auto"
      onKeyDown={(event) => {
        const node = event.currentTarget;
        const delta =
          event.key === "ArrowDown"
            ? 29
            : event.key === "ArrowUp"
              ? -29
              : event.key === "PageDown"
                ? node.clientHeight
                : event.key === "PageUp"
                  ? -node.clientHeight
                  : null;
        if (delta !== null) {
          pendingEnd.current = false;
          event.preventDefault();
          scrollByContent(node, delta);
        } else if (event.key === "Home" || event.key === "End") {
          event.preventDefault();
          pendingEnd.current = event.key === "End";
          pendingReveal.current = null;
          node.scrollTop = event.key === "Home" ? 0 : node.scrollHeight;
          setTop(node.scrollTop);
        }
      }}
      onScroll={(event) => setTop(event.currentTarget.scrollTop)}
    >
      <div className="relative" style={{ height: scrollHeight }}>
        {measures ? (
          <div
            ref={rowProbeRef}
            aria-hidden="true"
            className={`pointer-events-none invisible absolute top-0 left-0 h-0 overflow-hidden ${props.source ? "right-0 font-mono" : "w-0"}`}
          />
        ) : null}
        <div
          ref={contentRef}
          className="absolute inset-x-0"
          style={{
            top: nativeTail ? maxFirst * scale * 29 : position.top - rowFraction * layout.rowHeight,
          }}
        >
          {Array.from({ length: pageCount }, (_, offset) => (
            <RowPage
              key={page + offset}
              {...props}
              page={page + offset}
              from={offset === 0 ? from : 0}
              rowHeights={props.hasHorizontalOverflow ? knownHeights : undefined}
              ready={ready}
              more={more}
              limit={visibleEnd - offset * PREVIEW_PAGE_ROWS}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

const BASE_ROW_HEIGHT = 29;
const CELL_HORIZONTAL_INSET = 25;
const CELL_VERTICAL_INSET = 13;
const MAX_MEASURED_CELLS_PER_ROW = 16;
// Wider than any common glyph, so estimates never undercount lines.
const MAX_GLYPH_WIDTH_EM = 1.35;
const MAX_CACHED_ROW_HEIGHTS = 512;
const MAX_SYNC_SCAN_CHARS = 128_000;
// Some scripts lay out slowly: 130K characters of Thai took 2.4 s, and of Japanese 2.6 s, in Chromium.
// Mobile's table preview cuts cells at the same length; source lines stop where VS Code's do.
const MAX_SHOWN_CELL_CHARS = 2_000;
const MAX_SHOWN_LINE_CHARS = 10_000;
const MAX_SYNC_MEASURE_PASSES = 4;
const PROBE_CELL_CLASS_NAME =
  "box-border border-r border-b border-transparent px-3 py-1.5 whitespace-pre-wrap break-words tabular-nums";
const SOURCE_PROBE_CELL_CLASS_NAME = "box-border px-3 py-1 whitespace-pre-wrap break-words";

interface CachedRowHeight {
  row: readonly string[];
  height: number;
}

interface RowCellCandidate {
  readonly column: number;
  readonly text: string;
  readonly upperLines: number;
}

/** Conservative greedy word-wrap upper bound on rendered lines without native text shaping. */
function estimateUpperLines(text: string, innerWidth: number, fontSize: number): number {
  const length = text.length;
  let lines = 1;
  let lineWidth = 0;
  let wordWidth = 0;
  for (let index = 0; index < length; index++) {
    const code = text.charCodeAt(index);
    if (code === 10 || code === 13) {
      if (code === 13 && index + 1 < length && text.charCodeAt(index + 1) === 10) index++;
      if (wordWidth > 0) {
        if (lineWidth > 0 && lineWidth + wordWidth > innerWidth) lines++;
        wordWidth = 0;
      }
      lines++;
      lineWidth = 0;
      continue;
    }
    if (code === 32 || code === 9) {
      const spaceWidth = (code === 9 ? 4.5 : 0.38) * fontSize;
      if (wordWidth > 0) {
        if (lineWidth > 0 && lineWidth + wordWidth > innerWidth) {
          lines++;
          lineWidth = wordWidth;
        } else {
          lineWidth += wordWidth;
        }
        wordWidth = 0;
      }
      lineWidth += spaceWidth;
      if (lineWidth > innerWidth) {
        lines++;
        lineWidth = 0;
      }
      continue;
    }
    const charWidth = MAX_GLYPH_WIDTH_EM * fontSize;
    wordWidth += charWidth;
    if (wordWidth > innerWidth) {
      if (lineWidth > 0) {
        lines++;
        lineWidth = 0;
      }
      lines++;
      wordWidth = charWidth;
    }
  }
  if (wordWidth > 0 && lineWidth > 0 && lineWidth + wordWidth > innerWidth) lines++;
  return lines;
}

function collectRowHeightCandidates(
  row: readonly string[],
  columns: number,
  sizes: readonly number[],
  fontSize: number,
  shown: (text: string) => string,
): { candidates: RowCellCandidate[]; scannedChars: number } {
  const candidates: RowCellCandidate[] = [];
  const limit = Math.min(columns, row.length);
  let scannedChars = 0;
  for (let column = 0; column < limit; column++) {
    const cell = row[column];
    if (!cell) continue;
    const width = sizes[column];
    if (!width || fitsOneLine(cell, width, fontSize)) continue;
    const text = shown(cell);
    const innerWidth = Math.max(1, width - CELL_HORIZONTAL_INSET);
    const hasControlWhitespace = text.includes("\n") || text.includes("\r") || text.includes("\t");
    scannedChars += text.length;
    const upperLines = estimateUpperLines(text, innerWidth, fontSize);
    if (!hasControlWhitespace && upperLines <= 1) continue;
    candidates.push({ column, text, upperLines });
  }
  if (candidates.length > MAX_MEASURED_CELLS_PER_ROW) {
    candidates.sort((left, right) => right.upperLines - left.upperLines);
  }
  return { candidates, scannedChars };
}

function measureProbeEntries(
  probe: HTMLElement,
  entries: ReadonlyArray<{ index: number; column: number; text: string }>,
  sizes: readonly number[],
  className: string,
  results: Map<number, CachedRowHeight>,
): void {
  if (entries.length === 0) return;
  const fragment = document.createDocumentFragment();
  const nodes: Array<{ index: number; node: HTMLDivElement }> = [];
  for (const entry of entries) {
    const node = document.createElement("div");
    node.className = className;
    node.style.width = `${sizes[entry.column]}px`;
    node.textContent = entry.text;
    fragment.append(node);
    nodes.push({ index: entry.index, node });
  }
  probe.replaceChildren(fragment);
  for (const { index, node } of nodes) {
    const height = node.getBoundingClientRect().height;
    const current = results.get(index);
    if (current && height > current.height) current.height = height;
  }
  probe.replaceChildren();
}

function measureRowFullHeight(
  probe: HTMLElement,
  row: readonly string[],
  columns: number,
  sizes: readonly number[],
  fontSize: number,
  lineHeight: number,
  options: { limit: number; className: string; more: (count: number) => string },
): { height: number; scannedChars: number } {
  const { candidates, scannedChars } = collectRowHeightCandidates(
    row,
    columns,
    sizes,
    fontSize,
    (text) => shownText(text, options.limit, options.more),
  );
  const result = { row, height: BASE_ROW_HEIGHT };
  const measured = new Map([[0, result]]);
  measureProbeEntries(
    probe,
    candidates
      .slice(0, MAX_MEASURED_CELLS_PER_ROW)
      .map((candidate) => ({ index: 0, column: candidate.column, text: candidate.text })),
    sizes,
    options.className,
    measured,
  );
  if (candidates.length > MAX_MEASURED_CELLS_PER_ROW) {
    const measuredLines = Math.max(
      1,
      Math.floor((result.height - CELL_VERTICAL_INSET + 0.5) / lineHeight),
    );
    const rest: Array<{ index: number; column: number; text: string }> = [];
    for (const candidate of candidates.slice(MAX_MEASURED_CELLS_PER_ROW)) {
      if (candidate.upperLines <= measuredLines) break;
      rest.push({ index: 0, column: candidate.column, text: candidate.text });
    }
    measureProbeEntries(probe, rest, sizes, options.className, measured);
  }
  return { height: result.height, scannedChars };
}

/** The length of a cell's text the preview lays out, never splitting a surrogate pair. */
function shownLength(text: string, limit: number): number {
  if (text.length <= limit) return text.length;
  const code = text.charCodeAt(limit - 1);
  return code >= 0xd800 && code <= 0xdbff ? limit - 1 : limit;
}

function cutSuffix(count: number, more: (count: number) => string): string {
  return `… ${more(count)}`;
}

/** The text a cell lays out: at most `limit` characters, then how many were left out. */
function shownText(text: string, limit: number, more: (count: number) => string): string {
  const end = shownLength(text, limit);
  return end === text.length ? text : text.slice(0, end) + cutSuffix(text.length - end, more);
}

/** Whether every cell surely fits one line, so the row is 29px without a probe layout. */
function isSingleLineRow(
  row: readonly string[],
  columns: number,
  sizes: readonly number[],
  fontSize: number,
): boolean {
  const limit = Math.min(columns, row.length);
  for (let column = 0; column < limit; column++) {
    const text = row[column];
    const width = sizes[column];
    if (text && width && !fitsOneLine(text, width, fontSize)) return false;
  }
  return true;
}

function fitsOneLine(text: string, width: number, fontSize: number): boolean {
  return (
    text.length * fontSize * MAX_GLYPH_WIDTH_EM <= Math.max(1, width - CELL_HORIZONTAL_INSET) &&
    !text.includes("\n") &&
    !text.includes("\r") &&
    !text.includes("\t")
  );
}

function sameRowCells(left: readonly string[], right: readonly string[]): boolean {
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index++) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

function sameRowHeightMap(
  left: ReadonlyMap<number, number>,
  right: ReadonlyMap<number, number>,
): boolean {
  if (left.size !== right.size) return false;
  for (const [index, height] of left) {
    if (right.get(index) !== height) return false;
  }
  return true;
}

function columnLabel(index: number): string {
  let label = "";
  for (let value = index + 1; value > 0; value = Math.floor((value - 1) / 26))
    label = String.fromCharCode(65 + ((value - 1) % 26)) + label;
  return label;
}
