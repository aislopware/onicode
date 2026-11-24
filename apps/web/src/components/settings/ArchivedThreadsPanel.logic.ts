export interface ArchivedChunkView {
  readonly near: boolean;
  readonly rowHeight: number;
}

/** Measures from the rows a chunk has mounted, and keeps the same view when nothing changed. */
export function nextArchivedChunkView(
  previous: ArchivedChunkView,
  near: boolean,
  height: number,
  mountedRows: number,
): ArchivedChunkView {
  const rowHeight = mountedRows > 0 && height > 0 ? height / mountedRows : previous.rowHeight;
  if (previous.near === near && previous.rowHeight === rowHeight) return previous;
  return { near, rowHeight };
}
