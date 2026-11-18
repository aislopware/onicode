import type { TerminalSummary } from "@onicode/contracts";
import { mFallback, msg } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";

import { getTerminalLabel as getServerTerminalLabel, terminalNumber } from "./terminalLabels.ts";

/** @public */
export function getTerminalLabel(terminalId: string, resolve: MFunctionType = mFallback): string {
  const numericSuffix = terminalNumber(terminalId);
  return numericSuffix ? resolve(msg("Terminal {number}", { number: numericSuffix })) : terminalId;
}

/**
 * The server sends the running process name, or its English default, which is shown translated.
 * The default also arrives while a process runs whose name the server has not read yet.
 * @public
 */
export function resolveTerminalSessionLabel(
  terminalId: string,
  summary: Pick<TerminalSummary, "label"> | null | undefined,
  resolve: MFunctionType = mFallback,
): string {
  const trimmed = summary?.label.trim();
  return trimmed && trimmed !== getServerTerminalLabel(terminalId)
    ? trimmed
    : getTerminalLabel(terminalId, resolve);
}
