import type { NativeLayoutMetrics } from "../../lib/reserved-regions";

/**
 * Whether the bar's trailing tools share one "More actions" menu. Only the Duo's vertical bar
 * needs it: it never overflows, so tools that don't fit slide under the workspace tabs. A
 * regular-width sidebar draws a horizontal bar beside the detail column's vertical one.
 */
export function groupsAppBarTrailingTools(input: {
  readonly nativeColumns: boolean;
  readonly metrics: NativeLayoutMetrics | null;
  readonly sidebar: boolean;
  readonly closable: boolean;
  readonly tools: number;
}): boolean {
  return (
    input.nativeColumns &&
    (input.metrics?.verticalBarEdge ?? "none") !== "none" &&
    !input.sidebar &&
    !input.closable &&
    input.tools > 1
  );
}
