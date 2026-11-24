import { splitsSurrogatePair } from "@onicode/shared/assistantCitations";

import type { WorkflowDefinition } from "./workflow.ts";

// Kept apart from workflow.ts so the runtime can name runs without loading `effect/workflow`.

const DETAIL_KEPT = 2_000;
export const detailCut = (text: string) =>
  text.length <= DETAIL_KEPT
    ? text
    : text.slice(0, splitsSurrogatePair(text, DETAIL_KEPT) ? DETAIL_KEPT - 1 : DETAIL_KEPT);

/** An error as one line for a person, with what caused it when the error only wraps something. */
export const text = (error: unknown): string => {
  if (typeof error === "string") return detailCut(error);
  if (typeof error !== "object" || error === null) return String(error);
  // Also an error that came back from the journal, which is its fields and no longer an Error.
  const fields = error as {
    readonly message?: unknown;
    readonly reason?: unknown;
    cause?: unknown;
  };
  const message =
    typeof fields.message === "string" && fields.message !== ""
      ? fields.message
      : typeof fields.reason === "string"
        ? fields.reason
        : "Unknown error";
  return detailCut(fields.cause === undefined ? message : `${message}: ${text(fields.cause)}`);
};

export const engineTag = (definition: Pick<WorkflowDefinition, "name" | "version">) =>
  `${definition.name}@${definition.version}`;
