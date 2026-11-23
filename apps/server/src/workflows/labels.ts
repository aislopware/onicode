import type { WorkflowDefinition } from "./workflow.ts";

// Kept apart from workflow.ts so the runtime can name runs without loading `effect/workflow`.

/** An error as one line for a person, with what caused it when the error only wraps something. */
export const text = (error: unknown): string => {
  if (typeof error === "string") return error.slice(0, 2000);
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
  return (fields.cause === undefined ? message : `${message}: ${text(fields.cause)}`).slice(
    0,
    2000,
  );
};

export const engineTag = (definition: Pick<WorkflowDefinition, "name" | "version">) =>
  `${definition.name}@${definition.version}`;
