import { describe, expect, it } from "@effect/vitest";
import * as SentryCore from "@sentry/core";
import * as Cause from "effect/Cause";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Logger from "effect/Logger";
import * as Option from "effect/Option";
import * as Tracer from "effect/Tracer";

import {
  makeSentryEffectInstrumentation,
  makeSentrySpanBridge,
  SEGMENT_MAX_PARENT_AGE_MS,
} from "./sentry.ts";
import { scrubSentryPayload } from "./sentryScrub.ts";

interface FakeSpan {
  readonly name: string;
  readonly parent: FakeSpan | undefined;
  readonly newTrace: boolean;
  ended: boolean;
}

function makeFakeSentry(options: { readonly sampleRoots: boolean }) {
  const started: Array<FakeSpan> = [];
  let inNewTrace = false;
  const toSentry = (span: FakeSpan) =>
    ({
      isRecording: () => !span.ended && (options.sampleRoots || !span.newTrace),
      setStatus: () => {},
      setAttribute: () => {},
      end: () => {
        span.ended = true;
      },
      fake: span,
    }) as unknown as SentryCore.Span & { readonly fake: FakeSpan };
  const api = {
    startNewTrace: <T>(callback: () => T): T => {
      inNewTrace = true;
      try {
        return callback();
      } finally {
        inNewTrace = false;
      }
    },
    startInactiveSpan: (spanOptions: SentryCore.StartSpanOptions) => {
      const parent = spanOptions.parentSpan as (SentryCore.Span & { fake: FakeSpan }) | undefined;
      const span: FakeSpan = {
        name: spanOptions.name,
        parent: parent?.fake,
        newTrace: inNewTrace,
        ended: false,
      };
      started.push(span);
      return toSentry(span);
    },
  } as unknown as Pick<typeof SentryCore, "startInactiveSpan" | "startNewTrace">;
  return { api, started };
}

const nativeTracer = Tracer.make({ span: (options) => new Tracer.NativeSpan(options) });

describe("makeSentrySpanBridge", () => {
  it.effect("nests children under their parent and skips ignored spans", () =>
    Effect.gen(function* () {
      const fake = makeFakeSentry({ sampleRoots: true });
      const bridge = makeSentrySpanBridge(nativeTracer, {
        api: fake.api,
        now: () => 0,
        ignoredSpanNames: new Set(["noise"]),
      });
      yield* Effect.void.pipe(
        Effect.withSpan("leaf"),
        Effect.withSpan("noise"),
        Effect.withSpan("root"),
        Effect.withTracer(bridge.tracer),
      );
      expect(fake.started.map((span) => [span.name, span.parent?.name, span.newTrace])).toEqual([
        ["root", undefined, true],
        ["leaf", "root", false],
      ]);
      expect(fake.started.every((span) => span.ended)).toBe(true);
    }),
  );

  it.effect("starts a new trace under a long-lived parent", () =>
    Effect.gen(function* () {
      const fake = makeFakeSentry({ sampleRoots: true });
      let now = 0;
      const bridge = makeSentrySpanBridge(nativeTracer, { api: fake.api, now: () => now });
      yield* Effect.sync(() => {
        now = SEGMENT_MAX_PARENT_AGE_MS + 1;
      }).pipe(
        Effect.andThen(Effect.void.pipe(Effect.withSpan("late-child"))),
        Effect.withSpan("subscription"),
        Effect.withTracer(bridge.tracer),
      );
      expect(fake.started.map((span) => [span.name, span.parent?.name, span.newTrace])).toEqual([
        ["subscription", undefined, true],
        ["late-child", undefined, true],
      ]);
    }),
  );

  it("starts a new trace below a span whose recorded parent already ended", () => {
    const fake = makeFakeSentry({ sampleRoots: true });
    const bridge = makeSentrySpanBridge(nativeTracer, {
      api: fake.api,
      now: () => 0,
      ignoredSpanNames: new Set(["forked"]),
    });
    const start = (name: string, parent?: Tracer.AnySpan) =>
      bridge.tracer.span({
        name,
        parent: Option.fromNullishOr(parent),
        annotations: Context.empty(),
        links: [],
        startTime: 0n,
        kind: "internal",
        root: parent === undefined,
        sampled: true,
      });
    const root = start("root");
    const forked = start("forked", root);
    root.end(0n, Exit.void);
    start("late", forked);
    expect(fake.started.map((span) => [span.name, span.parent?.name, span.newTrace])).toEqual([
      ["root", undefined, true],
      ["late", undefined, true],
    ]);
  });

  it.effect("records nothing below a root Sentry sampled out", () =>
    Effect.gen(function* () {
      const fake = makeFakeSentry({ sampleRoots: false });
      const bridge = makeSentrySpanBridge(nativeTracer, { api: fake.api, now: () => 0 });
      yield* Effect.void.pipe(
        Effect.withSpan("grandchild"),
        Effect.withSpan("child"),
        Effect.withSpan("root"),
        Effect.withTracer(bridge.tracer),
      );
      expect(fake.started.map((span) => span.name)).toEqual(["root"]);
    }),
  );
});

