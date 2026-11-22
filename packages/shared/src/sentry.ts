import * as SentryCore from "@sentry/core";
import * as Cause from "effect/Cause";
import * as Context from "effect/Context";
import * as Exit from "effect/Exit";
import * as Logger from "effect/Logger";
import * as Option from "effect/Option";
import * as Tracer from "effect/Tracer";

/** Collects nothing about the user or request payloads; reports keep only code-level context. */
export const privateDataCollection = {
  userInfo: false,
  cookies: false,
  httpHeaders: false,
  httpBodies: [],
  urlQueryParams: false,
  graphQL: { document: false, variables: false },
  genAI: { inputs: false, outputs: false },
  mcp: { inputs: false, outputs: false },
  databaseQueryData: false,
  queues: false,
  stackFrameVariables: false,
} satisfies SentryCore.DataCollection;

/** Share of traces sent to Sentry. Errors are always sent. */
export const TRACES_SAMPLE_RATE = 0.05;

/**
 * A child of a span open longer than this starts its own Sentry trace. Long-lived parents (thread
 * subscriptions, reactor loops) would otherwise make one sampling decision for hours of work.
 */
export const SEGMENT_MAX_PARENT_AGE_MS = 30_000;

type SentryApi = Pick<typeof SentryCore, "startInactiveSpan" | "startNewTrace">;

interface OpenSentrySpan {
  readonly span: SentryCore.Span;
  readonly startedAtMs: number;
  /** Set when the span ends while children that share it are still open. */
  ended: boolean;
}

export interface SentrySpanBridgeOptions {
  /** Micro-spans too frequent to be worth sending; their children attach to the nearest sent span. */
  readonly ignoredSpanNames?: ReadonlySet<string>;
  readonly now?: () => number;
  readonly api?: SentryApi;
}

export interface SentrySpanBridge {
  readonly tracer: Tracer.Tracer;
  /** The Sentry span recording an Effect span, so errors captured inside it join its trace. */
  readonly sentrySpanFor: (effectSpanId: string) => SentryCore.Span | undefined;
}

/**
 * Wraps a tracer so its spans are also recorded as Sentry spans. Effect keeps its own ids and
 * sampling, so the local trace file is unaffected by Sentry's sampling.
 */
export function makeSentrySpanBridge(
  inner: Tracer.Tracer,
  options: SentrySpanBridgeOptions = {},
): SentrySpanBridge {
  const api = options.api ?? SentryCore;
  const now = options.now ?? Date.now;
  const ignored = options.ignoredSpanNames ?? new Set<string>();
  const open = new Map<string, OpenSentrySpan>();

  const startSegment = (name: string, effectTraceId: string): OpenSentrySpan => ({
    span: api.startNewTrace(() =>
      api.startInactiveSpan({
        name,
        attributes: { "onicode.effect.trace_id": effectTraceId },
      }),
    ),
    startedAtMs: now(),
    ended: false,
  });

  const tracer = Tracer.make({
    span(spanOptions) {
      const span = inner.span(spanOptions);
      const parentId = Option.isSome(spanOptions.parent)
        ? spanOptions.parent.value.spanId
        : undefined;
      const parent = parentId === undefined ? undefined : open.get(parentId);
      const parentIsCurrent =
        parent !== undefined &&
        !parent.ended &&
        now() - parent.startedAtMs <= SEGMENT_MAX_PARENT_AGE_MS;

      let entry: OpenSentrySpan;
      if (parentIsCurrent && (!span.sampled || ignored.has(span.name))) {
        entry = parent;
      } else if (parentIsCurrent && !parent.span.isRecording()) {
        // Sampled out: share the parent's non-recording span so the whole segment stays out.
        entry = parent;
      } else if (parentIsCurrent) {
        entry = {
          span: api.startInactiveSpan({ name: span.name, parentSpan: parent.span }),
          startedAtMs: now(),
          ended: false,
        };
      } else if (!span.sampled || ignored.has(span.name)) {
        return span;
      } else {
        entry = startSegment(span.name, span.traceId);
      }

      open.set(span.spanId, entry);
      const owned = entry !== parent;
      return new BridgedSpan(span, owned ? entry.span : undefined, () => {
        open.delete(span.spanId);
        if (owned) entry.ended = true;
      });
    },
    ...(inner.context ? { context: inner.context } : {}),
  });

  return { tracer, sentrySpanFor: (effectSpanId) => open.get(effectSpanId)?.span };
}

