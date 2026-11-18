let hoveredRow: Element | null = null;

/** Records the row the pointer just highlighted, before the highlight renders. */
export function noteHoveredRow(row: Element | null): void {
  hoveredRow = row;
}

/**
 * Scrolls a list's highlighted row into view, unless the pointer put the highlight there. That row
 * is already under the pointer, and scrolling it slides the list along with a moving mouse.
 */
export function revealHighlightedRow(row: Element | null): void {
  if (row === null) return;
  if (hoveredRow !== null && (row.contains(hoveredRow) || hoveredRow.contains(row))) return;
  hoveredRow = null;
  row.scrollIntoView({ block: "nearest" });
}