describe("makeSentryEffectInstrumentation logger", () => {
  it.effect("sends only Error instances, reporting other defects by their kind", () =>
    Effect.gen(function* () {
      const captured: Array<{ readonly exception: unknown; readonly handled: unknown }> = [];
      const scope = SentryCore.getCurrentScope();
      const previous = scope.getClient();
      scope.setClient({
        captureException: (exception: unknown, hint: SentryCore.EventHint) =>
          captured.push({ exception, handled: hint.mechanism?.handled }),
        captureMessage: () => {},
        getOptions: () => ({}),
      } as unknown as SentryCore.Client);
      const thrown = new Error("boom");
      yield* Effect.gen(function* () {
        yield* Effect.logError("Unhandled defect", { cause: Cause.die({ path: "/Users/ada" }) });
        yield* Effect.logError("Request failed", thrown);
      }).pipe(
        Effect.provide(Logger.layer([makeSentryEffectInstrumentation().logger])),
        Effect.ensuring(Effect.sync(() => scope.setClient(previous))),
      );
      expect(
        captured.map(({ exception, handled }) => {
          const error = exception as Error;
          return [error === thrown ? "thrown" : `${error.name}: ${error.message}`, handled];
        }),
      ).toEqual([
        ["NonErrorDefect: Defect that is not an Error: object", false],
        ["thrown", true],
      ]);
    }),
  );
});

describe("scrubSentryPayload", () => {
  it("replaces the home directory in every nested string", () => {
    expect(
      scrubSentryPayload(
        { message: "ENOENT /Users/ada/project", frames: [{ filename: "/Users/ada/a.ts" }] },
        "/Users/ada",
      ),
    ).toEqual({ message: "ENOENT ~/project", frames: [{ filename: "~/a.ts" }] });
    expect(scrubSentryPayload({ path: "C:\\Users\\ada\\x" }, "C:\\Users\\ada")).toEqual({
      path: "~\\x",
    });
  });

  it("drops URL queries and fragments, which can carry tickets", () => {
    expect(
      scrubSentryPayload(
        {
          url: "wss://box.tailnet.ts.net:3773/ws?wsTicket=secret&x=1",
          message: 'fetch "https://api.example.com/v1/a#token=abc" failed',
          page: "onicode://app/#/new?prompt=secret",
          data: { from: "/#/threads/1", to: "/#/new?prompt=secret" },
          plain: "a?b",
        },
        "",
      ),
    ).toEqual({
      url: "wss://box.tailnet.ts.net:3773/ws",
      message: 'fetch "https://api.example.com/v1/a" failed',
      page: "onicode://app/",
      data: { from: "/#/threads/1", to: "/#/new" },
      plain: "a?b",
    });
  });
});