export interface SentryEffectInstrumentation {
  /** Call once with the tracer the process would otherwise use. */
  readonly wrapTracer: (tracer: Tracer.Tracer) => Tracer.Tracer;
  readonly logger: Logger.Logger<unknown, void>;
}

/** Effect's tracer and logger hooks for a process that has started Sentry. */
export function makeSentryEffectInstrumentation(
  options: SentrySpanBridgeOptions = {},
): SentryEffectInstrumentation {
  let bridge: SentrySpanBridge | undefined;
  return {
    wrapTracer: (tracer) => {
      bridge = makeSentrySpanBridge(tracer, options);
      return bridge.tracer;
    },
    logger: makeSentryLogger({ sentrySpanFor: (spanId) => bridge?.sentrySpanFor(spanId) }),
  };
}

class BridgedSpan implements Tracer.Span {
  readonly _tag = "Span";
  private readonly inner: Tracer.Span;
  private readonly sentrySpan: SentryCore.Span | undefined;
  private readonly release: () => void;
  constructor(inner: Tracer.Span, sentrySpan: SentryCore.Span | undefined, release: () => void) {
    this.inner = inner;
    this.sentrySpan = sentrySpan;
    this.release = release;
  }
  get name() {
    return this.inner.name;
  }
  get spanId() {
    return this.inner.spanId;
  }
  get traceId() {
    return this.inner.traceId;
  }
  get parent() {
    return this.inner.parent;
  }
  get annotations() {
    return this.inner.annotations;
  }
  get status() {
    return this.inner.status;
  }
  get attributes() {
    return this.inner.attributes;
  }
  get links() {
    return this.inner.links;
  }
  get sampled() {
    return this.inner.sampled;
  }
  get kind() {
    return this.inner.kind;
  }
  end(endTime: bigint, exit: Exit.Exit<unknown, unknown>): void {
    this.release();
    this.inner.end(endTime, exit);
    const sentrySpan = this.sentrySpan;
    if (sentrySpan === undefined || !sentrySpan.isRecording()) return;
    sentrySpan.setStatus(
      Exit.isFailure(exit)
        ? { code: SentryCore.SPAN_STATUS_ERROR, message: failureStatus(exit.cause) }
        : { code: SentryCore.SPAN_STATUS_OK },
    );
    sentrySpan.end();
  }
  attribute(key: string, value: unknown): void {
    this.inner.attribute(key, value);
    // Strings can carry paths, commands and user content; counts and flags cannot.
    if (
      this.sentrySpan?.isRecording() &&
      (typeof value === "number" || typeof value === "boolean")
    ) {
      this.sentrySpan.setAttribute(key, value);
    }
  }
  event(name: string, startTime: bigint, attributes?: Record<string, unknown>): void {
    this.inner.event(name, startTime, attributes);
  }
  addLinks(links: ReadonlyArray<Tracer.SpanLink>): void {
    this.inner.addLinks(links);
  }
}

// Error messages can carry user content; the status keeps only the failure's kind.
function failureStatus(cause: Cause.Cause<unknown>): string {
  if (Cause.hasInterruptsOnly(cause)) return "cancelled";
  for (const reason of cause.reasons) {
    const error =
      reason._tag === "Fail" ? reason.error : reason._tag === "Die" ? reason.defect : undefined;
    if (
      typeof error === "object" &&
      error !== null &&
      "_tag" in error &&
      typeof error._tag === "string"
    ) {
      return error._tag;
    }
  }
  return "internal_error";
}

const breadcrumbLevels: Partial<Record<string, SentryCore.SeverityLevel>> = {
  Info: "info",
  Warn: "warning",
};

