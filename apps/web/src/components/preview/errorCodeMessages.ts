import { msg } from "gt-react";

import { m, type MessageResolver } from "~/i18n/messages";
import { PREVIEW_ERROR_CODE_MESSAGES, PREVIEW_TUNNEL_FAILURE_MESSAGE } from "./previewConstants";

// The desktop's tunnel proxy refuses a CONNECT with 502, and closes a plain http request unanswered.
const TUNNEL_FAILURE_CODES: ReadonlySet<string> = new Set([
  "ERR_TUNNEL_CONNECTION_FAILED",
  "ERR_EMPTY_RESPONSE",
]);

/** Components pass `useMessages()` so the message follows a language switch. */
export function describePreviewError(
  description: string,
  resolve: MessageResolver = m,
  tunnelled = false,
): string {
  if (tunnelled && TUNNEL_FAILURE_CODES.has(description)) {
    return resolve(PREVIEW_TUNNEL_FAILURE_MESSAGE);
  }
  const friendly = PREVIEW_ERROR_CODE_MESSAGES[description];
  if (friendly) return resolve(friendly);
  if (description.length > 0) return description;
  return resolve(msg("Network error"));
}