/**
 * Sends Error and Fatal logs to Sentry, preferring the defect or Error in their cause, and keeps
 * Info and Warn logs as breadcrumbs for the next error. Only the log's text parts are sent: the
 * structured data beside them holds command output, paths and user content.
 */
function makeSentryLogger(bridge: Pick<SentrySpanBridge, "sentrySpanFor">) {
  return Logger.make<unknown, void>(({ cause, fiber, logLevel, message }) => {
    const text = logMessageText(message);
    const breadcrumbLevel = breadcrumbLevels[logLevel];
    if (breadcrumbLevel !== undefined) {
      SentryCore.addBreadcrumb({ category: "log", level: breadcrumbLevel, message: text });
      return;
    }
    if (logLevel !== "Error" && logLevel !== "Fatal") return;

    const effectSpan = Context.getOption(fiber.context, Tracer.ParentSpan);
    const sentrySpan = Option.isSome(effectSpan)
      ? bridge.sentrySpanFor(effectSpan.value.spanId)
      : undefined;
    const level: SentryCore.SeverityLevel = logLevel === "Fatal" ? "fatal" : "error";
    const capture = () =>
      SentryCore.withScope((scope) => {
        scope.setLevel(level);
        const errors = [...causeErrors(cause), ...nestedErrors(message)];
        if (errors.length === 0) {
          // No stack to group by: one issue per log message.
          scope.setFingerprint([text]);
          SentryCore.captureMessage(text, level);
          return;
        }
        scope.setExtra("log_message", text);
        for (const error of errors) {
          SentryCore.captureException(error, {
            // A logged error was handled; only a defect nobody caught counts as a crash.
            mechanism: { type: "auto.log.effect", handled: !isUnhandledDefect(text) },
          });
        }
      });
    if (sentrySpan === undefined) capture();
    else SentryCore.withActiveSpan(sentrySpan, capture);
  });
}

function causeErrors(cause: Cause.Cause<unknown>): ReadonlyArray<Error> {
  const errors: Array<Error> = [];
  for (const reason of cause.reasons) {
    if (reason._tag === "Die") errors.push(defectError(reason.defect));
    else if (reason._tag === "Fail" && reason.error instanceof Error) errors.push(reason.error);
  }
  return errors;
}

/**
 * Sentry serializes the fields of a thrown non-Error into the report, and those can hold paths
 * or user content; such a defect is reported by its kind alone.
 */
function defectError(defect: unknown): Error {
  if (defect instanceof Error) return defect;
  const error = new Error(`Defect that is not an Error: ${valueKind(defect)}`);
  error.name = "NonErrorDefect";
  // The logger's own frames would group every such defect into one issue.
  error.stack = `${error.name}: ${error.message}`;
  return error;
}

function valueKind(value: unknown): string {
  if (typeof value !== "object" || value === null) return value === null ? "null" : typeof value;
  if ("_tag" in value && typeof value._tag === "string") return value._tag;
  return Array.isArray(value) ? "array" : "object";
}

// Logs pass their failure beside the message, on its own or as `{ cause }` or `{ error }`.
function nestedErrors(message: unknown): ReadonlyArray<Error> {
  const errors: Array<Error> = [];
  for (const part of Array.isArray(message) ? message : [message]) {
    if (part instanceof Error) {
      errors.push(part);
      continue;
    }
    if (typeof part !== "object" || part === null) continue;
    for (const key of ["cause", "error"] as const) {
      const value: unknown = (part as Record<string, unknown>)[key];
      if (value instanceof Error) errors.push(value);
      else if (Cause.isCause(value)) errors.push(...causeErrors(value));
    }
  }
  return errors;
}

// DefectReporter's message for a defect that reached the top of a fiber.
const isUnhandledDefect = (text: string) => text === "Unhandled defect";

function logMessageText(message: unknown): string {
  const parts = Array.isArray(message) ? message : [message];
  return parts.filter((part) => typeof part === "string").join(" ");
}

/** `production` for stable release builds, `development` otherwise. */
export const sentryEnvironment = (releaseChannel: string): string =>
  releaseChannel === "stable" ? "production" : "development";
